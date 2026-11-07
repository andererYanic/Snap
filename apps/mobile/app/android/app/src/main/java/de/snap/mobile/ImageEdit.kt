package de.snap.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.ColorMatrix
import android.graphics.ColorMatrixColorFilter
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.graphics.Paint
import android.media.ExifInterface
import android.os.Build
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest

/**
 * Bildbearbeitung über Android selbst: Drehen, Zuschneiden, Helligkeit/Kontrast/Sättigung/Wärme
 * und Filter (Farbmatrix). Vorschauen werden verkleinert und zwischengespeichert; gespeichert wird
 * in voller Größe (höchstens [MAX_PIXELS]), als Kopie neben dem Original oder als Temp-Datei, die
 * der Kern dann gegen das Original tauscht (Original in den Papierkorb, rückgängig machbar).
 */
object ImageEdit {
  private const val MAX_PIXELS = 24_000_000L
  private val lock = Mutex()

  data class Params(
    val rot: Int,
    val crop: FloatArray?, // x, y, w, h normalisiert (0..1), bezogen auf das gedrehte Bild
    val light: Float,
    val contrast: Float,
    val sat: Float,
    val warm: Float,
    val filter: String,
  ) {
    fun key(): String = "$rot|${crop?.joinToString(",")}|$light|$contrast|$sat|$warm|$filter"
  }

  fun parse(json: String): Params {
    val o = JSONObject(json)
    val c = o.optJSONObject("crop")
    fun f(name: String) = o.optDouble(name, 0.0).toFloat().coerceIn(-1f, 1f)
    val crop = c?.let {
      val x = it.optDouble("x", 0.0).toFloat().coerceIn(0f, 1f)
      val y = it.optDouble("y", 0.0).toFloat().coerceIn(0f, 1f)
      val w = it.optDouble("w", 1.0).toFloat().coerceIn(0.01f, 1f - x)
      val h = it.optDouble("h", 1.0).toFloat().coerceIn(0.01f, 1f - y)
      floatArrayOf(x, y, w, h)
    }
    val rot = ((o.optInt("rot", 0) % 360) + 360) % 360 / 90 * 90
    val filter = o.optString("filter", "").takeIf { it in FILTERS } ?: ""
    return Params(rot, crop, f("light"), f("contrast"), f("sat"), f("warm"), filter)
  }

  private val FILTERS = setOf("", "klar", "warm", "kuehl", "sw", "matt")

  /** Bild laden (EXIF-Ausrichtung angewendet), höchstens `maxPixels` Pixel bzw. `maxEdge` Kante. */
  private fun decode(file: File, maxPixels: Long, maxEdge: Int = Int.MAX_VALUE): Bitmap {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      val src = ImageDecoder.createSource(file)
      return ImageDecoder.decodeBitmap(src) { dec, info, _ ->
        dec.allocator = ImageDecoder.ALLOCATOR_SOFTWARE // zum Zeichnen auf Canvas nötig
        dec.isMutableRequired = false
        val w = info.size.width.toLong()
        val h = info.size.height.toLong()
        var s = 1.0
        if (w * h > maxPixels) s = Math.sqrt(maxPixels.toDouble() / (w * h))
        val edge = maxOf(w, h) * s
        if (edge > maxEdge) s *= maxEdge / edge
        if (s < 1.0) dec.setTargetSize(maxOf(1, (w * s).toInt()), maxOf(1, (h * s).toInt()))
      }
    }
    // Älteres Android: verkleinert dekodieren, Ausrichtung aus EXIF selbst anwenden
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.absolutePath, bounds)
    var sample = 1
    while ((bounds.outWidth.toLong() / sample) * (bounds.outHeight / sample) > maxPixels ||
      maxOf(bounds.outWidth, bounds.outHeight) / sample > maxEdge * 2) sample *= 2
    val bmp = BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
      ?: throw IllegalArgumentException("Bild nicht lesbar.")
    val deg = when (ExifInterface(file.absolutePath).getAttributeInt(ExifInterface.TAG_ORIENTATION, 1)) {
      ExifInterface.ORIENTATION_ROTATE_90 -> 90
      ExifInterface.ORIENTATION_ROTATE_180 -> 180
      ExifInterface.ORIENTATION_ROTATE_270 -> 270
      else -> 0
    }
    if (deg == 0) return bmp
    return Bitmap.createBitmap(bmp, 0, 0, bmp.width, bmp.height, Matrix().apply { postRotate(deg.toFloat()) }, true)
  }

  private fun colorMatrix(p: Params): ColorMatrix {
    val m = ColorMatrix()
    var light = p.light
    var contrast = p.contrast
    var sat = p.sat
    var warm = p.warm
    when (p.filter) {
      "klar" -> { contrast += 0.12f; sat += 0.15f }
      "warm" -> { warm += 0.45f; sat += 0.1f }
      "kuehl" -> { warm -= 0.45f }
      "sw" -> { sat = -1f; contrast += 0.1f }
      "matt" -> { contrast -= 0.15f; light += 0.05f; sat -= 0.15f }
    }
    m.postConcat(ColorMatrix().apply { setSaturation((1f + sat).coerceAtLeast(0f)) })
    val c = (1f + contrast).coerceAtLeast(0.05f)
    val t = (1f - c) * 128f + light * 90f
    m.postConcat(ColorMatrix(floatArrayOf(
      c, 0f, 0f, 0f, t + warm * 22f,
      0f, c, 0f, 0f, t,
      0f, 0f, c, 0f, t - warm * 22f,
      0f, 0f, 0f, 1f, 0f,
    )))
    return m
  }

  /** Drehen, (optional) zuschneiden, Farben anwenden. */
  private fun process(src: Bitmap, p: Params, withCrop: Boolean): Bitmap {
    var bmp = src
    if (p.rot != 0) {
      bmp = Bitmap.createBitmap(bmp, 0, 0, bmp.width, bmp.height, Matrix().apply { postRotate(p.rot.toFloat()) }, true)
    }
    val crop = p.crop
    if (withCrop && crop != null) {
      val x = (crop[0] * bmp.width).toInt().coerceIn(0, bmp.width - 1)
      val y = (crop[1] * bmp.height).toInt().coerceIn(0, bmp.height - 1)
      val w = (crop[2] * bmp.width).toInt().coerceIn(1, bmp.width - x)
      val h = (crop[3] * bmp.height).toInt().coerceIn(1, bmp.height - y)
      bmp = Bitmap.createBitmap(bmp, x, y, w, h)
    }
    val out = Bitmap.createBitmap(bmp.width, bmp.height, Bitmap.Config.ARGB_8888)
    Canvas(out).drawBitmap(bmp, 0f, 0f, Paint(Paint.FILTER_BITMAP_FLAG).apply { colorFilter = ColorMatrixColorFilter(colorMatrix(p)) })
    return out
  }

  private fun check(path: String): File {
    val f = File(path)
    if (!f.isAbsolute || path.split('/').contains("..") || !f.isFile) throw IllegalArgumentException("Nur Dateien lassen sich bearbeiten.")
    return f
  }

  /** Vorschau (ohne Zuschnitt, der als Rahmen darüber liegt) mit höchstens `edge` Pixeln Kante. */
  suspend fun preview(context: Context, path: String, paramsJson: String, edge: Int, withCrop: Boolean): String = withContext(Dispatchers.Default) {
    val f = check(path)
    val p = parse(paramsJson)
    val dir = File(context.cacheDir, "edit").apply { mkdirs() }
    val key = MessageDigest.getInstance("SHA-256")
      .digest("${f.absolutePath}|${f.lastModified()}|${f.length()}|${p.key()}|$edge|$withCrop".toByteArray())
      .joinToString("") { "%02x".format(it) }.take(40)
    val out = File(dir, "$key.jpg")
    if (out.isFile && out.length() > 0) return@withContext out.absolutePath
    lock.withLock {
      val src = decode(f, 4_000_000L, edge.coerceIn(64, 2048))
      val res = process(src, p, withCrop)
      val tmp = File(dir, "$key.tmp")
      tmp.outputStream().use { res.compress(Bitmap.CompressFormat.JPEG, 88, it) }
      tmp.renameTo(out)
      out.absolutePath
    }
  }

  /**
   * In voller Größe speichern. `mode = "copy"`: neue Datei „… (bearbeitet).jpg“ neben dem
   * Original. `mode = "temp"`: versteckte Temp-Datei im selben Ordner (der Kern tauscht sie gegen
   * das Original). Ergebnis: Pfad der geschriebenen Datei.
   */
  suspend fun save(path: String, paramsJson: String, mode: String): String = withContext(Dispatchers.Default) {
    val f = check(path)
    val p = parse(paramsJson)
    lock.withLock {
      val src = decode(f, MAX_PIXELS)
      val res = process(src, p, true)
      val lower = f.name.lowercase()
      val png = lower.endsWith(".png")
      val ext = if (png) "png" else "jpg"
      val stem = f.name.substringBeforeLast('.', f.name)
      val out = if (mode == "temp") {
        File(f.parentFile, ".snap-edit-${System.nanoTime()}.$ext")
      } else {
        var c = File(f.parentFile, "$stem (bearbeitet).$ext")
        var n = 2
        while (c.exists()) c = File(f.parentFile, "$stem (bearbeitet $n).$ext").also { n++ }
        c
      }
      // Nie überschreiben
      if (!out.createNewFile()) throw IllegalStateException("Zieldatei existiert bereits.")
      try {
        out.outputStream().use { res.compress(if (png) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, 92, it) }
        if (!png && (lower.endsWith(".jpg") || lower.endsWith(".jpeg"))) copyExif(f, out)
      } catch (e: Throwable) {
        out.delete()
        throw e
      }
      out.absolutePath
    }
  }

  /** Aufnahmedatum, Kamera und Ort aus dem Original übernehmen; Ausrichtung ist jetzt „normal“. */
  private fun copyExif(from: File, to: File) {
    try {
      val src = ExifInterface(from.absolutePath)
      val dst = ExifInterface(to.absolutePath)
      for (tag in listOf(
        ExifInterface.TAG_DATETIME, ExifInterface.TAG_DATETIME_ORIGINAL, ExifInterface.TAG_DATETIME_DIGITIZED,
        ExifInterface.TAG_MAKE, ExifInterface.TAG_MODEL,
        ExifInterface.TAG_GPS_LATITUDE, ExifInterface.TAG_GPS_LATITUDE_REF,
        ExifInterface.TAG_GPS_LONGITUDE, ExifInterface.TAG_GPS_LONGITUDE_REF,
        ExifInterface.TAG_GPS_ALTITUDE, ExifInterface.TAG_GPS_ALTITUDE_REF,
      )) {
        src.getAttribute(tag)?.let { dst.setAttribute(tag, it) }
      }
      dst.setAttribute(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL.toString())
      dst.saveAttributes()
    } catch (_: Throwable) {
      // Ohne Metadaten ist das Bild trotzdem gespeichert
    }
  }

  fun prune(context: Context, maxBytes: Long = 80L * 1024 * 1024) {
    val files = File(context.cacheDir, "edit").listFiles()?.sortedBy { it.lastModified() } ?: return
    var total = files.sumOf { it.length() }
    for (f in files) {
      if (total <= maxBytes) break
      total -= f.length()
      f.delete()
    }
  }
}

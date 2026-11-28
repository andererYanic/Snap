package de.snap.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.BitmapRegionDecoder
import android.graphics.Matrix
import android.graphics.Rect
import android.media.ExifInterface
import android.os.Build
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.security.MessageDigest

/**
 * Scharfe Ausschnitte beim Zoomen in Fotos: Statt das ganze Bild in voller Auflösung zu laden
 * (bei 50 MP ≈ 200 MB), wird nur der sichtbare Teil dekodiert (`BitmapRegionDecoder`, JPEG, PNG,
 * WebP, ab Android 9 HEIF) – in der Auflösung des Bildschirms. EXIF-Drehungen werden beachtet.
 * Ergebnisse liegen kurz im Cache (`zoom/`), alte werden laufend entfernt.
 */
object ZoomTiles {
  private const val MAX_FILES = 40
  private const val MAX_AGE_MS = 10 * 60 * 1000L

  /** Hält den Cache klein: höchstens 40 Dateien, keine älter als 10 Minuten. */
  fun prune(dir: File) {
    val files = dir.listFiles()?.sortedByDescending { it.lastModified() } ?: return
    val now = System.currentTimeMillis()
    files.forEachIndexed { i, f -> if (i >= MAX_FILES || now - f.lastModified() > MAX_AGE_MS) f.delete() }
  }

  /**
   * Ausschnitt `x, y, w, h` (0…1, bezogen auf das Bild, wie es angezeigt wird – also nach der
   * EXIF-Drehung) mit etwa `outW` Pixeln Breite. null, wenn das Format keine Ausschnitte erlaubt.
   */
  suspend fun image(context: Context, path: String, x: Double, y: Double, w: Double, h: Double, outW: Int): String? =
    withContext(Dispatchers.IO) {
      val f = File(path)
      if (!f.isAbsolute || path.split('/').contains("..") || !f.isFile) return@withContext null
      if (!(w > 0 && h > 0 && x >= 0 && y >= 0 && x + w <= 1.0001 && y + h <= 1.0001)) return@withContext null
      val dir = File(context.cacheDir, "zoom").apply { mkdirs() }
      prune(dir)
      val key = MessageDigest.getInstance("SHA-256")
        .digest("${f.absolutePath}|${f.lastModified()}|${f.length()}|${"%.4f %.4f %.4f %.4f".format(x, y, w, h)}|$outW".toByteArray())
        .joinToString("") { "%02x".format(it) }
        .take(40)
      val out = File(dir, "i$key.jpg")
      if (out.isFile && out.length() > 0) return@withContext out.absolutePath
      try {
        val deg = when (ExifInterface(f.absolutePath).getAttributeInt(ExifInterface.TAG_ORIENTATION, 1)) {
          ExifInterface.ORIENTATION_NORMAL, ExifInterface.ORIENTATION_UNDEFINED -> 0
          ExifInterface.ORIENTATION_ROTATE_90 -> 90
          ExifInterface.ORIENTATION_ROTATE_180 -> 180
          ExifInterface.ORIENTATION_ROTATE_270 -> 270
          else -> return@withContext null // gespiegelt: selten, dann ohne Ausschnitt
        }
        val dec = if (Build.VERSION.SDK_INT >= 31) BitmapRegionDecoder.newInstance(f.absolutePath)
        else @Suppress("DEPRECATION") BitmapRegionDecoder.newInstance(f.absolutePath, false)
        dec ?: return@withContext null
        try {
          val rw = dec.width.toDouble()
          val rh = dec.height.toDouble()
          // Angezeigtes Rechteck → Rechteck in der gespeicherten (ungedrehten) Datei
          val (lx, ly, lw, lh) = when (deg) {
            90 -> listOf(y * rw, (1 - x - w) * rh, h * rw, w * rh)
            180 -> listOf((1 - x - w) * rw, (1 - y - h) * rh, w * rw, h * rh)
            270 -> listOf((1 - y - h) * rw, x * rh, h * rw, w * rh)
            else -> listOf(x * rw, y * rh, w * rw, h * rh)
          }
          val rect = Rect(lx.toInt().coerceAtLeast(0), ly.toInt().coerceAtLeast(0),
            (lx + lw).toInt().coerceAtMost(dec.width), (ly + lh).toInt().coerceAtMost(dec.height))
          if (rect.width() < 1 || rect.height() < 1) return@withContext null
          // Angezeigte Breite des Ausschnitts in Dateipixeln (bei 90/270 ist das die Höhe)
          val shownW = if (deg == 90 || deg == 270) rect.height() else rect.width()
          var sample = 1
          while (shownW / (sample * 2) >= outW.coerceIn(16, 4096)) sample *= 2
          val opts = BitmapFactory.Options().apply { inSampleSize = sample; inPreferredConfig = Bitmap.Config.ARGB_8888 }
          var bmp = dec.decodeRegion(rect, opts) ?: return@withContext null
          if (deg != 0) {
            val m = Matrix().apply { postRotate(deg.toFloat()) }
            val rotated = Bitmap.createBitmap(bmp, 0, 0, bmp.width, bmp.height, m, true)
            if (rotated !== bmp) bmp.recycle()
            bmp = rotated
          }
          val tmp = File(dir, "i$key.tmp")
          tmp.outputStream().use { bmp.compress(Bitmap.CompressFormat.JPEG, 92, it) }
          bmp.recycle()
          tmp.renameTo(out)
          out.absolutePath
        } finally {
          dec.recycle()
        }
      } catch (_: Throwable) {
        null
      }
    }
}

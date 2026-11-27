package de.snap.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.os.Build
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File

/**
 * Texterkennung (OCR) mit Tesseract 5, eingebaut und offline: Deutsch + Englisch.
 * Die Sprachdaten liegen in der App (assets/tessdata) und werden beim ersten Gebrauch in den
 * privaten App-Ordner kopiert. Bilder werden auf höchstens [MAX_EDGE] Pixel verkleinert.
 */
object Ocr {
  private const val MAX_EDGE = 3000
  private const val LANGS = "deu+eng"
  private val lock = Mutex()
  @Volatile private var loaded: Boolean? = null

  @JvmStatic private external fun nativeRecognize(bitmap: Bitmap, dataPath: String, langs: String): ByteArray?

  private fun load(): Boolean = loaded ?: try {
    System.loadLibrary("snap_ocr")
    true
  } catch (_: Throwable) {
    false
  }.also { loaded = it }

  /** Sprachdaten bereitstellen (nur bei fehlender oder abweichender Datei kopieren). */
  private fun dataDir(ctx: Context): File {
    val dir = File(ctx.filesDir, "ocr/tessdata").apply { mkdirs() }
    for (lang in listOf("deu", "eng")) {
      val name = "$lang.traineddata"
      val out = File(dir, name)
      val size = ctx.assets.openFd("tessdata/$name").use { it.length }
      if (out.length() == size) continue
      val tmp = File(dir, "$name.tmp")
      ctx.assets.open("tessdata/$name").use { input -> tmp.outputStream().use { input.copyTo(it) } }
      tmp.renameTo(out)
    }
    return dir
  }

  private fun decode(path: String): Bitmap? {
    val f = File(path)
    if (!f.isAbsolute || path.split('/').contains("..") || !f.isFile) return null
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(path, bounds)
    val (w, h) = bounds.outWidth to bounds.outHeight
    if (Build.VERSION.SDK_INT >= 28) {
      // ImageDecoder kann auch HEIC und dreht nach EXIF
      return try {
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(f)) { d, info, _ ->
          d.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
          val edge = maxOf(info.size.width, info.size.height)
          if (edge > MAX_EDGE) {
            val s = MAX_EDGE.toDouble() / edge
            d.setTargetSize((info.size.width * s).toInt().coerceAtLeast(1), (info.size.height * s).toInt().coerceAtLeast(1))
          }
        }.copy(Bitmap.Config.ARGB_8888, false)
      } catch (_: Throwable) {
        null
      }
    }
    if (w <= 0 || h <= 0) return null
    var sample = 1
    while (maxOf(w, h) / sample > MAX_EDGE) sample *= 2
    val bmp = BitmapFactory.decodeFile(path, BitmapFactory.Options().apply {
      inSampleSize = sample
      inPreferredConfig = Bitmap.Config.ARGB_8888
    }) ?: return null
    return if (bmp.config == Bitmap.Config.ARGB_8888) bmp else bmp.copy(Bitmap.Config.ARGB_8888, false)
  }

  /** Text im Bild unter [path] erkennen. `null`: nicht möglich (kein Bild, OCR nicht verfügbar). */
  suspend fun recognize(ctx: Context, path: String): String? = withContext(Dispatchers.Default) {
    if (!load()) return@withContext null
    lock.withLock {
      val bmp = decode(path) ?: return@withLock null
      try {
        val data = dataDir(ctx.applicationContext)
        nativeRecognize(bmp, data.absolutePath, LANGS)?.toString(Charsets.UTF_8)?.trim()
      } finally {
        bmp.recycle()
      }
    }
  }
}

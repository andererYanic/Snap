package de.snap.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.ThumbnailUtils
import android.os.Build
import android.util.Size
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.withContext
import java.io.File
import java.security.MessageDigest

/**
 * Schnelle Vorschaubilder über Android selbst (nutzt eingebettete EXIF-Vorschauen und
 * Hardware-Decoder). Ergebnis als JPEG im Cache der App; Schlüssel aus Pfad, Änderungszeit und
 * Größe, damit geänderte Dateien neu erzeugt werden.
 */
object Thumbnails {
  private val slots = Semaphore(4)
  private val video = Regex("\\.(mp4|mkv|mov|webm|3gp|m4v|avi)$", RegexOption.IGNORE_CASE)

  suspend fun get(context: Context, path: String, edge: Int): String? = withContext(Dispatchers.IO) {
    val src = File(path)
    if (!src.isAbsolute || path.split('/').contains("..") || !src.isFile) return@withContext null
    val dir = File(context.cacheDir, "thumbs").apply { mkdirs() }
    val key = MessageDigest.getInstance("SHA-256")
      .digest("$path|${src.lastModified()}|${src.length()}|$edge".toByteArray())
      .joinToString("") { "%02x".format(it) }
      .take(40)
    val out = File(dir, "$key.jpg")
    if (out.isFile && out.length() > 0) return@withContext out.absolutePath
    slots.withPermit {
      val bmp = render(src, edge) ?: return@withPermit null
      val tmp = File(dir, "$key.tmp")
      tmp.outputStream().use { bmp.compress(Bitmap.CompressFormat.JPEG, 85, it) }
      bmp.recycle()
      tmp.renameTo(out)
      out.absolutePath
    }
  }

  private fun render(src: File, edge: Int): Bitmap? = try {
    val isVideo = video.containsMatchIn(src.name)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      val size = Size(edge, edge)
      if (isVideo) ThumbnailUtils.createVideoThumbnail(src, size, null) else ThumbnailUtils.createImageThumbnail(src, size, null)
    } else if (isVideo) {
      @Suppress("DEPRECATION")
      ThumbnailUtils.createVideoThumbnail(src.absolutePath, android.provider.MediaStore.Images.Thumbnails.MINI_KIND)
    } else {
      // Älteres Android: nur die Kanten lesen, dann passend verkleinert dekodieren
      val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
      BitmapFactory.decodeFile(src.absolutePath, bounds)
      var sample = 1
      while (bounds.outWidth / (sample * 2) >= edge && bounds.outHeight / (sample * 2) >= edge) sample *= 2
      BitmapFactory.decodeFile(src.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
    }
  } catch (_: Throwable) {
    null
  }

  /** Cache begrenzen: älteste Dateien löschen, bis höchstens `maxBytes` belegt sind. */
  fun prune(context: Context, maxBytes: Long = 200L * 1024 * 1024) {
    val files = File(context.cacheDir, "thumbs").listFiles()?.sortedBy { it.lastModified() } ?: return
    var total = files.sumOf { it.length() }
    for (f in files) {
      if (total <= maxBytes) break
      total -= f.length()
      f.delete()
    }
  }
}

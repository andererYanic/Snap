package de.snap.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.pdf.PdfRenderer
import android.os.ParcelFileDescriptor
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import java.security.MessageDigest

/**
 * PDF-Seiten über Android selbst (`PdfRenderer`, Teil des Systems – keine Zusatzbibliothek).
 * Seiten werden als PNG im Cache abgelegt; Schlüssel aus Pfad, Änderungszeit, Größe und Breite.
 * PDFs kommen von außen: Seitenzahl und Bildgröße sind begrenzt, Fehler führen zu `null`.
 */
object PdfPages {
  private const val MAX_PAGES = 5000
  /** Höchstens so viele Pixel je gerenderter Seite (≈ 4096 × 4096). */
  private const val MAX_PIXELS = 16_777_216L
  // PdfRenderer erlaubt nur eine offene Seite gleichzeitig je Dokument → ein Renderer, gesperrt.
  private val lock = Mutex()
  private var openPath: String? = null
  private var openStamp = 0L
  private var fd: ParcelFileDescriptor? = null
  private var renderer: PdfRenderer? = null

  private fun check(path: String): File? {
    val f = File(path)
    return if (f.isAbsolute && !path.split('/').contains("..") && f.isFile) f else null
  }

  private fun open(f: File): PdfRenderer {
    val stamp = f.lastModified() xor f.length()
    if (openPath == f.absolutePath && openStamp == stamp) renderer?.let { return it }
    close()
    val d = ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY)
    fd = d
    val r = PdfRenderer(d)
    renderer = r
    openPath = f.absolutePath
    openStamp = stamp
    return r
  }

  private fun close() {
    try { renderer?.close() } catch (_: Throwable) {}
    try { fd?.close() } catch (_: Throwable) {}
    renderer = null
    fd = null
    openPath = null
  }

  /** Seitenzahl und Seitenverhältnisse (Höhe/Breite) oder null, wenn kein lesbares PDF. */
  suspend fun info(path: String): Pair<Int, List<Double>>? = withContext(Dispatchers.IO) {
    val f = check(path) ?: return@withContext null
    lock.withLock {
      try {
        val r = open(f)
        val n = r.pageCount.coerceAtMost(MAX_PAGES)
        val ratios = (0 until n).map { i ->
          r.openPage(i).use { p -> if (p.width > 0) p.height.toDouble() / p.width else 1.414 }
        }
        n to ratios
      } catch (_: Throwable) {
        close()
        null
      }
    }
  }

  /** Rendert Seite `index` in `width` Pixeln Breite; Pfad zur PNG-Datei oder null. */
  suspend fun page(context: Context, path: String, index: Int, width: Int): String? = withContext(Dispatchers.IO) {
    val f = check(path) ?: return@withContext null
    val dir = File(context.cacheDir, "pdf").apply { mkdirs() }
    val key = MessageDigest.getInstance("SHA-256")
      .digest("${f.absolutePath}|${f.lastModified()}|${f.length()}|$index|$width".toByteArray())
      .joinToString("") { "%02x".format(it) }
      .take(40)
    val out = File(dir, "$key.png")
    if (out.isFile && out.length() > 0) return@withContext out.absolutePath
    lock.withLock {
      try {
        val r = open(f)
        if (index !in 0 until r.pageCount.coerceAtMost(MAX_PAGES)) return@withLock null
        r.openPage(index).use { p ->
          var w = width.coerceIn(64, 4096)
          var h = (w.toLong() * p.height / p.width.coerceAtLeast(1)).coerceAtLeast(1)
          if (w * h > MAX_PIXELS) {
            val s = Math.sqrt(MAX_PIXELS.toDouble() / (w * h))
            w = (w * s).toInt().coerceAtLeast(1)
            h = (h * s).toLong().coerceAtLeast(1)
          }
          val bmp = Bitmap.createBitmap(w, h.toInt(), Bitmap.Config.ARGB_8888)
          bmp.eraseColor(Color.WHITE)
          p.render(bmp, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
          val tmp = File(dir, "$key.tmp")
          tmp.outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
          bmp.recycle()
          tmp.renameTo(out)
          out.absolutePath
        }
      } catch (_: Throwable) {
        close()
        null
      }
    }
  }

  /** Cache begrenzen (älteste zuerst löschen) – auch die Bilder aus Word-Dateien. */
  fun prune(context: Context, maxBytes: Long = 150L * 1024 * 1024) {
    val docx = File(context.cacheDir, "docx").listFiles()?.sortedBy { it.lastModified() }
    if (docx != null) {
      var t = docx.sumOf { it.length() }
      for (f in docx) {
        if (t <= 100L * 1024 * 1024) break
        t -= f.length()
        f.delete()
      }
    }
    val files = File(context.cacheDir, "pdf").listFiles()?.sortedBy { it.lastModified() } ?: return
    var total = files.sumOf { it.length() }
    for (f in files) {
      if (total <= maxBytes) break
      total -= f.length()
      f.delete()
    }
  }
}

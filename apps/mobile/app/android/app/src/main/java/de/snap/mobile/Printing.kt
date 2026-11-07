package de.snap.mobile

import android.app.Activity
import android.content.Context
import android.graphics.BitmapFactory
import android.graphics.pdf.PdfDocument
import android.os.Bundle
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.print.PageRange
import android.print.PrintAttributes
import android.print.PrintDocumentAdapter
import android.print.PrintDocumentInfo
import android.print.PrintManager
import java.io.File
import java.io.FileOutputStream

/**
 * Drucken über das Druckframework von Android (Druckdialog des Systems, Druckdienste wie der
 * „Standard-Druckdienst“ oder Mopria – ohne Google-Dienste). PDFs werden unverändert
 * weitergereicht, Bilder auf eine Seite eingepasst.
 */
object Printing {
  fun print(activity: Activity, file: File) {
    val pm = activity.getSystemService(Context.PRINT_SERVICE) as PrintManager
    val name = file.name
    val adapter = if (file.name.lowercase().endsWith(".pdf")) PdfAdapter(file) else ImageAdapter(file)
    pm.print(name, adapter, PrintAttributes.Builder().build())
  }

  private class PdfAdapter(val file: File) : PrintDocumentAdapter() {
    override fun onLayout(old: PrintAttributes?, new: PrintAttributes, cancel: CancellationSignal, cb: LayoutResultCallback, extras: Bundle?) {
      if (cancel.isCanceled) return cb.onLayoutCancelled()
      val info = PrintDocumentInfo.Builder(file.name).setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT).build()
      cb.onLayoutFinished(info, old != new)
    }

    override fun onWrite(pages: Array<out PageRange>, dest: ParcelFileDescriptor, cancel: CancellationSignal, cb: WriteResultCallback) {
      try {
        file.inputStream().use { input -> FileOutputStream(dest.fileDescriptor).use { input.copyTo(it) } }
        if (cancel.isCanceled) cb.onWriteCancelled() else cb.onWriteFinished(arrayOf(PageRange.ALL_PAGES))
      } catch (e: Throwable) {
        cb.onWriteFailed(e.message)
      }
    }
  }

  private class ImageAdapter(val file: File) : PrintDocumentAdapter() {
    private var attrs: PrintAttributes? = null

    override fun onLayout(old: PrintAttributes?, new: PrintAttributes, cancel: CancellationSignal, cb: LayoutResultCallback, extras: Bundle?) {
      if (cancel.isCanceled) return cb.onLayoutCancelled()
      attrs = new
      val info = PrintDocumentInfo.Builder(file.nameWithoutExtension + ".pdf")
        .setContentType(PrintDocumentInfo.CONTENT_TYPE_PHOTO).setPageCount(1).build()
      cb.onLayoutFinished(info, true)
    }

    override fun onWrite(pages: Array<out PageRange>, dest: ParcelFileDescriptor, cancel: CancellationSignal, cb: WriteResultCallback) {
      val doc = PdfDocument()
      try {
        val media = attrs?.mediaSize ?: PrintAttributes.MediaSize.ISO_A4
        // Seitengröße in Punkt (1/72 Zoll); Maße der MediaSize sind in Tausendstel Zoll
        val pw = media.widthMils * 72 / 1000
        val ph = media.heightMils * 72 / 1000
        // Bild höchstens doppelt so fein wie die Seite dekodieren (spart Speicher)
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        var sample = 1
        while (bounds.outWidth / (sample * 2) >= pw * 4 && bounds.outHeight / (sample * 2) >= ph * 4) sample *= 2
        val bmp = BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
          ?: return cb.onWriteFailed("Bild nicht lesbar")
        val page = doc.startPage(PdfDocument.PageInfo.Builder(pw, ph, 1).create())
        val margin = 18f
        val s = minOf((pw - 2 * margin) / bmp.width, (ph - 2 * margin) / bmp.height)
        val w = bmp.width * s
        val h = bmp.height * s
        val left = (pw - w) / 2
        val top = (ph - h) / 2
        page.canvas.drawBitmap(bmp, null, android.graphics.RectF(left, top, left + w, top + h), null)
        doc.finishPage(page)
        bmp.recycle()
        FileOutputStream(dest.fileDescriptor).use { doc.writeTo(it) }
        if (cancel.isCanceled) cb.onWriteCancelled() else cb.onWriteFinished(arrayOf(PageRange.ALL_PAGES))
      } catch (e: Throwable) {
        cb.onWriteFailed(e.message)
      } finally {
        doc.close()
      }
    }
  }
}

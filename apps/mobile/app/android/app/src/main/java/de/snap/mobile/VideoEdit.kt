package de.snap.mobile

import android.content.Context
import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.media3.common.MediaItem
import androidx.media3.common.util.UnstableApi
import androidx.media3.effect.ScaleAndRotateTransformation
import androidx.media3.transformer.Composition
import androidx.media3.transformer.EditedMediaItem
import androidx.media3.transformer.Effects
import androidx.media3.transformer.ExportException
import androidx.media3.transformer.ExportResult
import androidx.media3.transformer.ProgressHolder
import androidx.media3.transformer.Transformer
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/**
 * Videobearbeitung über Media3 Transformer (läuft vollständig auf dem Gerät, ohne Google-Dienste):
 * Kürzen, Ton entfernen, in 90°-Schritten drehen. Ergebnis ist immer MP4 – als Kopie neben dem
 * Original oder als Temp-Datei, die der Kern gegen das Original tauscht (wie bei Bildern).
 */
@UnstableApi
object VideoEdit {
  private val lock = Mutex()
  private val main = Handler(Looper.getMainLooper())

  // Laufender Export (nur auf dem Haupt-Thread angefasst)
  private var running: Transformer? = null
  private var pending: CancellableContinuation<Unit>? = null
  private val holder = ProgressHolder()

  data class Params(val startMs: Long, val endMs: Long, val mute: Boolean, val rot: Int)

  fun parse(json: String, durationMs: Long): Params {
    val o = JSONObject(json)
    val start = o.optLong("startMs", 0).coerceIn(0, maxOf(0, durationMs - 100))
    val end = o.optLong("endMs", durationMs).let { if (it <= 0) durationMs else it }.coerceIn(start + 100, maxOf(start + 100, durationMs))
    val rot = ((o.optInt("rot", 0) % 360) + 360) % 360 / 90 * 90
    return Params(start, end, o.optBoolean("mute", false), rot)
  }

  private fun check(path: String): File {
    val f = File(path)
    if (!f.isAbsolute || path.split('/').contains("..") || !f.isFile) throw IllegalArgumentException("Nur Dateien lassen sich bearbeiten.")
    return f
  }

  private fun <T> retriever(f: File, block: (MediaMetadataRetriever) -> T): T {
    val r = MediaMetadataRetriever()
    try {
      r.setDataSource(f.absolutePath)
      return block(r)
    } finally {
      r.release()
    }
  }

  private fun durationOf(r: MediaMetadataRetriever): Long =
    r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull() ?: 0L

  /** Dauer, Größe, Ausrichtung und ob eine Tonspur da ist. */
  suspend fun info(path: String): String = withContext(Dispatchers.IO) {
    val f = check(path)
    retriever(f) { r ->
      JSONObject()
        .put("durationMs", durationOf(r))
        .put("width", r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull() ?: 0)
        .put("height", r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull() ?: 0)
        .put("rotation", r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION)?.toIntOrNull() ?: 0)
        .put("audio", r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_HAS_AUDIO) == "yes")
        .toString()
    }
  }

  /** `count` gleichmäßig verteilte Einzelbilder für die Zeitleiste (zwischengespeichert). */
  suspend fun frames(context: Context, path: String, count: Int, edge: Int): String = withContext(Dispatchers.IO) {
    val f = check(path)
    val n = count.coerceIn(1, 16)
    val e = edge.coerceIn(32, 320)
    val dir = File(context.cacheDir, "edit").apply { mkdirs() }
    val base = MessageDigest.getInstance("SHA-256")
      .digest("${f.absolutePath}|${f.lastModified()}|${f.length()}|$n|$e".toByteArray())
      .joinToString("") { "%02x".format(it) }.take(32)
    val out = JSONArray()
    val wanted = (0 until n).map { File(dir, "v$base-$it.jpg") }
    if (wanted.all { it.isFile && it.length() > 0 }) {
      wanted.forEach { out.put(it.absolutePath) }
      return@withContext out.toString()
    }
    lock.withLock {
      retriever(f) { r ->
        val dur = durationOf(r)
        for (i in 0 until n) {
          val t = (dur * 1000L) * (2 * i + 1) / (2 * n) // Mitte jedes Abschnitts, in µs
          val bmp: Bitmap? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            r.getScaledFrameAtTime(t, MediaMetadataRetriever.OPTION_CLOSEST_SYNC, e, e)
          } else {
            r.getFrameAtTime(t, MediaMetadataRetriever.OPTION_CLOSEST_SYNC)?.let { b ->
              val s = e.toFloat() / maxOf(b.width, b.height)
              if (s < 1f) Bitmap.createScaledBitmap(b, maxOf(1, (b.width * s).toInt()), maxOf(1, (b.height * s).toInt()), true) else b
            }
          }
          val file = wanted[i]
          if (bmp != null) {
            val tmp = File(dir, "v$base-$i.tmp")
            tmp.outputStream().use { bmp.compress(Bitmap.CompressFormat.JPEG, 80, it) }
            tmp.renameTo(file)
            out.put(file.absolutePath)
          } else {
            out.put(JSONObject.NULL)
          }
        }
      }
    }
    out.toString()
  }

  /**
   * Exportieren. `mode = "copy"`: neue Datei „… (bearbeitet).mp4“ neben dem Original.
   * `mode = "temp"`: versteckte Temp-Datei im selben Ordner (der Kern tauscht sie gegen das
   * Original). Ergebnis: Pfad der geschriebenen Datei. Abbrechen mit [cancel].
   */
  suspend fun save(context: Context, path: String, paramsJson: String, mode: String): String {
    val f = check(path)
    return lock.withLock {
      val dur = withContext(Dispatchers.IO) { retriever(f, ::durationOf) }
      if (dur <= 0) throw IllegalArgumentException("Video nicht lesbar.")
      val p = parse(paramsJson, dur)
      val stem = f.name.substringBeforeLast('.', f.name)
      val out = withContext(Dispatchers.IO) {
        val o = if (mode == "temp") {
          File(f.parentFile, ".snap-edit-${System.nanoTime()}.mp4")
        } else {
          var c = File(f.parentFile, "$stem (bearbeitet).mp4")
          var n = 2
          while (c.exists()) c = File(f.parentFile, "$stem (bearbeitet $n).mp4").also { n++ }
          c
        }
        // Platz reservieren, nie überschreiben
        if (!o.createNewFile()) throw IllegalStateException("Zieldatei existiert bereits.")
        o
      }
      try {
        export(context, f, p, out)
        out.absolutePath
      } catch (e: Throwable) {
        out.delete()
        throw e
      }
    }
  }

  private suspend fun export(context: Context, src: File, p: Params, out: File) = suspendCancellableCoroutine<Unit> { cont ->
    main.post {
      try {
        val clip = MediaItem.ClippingConfiguration.Builder()
          .setStartPositionMs(p.startMs)
          .setEndPositionMs(p.endMs)
          .build()
        val item = MediaItem.Builder().setUri(Uri.fromFile(src)).setClippingConfiguration(clip).build()
        val effects = if (p.rot != 0) {
          // Media3 dreht gegen den Uhrzeigersinn, `rot` zählt im Uhrzeigersinn
          Effects(emptyList(), listOf(ScaleAndRotateTransformation.Builder().setRotationDegrees((360 - p.rot).toFloat()).build()))
        } else {
          Effects.EMPTY
        }
        val edited = EditedMediaItem.Builder(item).setRemoveAudio(p.mute).setEffects(effects).build()
        val t = Transformer.Builder(context.applicationContext)
          .addListener(object : Transformer.Listener {
            override fun onCompleted(composition: Composition, result: ExportResult) {
              running = null
              pending = null
              if (cont.isActive) cont.resume(Unit)
            }

            override fun onError(composition: Composition, result: ExportResult, exception: ExportException) {
              running = null
              pending = null
              if (cont.isActive) cont.resumeWithException(IllegalStateException("Export fehlgeschlagen: ${exception.errorCodeName}"))
            }
          })
          .build()
        running = t
        pending = cont
        t.start(edited, out.absolutePath)
      } catch (e: Throwable) {
        running = null
        pending = null
        if (cont.isActive) cont.resumeWithException(e)
      }
    }
    cont.invokeOnCancellation { main.post { running?.cancel(); running = null; pending = null } }
  }

  /** Fortschritt des laufenden Exports in Prozent, -1 wenn keiner läuft oder unbekannt. */
  fun progress(cb: (Int) -> Unit) {
    main.post {
      val t = running
      if (t == null) {
        cb(-1)
      } else {
        cb(if (t.getProgress(holder) == Transformer.PROGRESS_STATE_AVAILABLE) holder.progress else -1)
      }
    }
  }

  /** Laufenden Export abbrechen (die halbfertige Datei wird gelöscht). */
  fun cancel() {
    main.post {
      val t = running ?: return@post
      running = null
      t.cancel() // meldet sich danach nicht mehr – daher hier beenden
      pending?.let { if (it.isActive) it.resumeWithException(IllegalStateException("Abgebrochen.")) }
      pending = null
    }
  }
}

package de.snap.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import java.io.File

/**
 * Aus dem „Teilen“-Menü anderer Apps empfangene Inhalte für Snap Drop.
 *
 * Sicherheit:
 * - Nur `content://`-Adressen fremder Apps; `file://` und Snaps eigene Inhalte werden abgewiesen
 *   (sonst könnte eine App Snap dazu bringen, private Dateien zu versenden).
 * - Dateien werden in einen eigenen Cache kopiert (höchstens 500 Dateien, 4 GB), Namen bereinigt.
 * - Text höchstens 64 KB.
 * - Gesendet wird erst, wenn der Mensch im Senden-Blatt einen Empfänger wählt.
 */
object ShareInbox {
  private const val MAX_FILES = 500
  private const val MAX_BYTES = 4L * 1024 * 1024 * 1024
  private const val MAX_TEXT = 64 * 1024

  private data class Pending(val uris: List<Uri>, val text: String?)

  @Volatile private var pending: Pending? = null
  /** Wird gerufen, wenn neu geteilt wurde und die Oberfläche läuft. */
  @Volatile var listener: (() -> Unit)? = null

  /** Nimmt einen Teilen-Intent an (aus `MainActivity`). `true`, wenn es einer war. */
  fun accept(context: Context, intent: Intent?): Boolean {
    if (intent == null) return false
    val action = intent.action
    if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) return false
    val uris = mutableListOf<Uri>()
    if (action == Intent.ACTION_SEND) {
      streamExtra(intent)?.let { uris.add(it) }
    } else {
      uris.addAll(streamList(intent))
    }
    val own = "${context.packageName}."
    val ok = uris.filter { u -> u.scheme == "content" && !(u.authority ?: "").startsWith(own) && u.authority != context.packageName }
      .take(MAX_FILES)
    val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.take(MAX_TEXT)?.takeIf { it.isNotBlank() }
    if (ok.isEmpty() && text == null) return true
    pending = Pending(ok, text)
    try { listener?.invoke() } catch (_: Throwable) {}
    return true
  }

  @Suppress("DEPRECATION")
  private fun streamExtra(intent: Intent): Uri? =
    if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableExtra(Intent.EXTRA_STREAM) as? Uri

  @Suppress("DEPRECATION")
  private fun streamList(intent: Intent): List<Uri> =
    (if (Build.VERSION.SDK_INT >= 33) intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)) ?: emptyList()

  /** Holt das Geteilte ab (einmalig): Dateien werden in den Cache kopiert. */
  fun take(context: Context): Pair<List<String>, String?>? {
    val p = pending ?: return null
    pending = null
    val root = File(context.cacheDir, "share").apply { mkdirs() }
    // Alte Ablagen (älter als ein Tag) aufräumen
    root.listFiles()?.filter { System.currentTimeMillis() - it.lastModified() > 86_400_000L }?.forEach { it.deleteRecursively() }
    val dir = File(root, System.currentTimeMillis().toString()).apply { mkdirs() }
    val out = mutableListOf<String>()
    var total = 0L
    for ((i, u) in p.uris.withIndex()) {
      try {
        val name = unique(dir, cleanName(displayName(context, u) ?: "Datei-${i + 1}"))
        val f = File(dir, name)
        context.contentResolver.openInputStream(u)?.use { input ->
          f.outputStream().use { o ->
            val buf = ByteArray(256 * 1024)
            while (true) {
              val n = input.read(buf)
              if (n < 0) break
              total += n
              if (total > MAX_BYTES) throw IllegalStateException("zu groß")
              o.write(buf, 0, n)
            }
          }
        } ?: continue
        out.add(f.absolutePath)
      } catch (e: Throwable) {
        android.util.Log.w("SnapShare", "Geteilte Datei nicht lesbar: ${e.message}")
        if (total > MAX_BYTES) break
      }
    }
    if (out.isEmpty() && p.text == null) return null
    return out to p.text
  }

  internal fun displayName(context: Context, u: Uri): String? = try {
    context.contentResolver.query(u, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
      if (c.moveToFirst()) c.getString(0) else null
    }
  } catch (_: Throwable) {
    null
  }

  /** Nur der letzte Pfadteil, ohne Steuerzeichen und Trennzeichen, nicht versteckt, ≤ 120 Zeichen. */
  internal fun cleanName(raw: String): String {
    var s = raw.substringAfterLast('/').substringAfterLast('\\')
      .filter { !it.isISOControl() && it != ':' && it.code !in 0x200B..0x200F && it.code !in 0x202A..0x202E }
      .trim().trimEnd('.')
    if (s.startsWith(".")) s = "_$s"
    if (s.isEmpty()) s = "Datei"
    return s.take(120)
  }

  private fun unique(dir: File, name: String): String {
    if (!File(dir, name).exists()) return name
    val dot = name.lastIndexOf('.')
    val (stem, ext) = if (dot > 0) name.substring(0, dot) to name.substring(dot) else name to ""
    var n = 2
    while (File(dir, "$stem ($n)$ext").exists()) n++
    return "$stem ($n)$ext"
  }
}

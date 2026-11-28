package de.snap.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Environment
import java.io.File

/**
 * „Öffnen mit Snap“ (Dateien) und „Ordner öffnen“ aus anderen Apps.
 *
 * Sicherheit (Eingaben fremder Apps, docs/SECURITY_MODEL.md):
 * - Adressen übersetzt und prüft der Kern (`resolveExternal`, Fuzz-Ziel `intent_uri`): nur Pfade
 *   im gemeinsamen Speicher, nie Snaps eigene Ordner (Schlüssel, Datenbank, Tresor) – auch nicht
 *   über Symlinks oder `..`.
 * - Andere `content://`-Inhalte fremder Apps werden in einen eigenen Cache kopiert (höchstens
 *   2 GB) und nur angezeigt. Snaps eigene Adressen werden abgewiesen.
 * - Es wird nur angezeigt bzw. ein Ordner geöffnet – nichts geteilt, gesendet oder verändert.
 */
object ExternalOpen {
  private const val MAX_COPY = 2L * 1024 * 1024 * 1024
  private val DIR_TYPES = setOf("vnd.android.document/directory", "resource/folder", "inode/directory")

  data class Request(val uri: Uri, val dir: Boolean)
  data class Opened(val kind: String, val path: String?)

  @Volatile private var pending: Request? = null
  /** Wird gerufen, wenn etwas zu öffnen ist und die Oberfläche läuft. */
  @Volatile var listener: (() -> Unit)? = null

  /** Nimmt einen VIEW-Intent an (aus `MainActivity`). `true`, wenn es einer war. */
  fun accept(context: Context, intent: Intent?): Boolean {
    if (intent?.action != Intent.ACTION_VIEW) return false
    val uri = intent.data ?: return true
    if (uri.scheme != "content" && uri.scheme != "file") return true
    if (isOwn(context, uri)) return true
    pending = Request(uri, intent.type in DIR_TYPES)
    try { listener?.invoke() } catch (_: Throwable) {}
    return true
  }

  private fun isOwn(context: Context, uri: Uri): Boolean {
    val a = uri.authority ?: return false
    return a == context.packageName || a.startsWith("${context.packageName}.")
  }

  fun storageRoot(): String = Environment.getExternalStorageDirectory().absolutePath

  /** Snaps eigene Ordner: interner Datenordner und die eigenen Ordner im gemeinsamen Speicher. */
  fun ownDirs(context: Context): List<String> {
    val dirs = mutableListOf(context.applicationInfo.dataDir, context.filesDir.absolutePath, context.cacheDir.absolutePath)
    (context.getExternalFilesDirs(null) + context.getExternalCacheDirs() + context.obbDirs)
      .filterNotNull()
      .forEach { d -> d.parentFile?.let { dirs.add(it.absolutePath) } ?: dirs.add(d.absolutePath) }
    return dirs
  }

  /**
   * Holt das zu Öffnende ab (einmalig, blockierend – ggf. wird kopiert):
   * `dir`/`file` mit Pfad, `unsupported` (nicht erlaubt bzw. unbekannt) oder null.
   */
  fun take(context: Context): Opened? {
    val r = pending ?: return null
    pending = null
    val root = storageRoot()
    val own = ownDirs(context)
    val s = r.uri.toString()
    // Ordner: nur, was sich sicher auf einen Ordner im Speicher abbilden lässt
    uniffi.snap_ffi.resolveExternal(s, true, root, own)?.let { return Opened("dir", it) }
    if (r.dir) return Opened("unsupported", null)
    uniffi.snap_ffi.resolveExternal(s, false, root, own)?.let { return Opened("file", it) }
    // file:// außerhalb des Speichers nie; content:// fremder Apps in den Cache kopieren
    if (r.uri.scheme != "content") return Opened("unsupported", null)
    return copy(context, r.uri)?.let { Opened("file", it) } ?: Opened("unsupported", null)
  }

  private fun copy(context: Context, uri: Uri): String? {
    val base = File(context.cacheDir, "open").apply { mkdirs() }
    base.listFiles()?.filter { System.currentTimeMillis() - it.lastModified() > 86_400_000L }?.forEach { it.deleteRecursively() }
    val dir = File(base, System.currentTimeMillis().toString()).apply { mkdirs() }
    val f = File(dir, ShareInbox.cleanName(ShareInbox.displayName(context, uri) ?: "Datei"))
    return try {
      var total = 0L
      context.contentResolver.openInputStream(uri)?.use { input ->
        f.outputStream().use { o ->
          val buf = ByteArray(256 * 1024)
          while (true) {
            val n = input.read(buf)
            if (n < 0) break
            total += n
            if (total > MAX_COPY) throw IllegalStateException("zu groß")
            o.write(buf, 0, n)
          }
        }
      } ?: return null
      f.absolutePath
    } catch (_: Throwable) {
      dir.deleteRecursively()
      null
    }
  }
}

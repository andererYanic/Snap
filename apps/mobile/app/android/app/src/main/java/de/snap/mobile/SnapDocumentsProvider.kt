package de.snap.mobile

import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.Environment
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.webkit.MimeTypeMap
import kotlinx.coroutines.runBlocking
import uniffi.snap_ffi.DocumentId
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileNotFoundException

/**
 * „Snap“ als Quelle in der System-Dateiauswahl (Seitenleiste, wie Google Drive): Dateien eigener
 * Geräte und mit Snap Drop Empfangenes, z. B. zum Anhängen in WhatsApp.
 *
 * Sicherheit (docs/SECURITY_MODEL.md, Entscheidung 2026-11-28):
 * - Aufrufbar nur von der Systemoberfläche (`MANAGE_DOCUMENTS`); andere Apps erhalten von Android
 *   nur die eine Datei, die der Mensch dort antippt.
 * - Nur lesend: keine Schreib-, Lösch- oder Ordnerrechte, `openDocument` nur mit „r“. Keine
 *   Ordnerfreigaben (kein `FLAG_SUPPORTS_IS_CHILD`), damit keine App ganze Bäume bekommt.
 * - Eigene Geräte: nur, was Snap ohnehin sieht (Katalog der gekoppelten Geräte, gleiche Rechte);
 *   geladen wird verschlüsselt erst beim Öffnen, in Snaps eigenen Cache.
 * - Snap Drop: nur Dateien im Empfangsordner, vom Kern geprüft (keine Symlinks hinaus, nie Snaps
 *   eigene Ordner). Der Tresor erscheint nie.
 * - Kennungen sind Eingaben von außen: streng zerlegt, Unbekanntes → nicht gefunden.
 */
class SnapDocumentsProvider : DocumentsProvider() {
  companion object {
    private const val MAX_FILE = 4L * 1024 * 1024 * 1024
    private val ROOT_COLUMNS = arrayOf(
      Root.COLUMN_ROOT_ID, Root.COLUMN_FLAGS, Root.COLUMN_TITLE, Root.COLUMN_SUMMARY,
      Root.COLUMN_DOCUMENT_ID, Root.COLUMN_ICON, Root.COLUMN_MIME_TYPES,
    )
    private val DOC_COLUMNS = arrayOf(
      Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE,
      Document.COLUMN_FLAGS, Document.COLUMN_SIZE, Document.COLUMN_LAST_MODIFIED,
    )
  }

  override fun onCreate(): Boolean = true

  // ------------------------------------------------------------ Kennungen
  // Zerlegen und Erzeugen im Kern (`space_core::intent::DocId`, Fuzz-Ziel `document_id`): nur die
  // kanonische Form mit geprüften Teilen wird angenommen.

  private fun parse(raw: String): DocumentId = uniffi.snap_ffi.documentIdParse(raw) ?: throw FileNotFoundException()

  /** Kennung oder null (ungültige fremde Namen werden still übergangen). */
  private fun idOf(id: DocumentId): String? = uniffi.snap_ffi.documentIdMake(id)

  private fun isHex(s: String, len: Int) = s.length == len && s.all { it in '0'..'9' || it in 'a'..'f' }

  // ------------------------------------------------------------ Kern

  private fun core(json: String): JSONObject {
    val ctx = context ?: throw FileNotFoundException()
    val raw = runBlocking { CoreHolder.get(ctx.applicationContext).request(json) }
    val obj = JSONObject(raw)
    val key = obj.keys().next()
    if (key == "Error") throw FileNotFoundException(obj.getJSONObject(key).optString("message"))
    return obj.optJSONObject(key) ?: JSONObject()
  }

  private fun dropDir(): File = File(Environment.getExternalStorageDirectory(), "Download/Snap Drop")

  /** Datei bzw. Ordner im Empfangsordner, vom Kern geprüft (gemeinsamer Speicher, nicht hinaus). */
  private fun dropFile(rel: String, dir: Boolean): File {
    val ctx = context ?: throw FileNotFoundException()
    val base = dropDir().canonicalFile
    val candidate = if (rel.isEmpty()) base else File(base, rel)
    val real = uniffi.snap_ffi.checkSharedPath(candidate.path, dir, ExternalOpen.storageRoot(), ExternalOpen.ownDirs(ctx))
      ?: throw FileNotFoundException()
    val f = File(real)
    if (f != base && !f.path.startsWith(base.path + "/")) throw FileNotFoundException()
    return f
  }

  private fun mimeOf(name: String): String =
    MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
      ?: "application/octet-stream"

  // ------------------------------------------------------------ Zeilen

  private fun MatrixCursor.dir(id: DocumentId, name: String) {
    val doc = idOf(id) ?: return
    newRow().add(Document.COLUMN_DOCUMENT_ID, doc).add(Document.COLUMN_DISPLAY_NAME, name)
      .add(Document.COLUMN_MIME_TYPE, Document.MIME_TYPE_DIR).add(Document.COLUMN_FLAGS, 0)
  }

  private fun MatrixCursor.file(id: DocumentId, name: String, size: Long?, mtime: Long?) {
    val doc = idOf(id) ?: return
    newRow().add(Document.COLUMN_DOCUMENT_ID, doc).add(Document.COLUMN_DISPLAY_NAME, name)
      .add(Document.COLUMN_MIME_TYPE, mimeOf(name)).add(Document.COLUMN_FLAGS, 0)
      .add(Document.COLUMN_SIZE, size).add(Document.COLUMN_LAST_MODIFIED, mtime)
  }

  override fun queryRoots(projection: Array<out String>?): Cursor {
    val c = MatrixCursor(projection ?: ROOT_COLUMNS)
    val ctx = context ?: return c
    c.newRow()
      .add(Root.COLUMN_ROOT_ID, "snap")
      .add(Root.COLUMN_FLAGS, 0)
      .add(Root.COLUMN_TITLE, ctx.getString(R.string.app_name))
      .add(Root.COLUMN_SUMMARY, ctx.getString(R.string.documents_summary))
      .add(Root.COLUMN_DOCUMENT_ID, idOf(DocumentId.Root))
      .add(Root.COLUMN_ICON, R.mipmap.ic_launcher)
      .add(Root.COLUMN_MIME_TYPES, "*/*")
    return c
  }

  override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
    val c = MatrixCursor(projection ?: DOC_COLUMNS)
    val ctx = context ?: return c
    when (val id = parse(documentId)) {
      DocumentId.Root -> c.dir(id, ctx.getString(R.string.app_name))
      DocumentId.Devices -> c.dir(id, ctx.getString(R.string.documents_devices))
      is DocumentId.Drop -> {
        val f = dropFile(id.rel, id.rel.isEmpty() || File(dropDir(), id.rel).isDirectory)
        if (id.rel.isEmpty()) c.dir(id, ctx.getString(R.string.documents_drop))
        else if (f.isDirectory) c.dir(id, f.name) else c.file(id, f.name, f.length(), f.lastModified())
      }
      is DocumentId.Device -> c.dir(id, deviceName(id.node))
      is DocumentId.Dir -> c.dir(id, (if (id.dir.isEmpty()) id.share else id.dir).substringAfterLast('/'))
      is DocumentId.RemoteFile -> c.file(id, id.name, null, null)
    }
    return c
  }

  private fun deviceName(node: String): String {
    val devices = core("\"PairList\"").optJSONArray("devices") ?: JSONArray()
    for (i in 0 until devices.length()) {
      val d = devices.getJSONObject(i)
      if (d.optString("node_id") == node) return d.optString("name", node.take(8))
    }
    throw FileNotFoundException()
  }

  override fun queryChildDocuments(parentDocumentId: String, projection: Array<out String>?, sortOrder: String?): Cursor {
    val c = MatrixCursor(projection ?: DOC_COLUMNS)
    val ctx = context ?: return c
    when (val id = parse(parentDocumentId)) {
      DocumentId.Root -> {
        c.dir(DocumentId.Devices, ctx.getString(R.string.documents_devices))
        c.dir(DocumentId.Drop(""), ctx.getString(R.string.documents_drop))
      }
      DocumentId.Devices -> {
        val devices = core("\"PairList\"").optJSONArray("devices") ?: JSONArray()
        for (i in 0 until devices.length()) {
          val d = devices.getJSONObject(i)
          val node = d.optString("node_id")
          if (isHex(node, 64)) c.dir(DocumentId.Device(node), d.optString("name", node.take(8)))
        }
      }
      is DocumentId.Drop -> {
        val dir = try { dropFile(id.rel, true) } catch (_: FileNotFoundException) { return c }
        dir.listFiles()?.filter { !it.name.startsWith(".") }?.sortedBy { it.name.lowercase() }?.take(2000)?.forEach { f ->
          val rel = if (id.rel.isEmpty()) f.name else "${id.rel}/${f.name}"
          if (f.isDirectory) c.dir(DocumentId.Drop(rel), f.name) else if (f.isFile) c.file(DocumentId.Drop(rel), f.name, f.length(), f.lastModified())
        }
      }
      is DocumentId.Device -> remoteDir(c, id.node, null, "")
      is DocumentId.Dir -> remoteDir(c, id.node, id.share, id.dir)
      is DocumentId.RemoteFile -> {}
    }
    return c
  }

  /** Ordner eines anderen Geräts aus dem lokalen Katalog (wie in der App, höchstens 5000 Einträge). */
  private fun remoteDir(c: MatrixCursor, node: String, share: String?, dir: String) {
    var after: String? = null
    var count = 0
    do {
      val req = JSONObject().put("RemoteDir", JSONObject()
        .put("device", node).put("share", share ?: JSONObject.NULL).put("dir", dir)
        .put("after", after ?: JSONObject.NULL).put("limit", 1000))
      val page = core(req.toString())
      val dirs = page.optJSONArray("dirs") ?: JSONArray()
      for (i in 0 until dirs.length()) {
        val name = dirs.getString(i)
        // Wurzel des Geräts: Freigaben; darin Unterordner
        val next = if (share == null) DocumentId.Dir(node, name, "") else DocumentId.Dir(node, share, if (dir.isEmpty()) name else "$dir/$name")
        c.dir(next, name)
        count++
      }
      val files = page.optJSONArray("files") ?: JSONArray()
      for (i in 0 until files.length()) {
        val f = files.getJSONObject(i)
        val hash = f.optString("hash")
        val name = f.optString("name")
        if (!isHex(hash, 64)) continue
        c.file(DocumentId.RemoteFile(node, hash, name), name, f.optLong("size"), f.optLong("created_at") * 1000)
        count++
      }
      after = page.optString("next").takeIf { page.has("next") && !page.isNull("next") && it.isNotEmpty() }
    } while (after != null && count < 5000)
  }

  override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
    // Nur lesend – jede andere Anfrage wird abgewiesen
    if (mode != "r") throw FileNotFoundException("nur lesend")
    val file = when (val id = parse(documentId)) {
      is DocumentId.Drop -> dropFile(id.rel, false)
      is DocumentId.RemoteFile -> fetch(id, signal)
      else -> throw FileNotFoundException()
    }
    return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
  }

  /** Datei eines anderen Geräts in Snaps Cache laden (verschlüsselt, geprüfter Hash). */
  private fun fetch(id: DocumentId.RemoteFile, signal: CancellationSignal?): File {
    val ctx = context ?: throw FileNotFoundException()
    val base = File(ctx.cacheDir, "documents").apply { mkdirs() }
    base.listFiles()?.filter { System.currentTimeMillis() - it.lastModified() > 86_400_000L }?.forEach { it.deleteRecursively() }
    val dir = File(base, id.hash).apply { mkdirs() }
    val out = File(dir, ShareInbox.cleanName(id.name))
    if (out.isFile && out.length() > 0) return out
    signal?.throwIfCanceled()
    val req = JSONObject().put("Download", JSONObject().put("target", id.hash).put("output", out.absolutePath).put("force", true))
    val r = core(req.toString())
    val path = r.optString("path").takeIf { it.isNotEmpty() } ?: throw FileNotFoundException("Gerät nicht erreichbar")
    val f = File(path)
    if (f.canonicalPath != out.canonicalPath || !f.isFile || f.length() > MAX_FILE) {
      f.delete()
      throw FileNotFoundException()
    }
    return f
  }
}

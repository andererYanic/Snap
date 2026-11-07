package de.snap.mobile

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import com.facebook.react.bridge.Promise
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import uniffi.snap_ffi.CoreOptions
import uniffi.snap_ffi.EventListener
import uniffi.snap_ffi.SnapCore
import uniffi.snap_ffi.SnapException

/**
 * Brücke zwischen der Oberfläche (React Native) und dem eingebetteten Snap-Kern (Rust, UniFFI).
 *
 * Der Kern läuft im App-Prozess und wird einmal pro Prozess gestartet ([CoreHolder]).
 * Anfragen und Antworten sind JSON im Format des Desktop-Dienstes.
 */
class SnapCoreModule(private val ctx: ReactApplicationContext) : ReactContextBaseJavaModule(ctx) {
  private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

  override fun getName() = "SnapCore"

  /** Startet den Kern (falls nötig) und abonniert Ereignisse → `snap-event`. */
  @ReactMethod
  fun start(promise: Promise) {
    scope.launch {
      try {
        val core = CoreHolder.get(ctx.applicationContext)
        CoreHolder.listen { json ->
          // Aufruf kommt von einem Rust-Thread: niemals eine Ausnahme nach außen lassen,
          // sonst beendet die JVM den Prozess.
          try {
            if (ctx.hasActiveReactInstance()) {
              ctx.emitDeviceEvent("snap-event", json)
            }
          } catch (e: Throwable) {
            android.util.Log.w("Snap", "Ereignis nicht zustellbar: ${e.message}")
          }
        }
        promise.resolve(core.apiVersion().toInt())
      } catch (e: SnapException) {
        promise.reject("start", e.message, e)
      } catch (e: Throwable) {
        promise.reject("start", e.message ?: e.toString(), e)
      }
    }
  }

  /** Eine Anfrage an den Kern (JSON hin, JSON zurück). */
  @ReactMethod
  fun request(json: String, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(CoreHolder.get(ctx.applicationContext).request(json))
      } catch (e: Throwable) {
        promise.reject("request", e.message ?: e.toString(), e)
      }
    }
  }

  /**
   * Bericht über einen Absturz beim letzten Start (Rust-Panic, Java-Ausnahme oder Prozess beim
   * Starten des Kerns beendet). `null`, wenn alles normal war. Wird beim Lesen gelöscht.
   */
  @ReactMethod
  fun lastCrash(promise: Promise) {
    try {
      promise.resolve(CoreHolder.takeCrashReport(ctx.applicationContext))
    } catch (e: Throwable) {
      promise.resolve("Absturzbericht nicht lesbar: ${e.message}")
    }
  }

  private fun mimeOf(file: java.io.File): String =
    android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*"

  private fun contentUri(file: java.io.File): Uri =
    androidx.core.content.FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", file)

  /** Prüft einen Pfad aus der Oberfläche: absolut, ohne „..“, existierende normale Datei. */
  private fun regularFile(path: String): java.io.File? {
    val f = java.io.File(path)
    if (!f.isAbsolute || path.split('/').contains("..") || !f.isFile) return null
    return f
  }

  /** Öffnet eine Datei mit der passenden App (Android fragt ggf. nach der App). */
  @ReactMethod
  fun openFile(path: String, promise: Promise) {
    val f = regularFile(path) ?: return promise.reject("open", "Nur Dateien lassen sich öffnen.")
    try {
      val intent = Intent(Intent.ACTION_VIEW)
        .setDataAndType(contentUri(f), mimeOf(f))
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
      ctx.startActivity(intent)
      promise.resolve(null)
    } catch (e: android.content.ActivityNotFoundException) {
      promise.reject("noapp", "Keine App zum Öffnen dieses Dateityps installiert.")
    } catch (e: Throwable) {
      promise.reject("open", e.message ?: e.toString())
    }
  }

  /** Teilt Dateien über das Android-Teilen-Menü (nur lesend für die gewählte App). */
  @ReactMethod
  fun shareFiles(paths: com.facebook.react.bridge.ReadableArray, promise: Promise) {
    try {
      val files = (0 until paths.size()).mapNotNull { paths.getString(it)?.let(::regularFile) }
      if (files.isEmpty()) return promise.reject("share", "Nur Dateien lassen sich teilen.")
      val uris = ArrayList(files.map(::contentUri))
      val intent = if (uris.size == 1) {
        Intent(Intent.ACTION_SEND).setType(mimeOf(files[0])).putExtra(Intent.EXTRA_STREAM, uris[0])
      } else {
        Intent(Intent.ACTION_SEND_MULTIPLE).setType("*/*").putParcelableArrayListExtra(Intent.EXTRA_STREAM, uris)
      }
      intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      ctx.startActivity(Intent.createChooser(intent, null).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
      promise.resolve(null)
    } catch (e: Throwable) {
      promise.reject("share", e.message ?: e.toString())
    }
  }

  /** Belegter und gesamter Platz des gemeinsamen Speichers (Bytes). */
  @ReactMethod
  fun storageInfo(promise: Promise) {
    val st = android.os.StatFs(Environment.getExternalStorageDirectory().absolutePath)
    val map = com.facebook.react.bridge.Arguments.createMap()
    map.putDouble("total", st.totalBytes.toDouble())
    map.putDouble("free", st.availableBytes.toDouble())
    promise.resolve(map)
  }

  /** Schnelles Vorschaubild (Android-eigene Erzeugung, Cache). Pfad zur JPEG-Datei oder null. */
  @ReactMethod
  fun thumbnail(path: String, edge: Double, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(Thumbnails.get(ctx.applicationContext, path, edge.toInt().coerceIn(64, 1024)))
      } catch (e: Throwable) {
        promise.resolve(null)
      }
    }
  }

  /** PDF: Seitenzahl und Seitenverhältnisse (Höhe/Breite) oder null, wenn nicht lesbar. */
  @ReactMethod
  fun pdfInfo(path: String, promise: Promise) {
    scope.launch {
      try {
        val info = PdfPages.info(path) ?: return@launch promise.resolve(null)
        val map = com.facebook.react.bridge.Arguments.createMap()
        map.putInt("pages", info.first)
        val arr = com.facebook.react.bridge.Arguments.createArray()
        info.second.forEach { arr.pushDouble(it) }
        map.putArray("ratios", arr)
        promise.resolve(map)
      } catch (e: Throwable) {
        promise.resolve(null)
      }
    }
  }

  /** PDF: Seite als PNG (Pfad) in der gewünschten Breite. */
  @ReactMethod
  fun pdfPage(path: String, index: Double, width: Double, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(PdfPages.page(ctx.applicationContext, path, index.toInt(), width.toInt()))
      } catch (e: Throwable) {
        promise.resolve(null)
      }
    }
  }

  /**
   * Liest eine Textdatei für die Anzeige (höchstens `maxBytes`, UTF-8; ungültige Bytes werden
   * ersetzt). Ergebnis: `{ text, truncated }`.
   */
  @ReactMethod
  fun readText(path: String, maxBytes: Double, promise: Promise) {
    val f = regularFile(path) ?: return promise.reject("read", "Nur Dateien lassen sich anzeigen.")
    scope.launch {
      try {
        val limit = maxBytes.toLong().coerceIn(1024, 8L * 1024 * 1024)
        val buf = ByteArray(minOf(limit, f.length()).toInt())
        var n = 0
        f.inputStream().use { input ->
          while (n < buf.size) {
            val r = input.read(buf, n, buf.size - n)
            if (r < 0) break
            n += r
          }
        }
        // Nicht mitten in einem UTF-8-Zeichen abschneiden
        var end = n
        if (f.length() > n) {
          while (end > 0 && (buf[end - 1].toInt() and 0xC0) == 0x80) end--
          if (end > 0 && (buf[end - 1].toInt() and 0x80) != 0) end--
        }
        val map = com.facebook.react.bridge.Arguments.createMap()
        map.putString("text", String(buf, 0, end, Charsets.UTF_8))
        map.putBoolean("truncated", f.length() > n)
        promise.resolve(map)
      } catch (e: Throwable) {
        promise.reject("read", e.message ?: e.toString())
      }
    }
  }

  /** Ordner im App-Cache für Bilder aus Word-Dateien (wird mit dem Cache begrenzt). */
  @ReactMethod
  fun docxImageDir(promise: Promise) {
    val dir = java.io.File(ctx.cacheDir, "docx").apply { mkdirs() }
    promise.resolve(dir.absolutePath)
  }

  /** Schrift registrieren; Ergebnis: Name der eigenen Schrift oder null (Inconsolata). */
  @ReactMethod
  fun applyFont(family: String, promise: Promise) {
    try {
      // Nur eigene, harmlose Namen zulassen
      val name = if (Regex("^SnapFont(-\\d{1,6})?$").matches(family)) family else Fonts.FAMILY
      Fonts.apply(ctx.applicationContext, name)
      promise.resolve(Fonts.customName(ctx.applicationContext))
    } catch (e: Throwable) {
      promise.resolve(null)
    }
  }

  /** Eigene Schrift aus einer Datei (z. B. aus dem Dateibrowser von Snap). */
  @ReactMethod
  fun setFontFromFile(path: String, promise: Promise) {
    val f = regularFile(path) ?: return promise.reject("font", "Nur Dateien lassen sich verwenden.")
    scope.launch {
      try {
        f.inputStream().use { Fonts.import(ctx.applicationContext, it, f.name) }
        promise.resolve(Fonts.customName(ctx.applicationContext))
      } catch (e: Throwable) {
        promise.reject("font", e.message ?: "Diese Schrift lässt sich nicht verwenden.")
      }
    }
  }

  private val FONT_REQUEST = 4711
  private var fontPromise: Promise? = null
  private val fontPicker = object : com.facebook.react.bridge.BaseActivityEventListener() {
    override fun onActivityResult(activity: android.app.Activity, requestCode: Int, resultCode: Int, data: Intent?) {
      if (requestCode != FONT_REQUEST) return
      val p = fontPromise ?: return
      fontPromise = null
      val uri = data?.data
      if (resultCode != android.app.Activity.RESULT_OK || uri == null) return p.resolve(null)
      scope.launch {
        try {
          val name = ctx.contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { c -> if (c.moveToFirst()) c.getString(0) else null } ?: "Eigene Schrift"
          val input = ctx.contentResolver.openInputStream(uri) ?: throw IllegalArgumentException("Datei nicht lesbar.")
          input.use { Fonts.import(ctx.applicationContext, it, name) }
          p.resolve(Fonts.customName(ctx.applicationContext))
        } catch (e: Throwable) {
          p.reject("font", e.message ?: "Diese Schrift lässt sich nicht verwenden.")
        }
      }
    }
  }

  /** Systemdialog „Datei auswählen“ für eine TTF/OTF-Schrift (ohne Google-Dienste). */
  @ReactMethod
  fun pickFont(promise: Promise) {
    val activity = ctx.currentActivity ?: return promise.reject("font", "App ist nicht im Vordergrund.")
    if (fontPromise != null) return promise.reject("font", "Auswahl läuft bereits.")
    fontPromise = promise
    ctx.removeActivityEventListener(fontPicker)
    ctx.addActivityEventListener(fontPicker)
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
      .addCategory(Intent.CATEGORY_OPENABLE)
      .setType("*/*")
      .putExtra(Intent.EXTRA_MIME_TYPES, arrayOf("font/ttf", "font/otf", "font/sfnt", "application/x-font-ttf", "application/x-font-otf", "application/font-sfnt", "application/octet-stream"))
    try {
      activity.startActivityForResult(intent, FONT_REQUEST)
    } catch (e: Throwable) {
      fontPromise = null
      promise.reject("font", e.message ?: e.toString())
    }
  }

  /** Zurück zur Standardschrift Inconsolata. */
  @ReactMethod
  fun resetFont(promise: Promise) {
    Fonts.reset(ctx.applicationContext)
    promise.resolve(null)
  }

  /** Bildbearbeitung: Vorschau (Pfad zur JPEG im Cache). `withCrop` = Zuschnitt schon anwenden. */
  @ReactMethod
  fun editPreview(path: String, params: String, edge: Double, withCrop: Boolean, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(ImageEdit.preview(ctx.applicationContext, path, params, edge.toInt(), withCrop))
      } catch (e: Throwable) {
        promise.reject("edit", e.message ?: "Bild lässt sich nicht bearbeiten.")
      }
    }
  }

  /** Bildbearbeitung speichern: `copy` (neue Datei daneben) oder `temp` (für „Original ersetzen“). */
  @ReactMethod
  fun editSave(path: String, params: String, mode: String, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(ImageEdit.save(path, params, if (mode == "temp") "temp" else "copy"))
      } catch (e: Throwable) {
        promise.reject("edit", e.message ?: "Speichern fehlgeschlagen.")
      }
    }
  }

  /** Video: Dauer, Größe, Ausrichtung, Tonspur (JSON). */
  @androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
  @ReactMethod
  fun videoInfo(path: String, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(VideoEdit.info(path))
      } catch (e: Throwable) {
        promise.reject("video", e.message ?: "Video nicht lesbar.")
      }
    }
  }

  /** Video: Einzelbilder für die Zeitleiste (JSON-Liste von Pfaden). */
  @androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
  @ReactMethod
  fun videoFrames(path: String, count: Double, edge: Double, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(VideoEdit.frames(ctx.applicationContext, path, count.toInt(), edge.toInt()))
      } catch (e: Throwable) {
        promise.reject("video", e.message ?: "Video nicht lesbar.")
      }
    }
  }

  /** Video exportieren: `copy` (neue Datei daneben) oder `temp` (für „Original ersetzen“). */
  @androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
  @ReactMethod
  fun videoSave(path: String, params: String, mode: String, promise: Promise) {
    scope.launch {
      try {
        promise.resolve(VideoEdit.save(ctx.applicationContext, path, params, if (mode == "temp") "temp" else "copy"))
      } catch (e: Throwable) {
        promise.reject("video", e.message ?: "Speichern fehlgeschlagen.")
      }
    }
  }

  @androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
  @ReactMethod
  fun videoProgress(promise: Promise) {
    VideoEdit.progress { promise.resolve(it) }
  }

  @androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
  @ReactMethod
  fun videoCancel() {
    VideoEdit.cancel()
  }

  /** Cache-Ordner der App (für geöffnete Dateien anderer Geräte). */
  @ReactMethod
  fun cacheDir(promise: Promise) {
    promise.resolve(ctx.cacheDir.absolutePath)
  }

  /** Drucken über den Druckdialog des Systems (PDF unverändert, Bilder eingepasst). */
  @ReactMethod
  fun printFile(path: String, promise: Promise) {
    val f = regularFile(path) ?: return promise.reject("print", "Nur Dateien lassen sich drucken.")
    val activity = ctx.currentActivity ?: return promise.reject("print", "App ist nicht im Vordergrund.")
    activity.runOnUiThread {
      try {
        Printing.print(activity, f)
        promise.resolve(null)
      } catch (e: Throwable) {
        promise.reject("print", e.message ?: e.toString())
      }
    }
  }

  /** Kurze Vibration über das System (ohne Berechtigung): `select` beim Markieren, `tick` beim Erweitern. */
  @ReactMethod
  fun haptic(kind: String) {
    val view = ctx.currentActivity?.window?.decorView ?: return
    val c = when (kind) {
      "tick" -> android.view.HapticFeedbackConstants.CLOCK_TICK
      "confirm" -> if (Build.VERSION.SDK_INT >= 30) android.view.HapticFeedbackConstants.CONFIRM else android.view.HapticFeedbackConstants.VIRTUAL_KEY
      else -> android.view.HapticFeedbackConstants.LONG_PRESS
    }
    view.post { view.performHapticFeedback(c) }
  }

  /** Hat die App Zugriff auf alle Dateien (MANAGE_EXTERNAL_STORAGE)? */
  @ReactMethod
  fun hasAllFilesAccess(promise: Promise) {
    promise.resolve(Build.VERSION.SDK_INT < Build.VERSION_CODES.R || Environment.isExternalStorageManager())
  }

  /** Öffnet die Systemseite, auf der der Nutzer „Zugriff auf alle Dateien“ erlaubt. */
  @ReactMethod
  fun requestAllFilesAccess() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return
    val intent = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:${ctx.packageName}"))
    intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    try {
      ctx.startActivity(intent)
    } catch (_: Exception) {
      ctx.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }
  }

  /** Gemeinsamer Speicher (z. B. /storage/emulated/0). */
  @ReactMethod
  fun storageRoot(promise: Promise) {
    promise.resolve(Environment.getExternalStorageDirectory().absolutePath)
  }

  /** App-Einstellungen der Oberfläche (Sprache, Material You) – nur Text, lokal. */
  @ReactMethod
  fun getPref(key: String, promise: Promise) {
    promise.resolve(ctx.getSharedPreferences("snap-ui", 0).getString(key.take(64), null))
  }

  @ReactMethod
  fun setPref(key: String, value: String) {
    ctx.getSharedPreferences("snap-ui", 0).edit().putString(key.take(64), value.take(256)).apply()
  }

  /** Android-Version (für Material You ab Android 12). */
  @ReactMethod
  fun sdkInt(promise: Promise) {
    promise.resolve(Build.VERSION.SDK_INT)
  }

  // Für NativeEventEmitter erforderlich (keine Logik nötig).
  @ReactMethod fun addListener(@Suppress("UNUSED_PARAMETER") eventName: String) {}
  @ReactMethod fun removeListeners(@Suppress("UNUSED_PARAMETER") count: Double) {}
}

/** Der Kern lebt so lange wie der App-Prozess – auch für Hintergrundarbeit (WorkManager). */
object CoreHolder {
  @Volatile private var core: SnapCore? = null
  @Volatile private var listener: ((String) -> Unit)? = null
  private var subscribed = false

  private fun dataDir(context: android.content.Context) = context.filesDir.absolutePath + "/snap"

  private fun startMarker(context: android.content.Context) = java.io.File(context.filesDir, "snap-starting")

  /** Java-/Kotlin-Abstürze ebenfalls ins crash.log schreiben (danach normal weiter). */
  fun installCrashHandler(context: android.content.Context) {
    val previous = Thread.getDefaultUncaughtExceptionHandler()
    Thread.setDefaultUncaughtExceptionHandler { thread, e ->
      try {
        val dir = java.io.File(dataDir(context)).apply { mkdirs() }
        java.io.File(dir, "crash.log").appendText(
          "[${System.currentTimeMillis() / 1000}] Thread ${thread.name}: ${android.util.Log.getStackTraceString(e).take(6000)}\n",
        )
      } catch (_: Throwable) {
      }
      previous?.uncaughtException(thread, e)
    }
  }

  fun takeCrashReport(context: android.content.Context): String? {
    System.loadLibrary("snap_ffi")
    val parts = mutableListOf<String>()
    val marker = startMarker(context)
    if (marker.exists()) {
      parts += "Der Snap-Kern wurde beim letzten Start unerwartet beendet."
      marker.delete()
    }
    uniffi.snap_ffi.takeCrashLog(dataDir(context))?.let { parts += it }
    return if (parts.isEmpty()) null else parts.joinToString("\n\n")
  }

  @Synchronized
  fun get(context: android.content.Context): SnapCore {
    core?.let { return it }
    System.loadLibrary("snap_ffi")
    val storage = Environment.getExternalStorageDirectory().absolutePath
    // Marker: existiert er beim nächsten Start noch, ist der Prozess beim Starten gestorben.
    val marker = startMarker(context)
    marker.writeText(System.currentTimeMillis().toString())
    val c = SnapCore.start(
      CoreOptions(
        dataDir = dataDir(context),
        storageRoot = storage,
        deviceName = "${Build.MANUFACTURER} ${Build.MODEL}".trim(),
      ),
    )
    marker.delete()
    core = c
    return c
  }

  /** Ein Abonnement pro Prozess; der jeweils aktuelle Empfänger bekommt die Ereignisse. */
  @Synchronized
  fun listen(onEvent: (String) -> Unit) {
    listener = onEvent
    if (subscribed) return
    val c = core ?: return
    c.subscribe(object : EventListener {
      override fun onEvent(json: String) {
        try {
          listener?.invoke(json)
        } catch (e: Throwable) {
          android.util.Log.w("Snap", "Ereignis-Empfänger: ${e.message}")
        }
      }
    })
    subscribed = true
  }

}

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

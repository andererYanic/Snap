package de.snap.mobile

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Environment
import android.os.IBinder
import android.os.PowerManager
import android.util.Base64
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject

/**
 * Snap Drop empfängt, während die App geschlossen ist.
 *
 * - **Wecken** (Standard): gestartet von [DropWakeReceiver], gut 3 Minuten empfangsbereit
 *   (verlängert, solange ein Angebot offen ist oder eine Übertragung läuft), danach beendet.
 * - **Dauerhaft** (Einstellung „Auch über WLAN“): bleibt mit dauerhafter Mitteilung aktiv.
 *
 * Angebote erscheinen als Mitteilung mit „Annehmen“/„Ablehnen“ (bei geöffneter App zeigt die
 * Oberfläche sie selbst). Die Sichtbarkeit des Nutzers ändert sich dabei nicht (`DropBackground`).
 */
class DropBgService : Service() {
  companion object {
    private const val ACTION_WAKE = "de.snap.mobile.drop.BG_WAKE"
    private const val ACTION_ALWAYS = "de.snap.mobile.drop.BG_ALWAYS"
    private const val ACTION_STOP_ALWAYS = "de.snap.mobile.drop.BG_STOP_ALWAYS"
    private const val ACTION_RESPOND = "de.snap.mobile.drop.BG_RESPOND"
    private const val CH_SERVICE = DropNotify.CH_SERVICE
    private const val CH_OFFERS = DropNotify.CH_OFFERS
    private const val NOTE_SERVICE = 4100
    private const val NOTE_WAKE = 4101
    private const val WAKE_MS = 200_000L
    private const val WINDOW_WAKE_SECS = 240
    private const val WINDOW_ALWAYS_SECS = 900
    private const val TICK_MS = 15_000L

    @Volatile private var alwaysWanted = false

    private fun intent(ctx: Context, action: String) = Intent(ctx, DropBgService::class.java).setAction(action)

    fun exempt(ctx: Context) = Build.VERSION.SDK_INT < 23 ||
      (ctx.getSystemService(Context.POWER_SERVICE) as PowerManager).isIgnoringBatteryOptimizations(ctx.packageName)

    /** Vom Weck-Empfänger: Dienst starten – oder, wenn Android das nicht erlaubt, eine
     *  Mitteilung zeigen, deren Antippen den Empfang startet. */
    fun wake(ctx: Context, name: String?) {
      val i = intent(ctx, ACTION_WAKE)
      if (MainActivity.visible || exempt(ctx) || Build.VERSION.SDK_INT < 31) {
        try {
          ContextCompat.startForegroundService(ctx, i)
          return
        } catch (e: Throwable) {
          // Android erlaubt den Start gerade nicht → Mitteilung statt Dienst
          android.util.Log.i("SnapDrop", "Dienst nicht startbar: ${e.message}")
        }
      }
      ensureChannels(ctx)
      val text = if (name != null) ctx.getString(R.string.drop_wake_contact, name) else ctx.getString(R.string.drop_wake_someone)
      val tapFlags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
      val tap = if (Build.VERSION.SDK_INT >= 26) PendingIntent.getForegroundService(ctx, 1, i, tapFlags) else PendingIntent.getService(ctx, 1, i, tapFlags)
      val n = NotificationCompat.Builder(ctx, CH_OFFERS)
        .setSmallIcon(R.mipmap.ic_launcher)
        .setContentTitle(ctx.getString(R.string.drop_share))
        .setContentText(text)
        .setStyle(NotificationCompat.BigTextStyle().bigText(text))
        .setContentIntent(tap)
        .setAutoCancel(true)
        .setTimeoutAfter(120_000)
        .setPriority(NotificationCompat.PRIORITY_HIGH)
        .setCategory(NotificationCompat.CATEGORY_MESSAGE)
        .build()
      notify(ctx, NOTE_WAKE, n)
    }

    fun startAlways(ctx: Context) {
      alwaysWanted = true
      try {
        ContextCompat.startForegroundService(ctx, intent(ctx, ACTION_ALWAYS))
      } catch (e: Throwable) {
        android.util.Log.w("SnapDrop", "Dauerdienst nicht startbar: ${e.message}")
      }
    }

    fun stopAlways(ctx: Context) {
      if (!alwaysWanted) return
      alwaysWanted = false
      try {
        ctx.startService(intent(ctx, ACTION_STOP_ALWAYS))
      } catch (_: Throwable) {}
    }

    private fun notify(ctx: Context, id: Int, n: android.app.Notification) = DropNotify.notify(ctx, id, n)

    private fun ensureChannels(ctx: Context) = DropNotify.ensureChannels(ctx)
  }

  private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
  private var loop: Job? = null
  @Volatile private var always = false
  @Volatile private var wakeUntil = 0L
  private val pending = java.util.Collections.synchronizedSet(mutableSetOf<Long>())
  private val active = java.util.Collections.synchronizedSet(mutableSetOf<Long>())
  private var beacon: DropBeacon? = null
  private var lastInfo: String? = null
  private var multicast: WifiManager.MulticastLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    ensureChannels(this)
    when (intent?.action) {
      ACTION_RESPOND -> {
        DropNotify.respond(this, intent.getLongExtra("id", -1), intent.getBooleanExtra("accept", false))
        if (loop == null) stopSelf()
        return START_NOT_STICKY
      }
      ACTION_STOP_ALWAYS -> {
        always = false
        if (loop == null) {
          stopSelf()
          return START_NOT_STICKY
        }
      }
      ACTION_ALWAYS -> always = true
      null -> always = alwaysWanted // vom System neu gestartet
      else -> wakeUntil = System.currentTimeMillis() + WAKE_MS
    }
    (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).cancel(NOTE_WAKE)
    goForeground()
    if (loop == null) loop = scope.launch { run() }
    return if (always) START_STICKY else START_NOT_STICKY
  }

  private fun goForeground() {
    val open = PendingIntent.getActivity(
      this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK), PendingIntent.FLAG_IMMUTABLE,
    )
    val n = NotificationCompat.Builder(this, CH_SERVICE)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(getString(if (always) R.string.drop_bg_always else R.string.drop_bg_ready))
      .setContentText(getString(R.string.drop_bg_text))
      .setContentIntent(open)
      .setOngoing(true)
      .setPriority(NotificationCompat.PRIORITY_LOW)
      .build()
    val type = if (Build.VERSION.SDK_INT >= 29) ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE else 0
    ServiceCompat.startForeground(this, NOTE_SERVICE, n, type)
  }

  private suspend fun run() {
    // Ohne Dateizugriff startet der Kern nicht (die App fragt beim ersten Start danach)
    if (Build.VERSION.SDK_INT >= 30 && !Environment.isExternalStorageManager()) {
      finish()
      return
    }
    val core = try {
      CoreHolder.get(applicationContext)
    } catch (e: Throwable) {
      android.util.Log.w("SnapDrop", "Kern nicht startbar: ${e.message}")
      finish()
      return
    }
    DropNotify.install(applicationContext)
    CoreHolder.addListener("drop-bg") { onEvent(it) }
    var lastWindow = 0L
    while (scope.isActive) {
      val now = System.currentTimeMillis()
      if (!always && now > wakeUntil && pending.isEmpty() && active.isEmpty()) break
      // Empfangsfenster im Kern regelmäßig verlängern (es läuft von selbst ab, falls wir sterben)
      if (now - lastWindow > (if (always) 600_000L else 60_000L)) {
        val everyone = DropWake.prefs(this).getString(DropWake.K_WHO, "contacts") == "visibility"
        val secs = if (always) WINDOW_ALWAYS_SECS else WINDOW_WAKE_SECS
        request(core, """{"DropBackground":{"secs":$secs,"everyone":$everyone}}""")
        lastWindow = now
      }
      radio(core)
      delay(TICK_MS)
    }
    finish()
  }

  private suspend fun request(core: uniffi.snap_ffi.SnapCore, json: String): String? = try {
    core.request(json)
  } catch (e: Throwable) {
    android.util.Log.w("SnapDrop", "Anfrage im Hintergrund: ${e.message}")
    null
  }

  /** Multicast (für das Finden im WLAN) und Bluetooth-Werbung mit den aktuellen Daten. */
  private suspend fun radio(core: uniffi.snap_ffi.SnapCore) {
    if (multicast == null) {
      try {
        val wifi = applicationContext.getSystemService(WIFI_SERVICE) as WifiManager
        multicast = wifi.createMulticastLock("snap-drop-bg").apply {
          setReferenceCounted(false)
          acquire()
        }
      } catch (_: Throwable) {}
    }
    // Läuft die Werbung der geöffneten App, keine zweite daneben
    if (SnapDropModule.foregroundAdvertising) {
      beacon?.stop()
      beacon = null
      lastInfo = null
      return
    }
    if (!DropWake.canAdvertise(this)) return
    val info = request(core, "\"DropConnectInfo\"")?.let {
      JSONObject(it).optJSONObject("DropConnectInfo")?.optString("info")
    }?.takeIf { it.isNotEmpty() && it != "null" } ?: return
    if (info == lastInfo && beacon?.running == true) return
    val bytes = try { Base64.decode(info, Base64.DEFAULT) } catch (_: Throwable) { return }
    val b = beacon ?: DropBeacon(this).also { beacon = it }
    if (b.start(bytes)) lastInfo = info
  }

  /** Nur Buchführung (offen/laufend); Mitteilungen und Popup zeigt [DropNotify]. */
  private fun onEvent(json: String) {
    val ev = try { JSONObject(json) } catch (_: Throwable) { return }
    val id = ev.optLong("id", -1)
    when (ev.optString("type")) {
      "DropOffer" -> {
        pending += id
        wakeUntil = maxOf(wakeUntil, System.currentTimeMillis() + 130_000)
      }
      "DropOfferClosed" -> pending -= id
      "DropProgress" -> if (ev.optBoolean("incoming")) {
        pending -= id
        active += id
      }
      "DropFinished" -> if (ev.optBoolean("incoming")) {
        active -= id
        pending -= id
        wakeUntil = maxOf(wakeUntil, System.currentTimeMillis() + 20_000)
      }
    }
  }

  private suspend fun finish() {
    CoreHolder.removeListener("drop-bg")
    try {
      CoreHolder.get(applicationContext).request("""{"DropBackground":{"secs":0,"everyone":false}}""")
    } catch (_: Throwable) {}
    beacon?.stop()
    beacon = null
    try { multicast?.release() } catch (_: Throwable) {}
    multicast = null
    loop = null
    ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
    stopSelf()
  }

  override fun onDestroy() {
    beacon?.stop()
    try { multicast?.release() } catch (_: Throwable) {}
    CoreHolder.removeListener("drop-bg")
    scope.cancel()
    super.onDestroy()
  }
}

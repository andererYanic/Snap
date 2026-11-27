package de.snap.mobile

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Environment
import android.os.IBinder
import android.os.PowerManager
import android.text.format.Formatter
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
    private const val CH_SERVICE = "drop_bg"
    private const val CH_OFFERS = "drop_offers"
    private const val NOTE_SERVICE = 4100
    private const val NOTE_WAKE = 4101
    private const val NOTE_OFFER = 4200
    private const val NOTE_DONE = 4600
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

    private fun notify(ctx: Context, id: Int, n: android.app.Notification) {
      try {
        (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(id, n)
      } catch (_: SecurityException) {
        // Mitteilungen nicht erlaubt (Android 13+): still bleiben
      }
    }

    private fun ensureChannels(ctx: Context) {
      if (Build.VERSION.SDK_INT < 26) return
      val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
      nm.createNotificationChannel(
        NotificationChannel(CH_SERVICE, ctx.getString(R.string.drop_bg_channel), NotificationManager.IMPORTANCE_LOW).apply {
          setShowBadge(false)
        },
      )
      nm.createNotificationChannel(
        NotificationChannel(CH_OFFERS, ctx.getString(R.string.drop_offer_channel), NotificationManager.IMPORTANCE_HIGH).apply {
          // Auf dem Sperrbildschirm ohne Namen und Dateien
          lockscreenVisibility = android.app.Notification.VISIBILITY_PRIVATE
        },
      )
    }
  }

  private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
  private var loop: Job? = null
  @Volatile private var always = false
  @Volatile private var wakeUntil = 0L
  private val pending = java.util.Collections.synchronizedSet(mutableSetOf<Long>())
  private val active = java.util.Collections.synchronizedSet(mutableSetOf<Long>())
  private val names = java.util.concurrent.ConcurrentHashMap<Long, String>()
  private var beacon: DropBeacon? = null
  private var lastInfo: String? = null
  private var multicast: WifiManager.MulticastLock? = null
  private var lastProgress = 0L

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    ensureChannels(this)
    when (intent?.action) {
      ACTION_RESPOND -> {
        respond(intent.getLongExtra("id", -1), intent.getBooleanExtra("accept", false))
        if (loop == null) goForeground()
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

  private fun onEvent(json: String) {
    val ev = try { JSONObject(json) } catch (_: Throwable) { return }
    val id = ev.optLong("id", -1)
    when (ev.optString("type")) {
      "DropOffer" -> {
        pending += id
        wakeUntil = maxOf(wakeUntil, System.currentTimeMillis() + 130_000)
        val name = ev.optString("contact").takeIf { it.isNotEmpty() && it != "null" } ?: ev.optString("name")
        names[id] = name
        if (!MainActivity.visible) showOffer(id, name, ev)
      }
      "DropOfferClosed" -> {
        pending -= id
        cancel(NOTE_OFFER + (id % 400).toInt())
      }
      "DropProgress" -> if (ev.optBoolean("incoming")) {
        active += id
        val now = System.currentTimeMillis()
        if (now - lastProgress > 1000 && !MainActivity.visible) {
          lastProgress = now
          showProgress(id, ev.optLong("done_bytes"), ev.optLong("total_bytes"))
        }
      }
      "DropFinished" -> if (ev.optBoolean("incoming")) {
        active -= id
        pending -= id
        cancel(NOTE_OFFER + (id % 400).toInt())
        wakeUntil = maxOf(wakeUntil, System.currentTimeMillis() + 20_000)
        if (!MainActivity.visible) showFinished(id, ev)
      }
    }
  }

  private fun cancel(note: Int) {
    (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).cancel(note)
  }

  private fun respondIntent(id: Long, accept: Boolean): PendingIntent {
    val i = Intent(this, DropBgService::class.java).setAction(ACTION_RESPOND).putExtra("id", id).putExtra("accept", accept)
    return PendingIntent.getService(this, (id * 2 + if (accept) 1 else 0).toInt(), i, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
  }

  private fun openApp(): PendingIntent = PendingIntent.getActivity(
    this, 2, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK), PendingIntent.FLAG_IMMUTABLE,
  )

  private fun showOffer(id: Long, name: String, ev: JSONObject) {
    val files = ev.optInt("files")
    val what = if (files == 0) {
      getString(R.string.drop_offer_text)
    } else {
      getString(
        R.string.drop_offer_files,
        resources.getQuantityString(R.plurals.drop_files, files, files),
        Formatter.formatShortFileSize(this, ev.optLong("total")),
      )
    }
    val contact = ev.optString("contact").let { it.isNotEmpty() && it != "null" }
    val text = if (contact) what else what + "\n" + getString(R.string.drop_offer_code, ev.optString("code"))
    val public = NotificationCompat.Builder(this, CH_OFFERS)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(getString(R.string.drop_share))
      .setContentText(getString(R.string.drop_offer_locked))
      .build()
    val n = NotificationCompat.Builder(this, CH_OFFERS)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(name)
      .setContentText(what)
      .setStyle(NotificationCompat.BigTextStyle().bigText(text))
      .setContentIntent(openApp())
      .setPublicVersion(public)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setTimeoutAfter(120_000)
      .addAction(0, getString(R.string.drop_decline), respondIntent(id, false))
      .addAction(0, getString(R.string.drop_accept), respondIntent(id, true))
      .build()
    notify(this, NOTE_OFFER + (id % 400).toInt(), n)
  }

  private fun showProgress(id: Long, done: Long, total: Long) {
    val n = NotificationCompat.Builder(this, CH_SERVICE)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(names[id] ?: getString(R.string.drop_share))
      .setContentText(getString(R.string.drop_receiving))
      .setProgress(1000, if (total > 0) (done * 1000 / total).toInt() else 0, total <= 0)
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .build()
    notify(this, NOTE_OFFER + (id % 400).toInt(), n)
  }

  private fun showFinished(id: Long, ev: JSONObject) {
    val peer = ev.optString("peer")
    val b = NotificationCompat.Builder(this, CH_OFFERS)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(getString(R.string.drop_share))
      .setAutoCancel(true)
      .setContentIntent(openApp())
    if (!ev.optBoolean("ok")) {
      b.setContentText(getString(R.string.drop_failed, ev.optString("error").take(200)))
    } else {
      val text = ev.optString("text").takeIf { ev.has("text") && !ev.isNull("text") }
      val link = ev.optString("link").takeIf { ev.has("link") && !ev.isNull("link") }
      if (text != null) {
        // Text immer in die Zwischenablage (Schreiben ist auch im Hintergrund erlaubt)
        try {
          (getSystemService(CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("Snap Drop", text))
        } catch (_: Throwable) {}
        b.setContentText(getString(if (link != null) R.string.drop_link_copied else R.string.drop_text_copied, peer))
        val uri = link?.let { Uri.parse(it) }
        if (uri != null && (uri.scheme == "https" || uri.scheme == "http") && !uri.host.isNullOrEmpty()) {
          val view = Intent(Intent.ACTION_VIEW, uri).addCategory(Intent.CATEGORY_BROWSABLE).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
          b.addAction(0, getString(R.string.drop_open), PendingIntent.getActivity(this, 3, view, PendingIntent.FLAG_IMMUTABLE))
        }
      } else {
        val n = ev.optJSONArray("files")?.length() ?: 0
        b.setContentText(getString(R.string.drop_saved, resources.getQuantityString(R.plurals.drop_files, n, n), peer))
      }
    }
    notify(this, NOTE_DONE + (id % 400).toInt(), b.build())
  }

  private fun respond(id: Long, accept: Boolean) {
    if (id < 0) return
    cancel(NOTE_OFFER + (id % 400).toInt())
    scope.launch {
      try {
        val core = CoreHolder.get(applicationContext)
        core.request("""{"DropRespond":{"id":$id,"accept":$accept,"block":false}}""")
        if (accept) {
          active += id
          wakeUntil = maxOf(wakeUntil, System.currentTimeMillis() + 60_000)
        }
      } catch (e: Throwable) {
        android.util.Log.w("SnapDrop", "Antwort nicht übermittelt: ${e.message}")
      } finally {
        pending -= id
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

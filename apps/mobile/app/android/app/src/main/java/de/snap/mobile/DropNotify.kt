package de.snap.mobile

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.text.format.Formatter
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import org.json.JSONObject
import java.lang.ref.WeakReference
import java.util.concurrent.ConcurrentHashMap

/**
 * Snap-Drop-Angebote, wenn die App nicht im Vordergrund ist (geschlossen oder im Hintergrund):
 * Mitteilung mit „Annehmen“/„Ablehnen“, auf Wunsch zusätzlich als Popup über anderen Apps
 * ([DropOfferActivity], braucht „Über anderen Apps einblenden“). Einmal je Prozess installiert.
 */
object DropNotify {
  const val CH_SERVICE = "drop_bg"
  const val CH_OFFERS = "drop_offers"
  private const val NOTE_OFFER = 4200
  private const val NOTE_DONE = 4600
  const val K_POPUP = "popup"

  @Volatile private var installed = false
  private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
  private val names = ConcurrentHashMap<Long, String>()
  @Volatile private var lastProgress = 0L
  /** Gerade gezeigtes Popup (zum Schließen, wenn das Angebot wegfällt). */
  @Volatile var popup: WeakReference<DropOfferActivity>? = null

  fun install(ctx: Context) {
    if (installed) return
    installed = true
    val app = ctx.applicationContext
    ensureChannels(app)
    CoreHolder.addListener("drop-notify") { onEvent(app, it) }
  }

  fun popupWanted(ctx: Context) = DropWake.prefs(ctx).getBoolean(K_POPUP, false)

  fun canPopup(ctx: Context) = Build.VERSION.SDK_INT < 23 || Settings.canDrawOverlays(ctx)

  fun ensureChannels(ctx: Context) {
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

  fun notify(ctx: Context, id: Int, n: android.app.Notification) {
    try {
      (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(id, n)
    } catch (_: SecurityException) {
      // Mitteilungen nicht erlaubt (Android 13+): still bleiben
    }
  }

  private fun cancel(ctx: Context, note: Int) {
    (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(note)
  }

  private fun noteId(id: Long) = NOTE_OFFER + (id % 400).toInt()

  private fun onEvent(ctx: Context, json: String) {
    val ev = try { JSONObject(json) } catch (_: Throwable) { return }
    val id = ev.optLong("id", -1)
    when (ev.optString("type")) {
      "DropOffer" -> {
        val contact = ev.optString("contact").takeIf { it.isNotEmpty() && it != "null" }
        val name = contact ?: ev.optString("name")
        names[id] = name
        if (!MainActivity.visible) showOffer(ctx, id, name, contact != null, ev)
      }
      "DropOfferClosed" -> {
        cancel(ctx, noteId(id))
        closePopup(id)
      }
      "DropProgress" -> if (ev.optBoolean("incoming")) {
        val now = System.currentTimeMillis()
        if (now - lastProgress > 1000 && !MainActivity.visible) {
          lastProgress = now
          showProgress(ctx, id, ev.optLong("done_bytes"), ev.optLong("total_bytes"))
        }
      }
      "DropFinished" -> if (ev.optBoolean("incoming")) {
        cancel(ctx, noteId(id))
        closePopup(id)
        names.remove(id)
        if (!MainActivity.visible) showFinished(ctx, id, ev)
      }
    }
  }

  /** Text des Angebots: was, wie groß – und bei Fremden der Prüfcode. */
  fun describe(ctx: Context, files: Int, total: Long, contact: Boolean, code: String): Pair<String, String> {
    val what = if (files == 0) {
      ctx.getString(R.string.drop_offer_text)
    } else {
      ctx.getString(
        R.string.drop_offer_files,
        ctx.resources.getQuantityString(R.plurals.drop_files, files, files),
        Formatter.formatShortFileSize(ctx, total),
      )
    }
    val full = if (contact) what else what + "\n" + ctx.getString(R.string.drop_offer_code, code)
    return what to full
  }

  private fun respondIntent(ctx: Context, id: Long, accept: Boolean): PendingIntent {
    val i = Intent(ctx, DropActionReceiver::class.java).putExtra("id", id).putExtra("accept", accept)
    return PendingIntent.getBroadcast(ctx, (id * 2 + if (accept) 1 else 0).toInt(), i, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
  }

  private fun openApp(ctx: Context): PendingIntent = PendingIntent.getActivity(
    ctx, 2, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK), PendingIntent.FLAG_IMMUTABLE,
  )

  private fun showOffer(ctx: Context, id: Long, name: String, contact: Boolean, ev: JSONObject) {
    val files = ev.optInt("files")
    val total = ev.optLong("total")
    val code = ev.optString("code")
    val (what, text) = describe(ctx, files, total, contact, code)
    val public = NotificationCompat.Builder(ctx, CH_OFFERS)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(ctx.getString(R.string.drop_share))
      .setContentText(ctx.getString(R.string.drop_offer_locked))
      .build()
    val n = NotificationCompat.Builder(ctx, CH_OFFERS)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(name)
      .setContentText(what)
      .setStyle(NotificationCompat.BigTextStyle().bigText(text))
      .setContentIntent(openApp(ctx))
      .setPublicVersion(public)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setTimeoutAfter(120_000)
      .addAction(0, ctx.getString(R.string.drop_decline), respondIntent(ctx, id, false))
      .addAction(0, ctx.getString(R.string.drop_accept), respondIntent(ctx, id, true))
      .build()
    notify(ctx, noteId(id), n)
    // Auf Wunsch zusätzlich als Fenster über der gerade genutzten App (bzw. dem Sperrbildschirm)
    if (popupWanted(ctx) && canPopup(ctx)) {
      try {
        ctx.startActivity(
          Intent(ctx, DropOfferActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            .putExtra("id", id)
            .putExtra("name", name)
            .putExtra("text", text),
        )
      } catch (e: Throwable) {
        android.util.Log.w("SnapDrop", "Popup nicht möglich: ${e.message}")
      }
    }
  }

  private fun showProgress(ctx: Context, id: Long, done: Long, total: Long) {
    val n = NotificationCompat.Builder(ctx, CH_SERVICE)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(names[id] ?: ctx.getString(R.string.drop_share))
      .setContentText(ctx.getString(R.string.drop_receiving))
      .setProgress(1000, if (total > 0) (done * 1000 / total).toInt() else 0, total <= 0)
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .addAction(0, ctx.getString(R.string.drop_cancel), respondIntent(ctx, id, false))
      .build()
    notify(ctx, noteId(id), n)
  }

  private fun showFinished(ctx: Context, id: Long, ev: JSONObject) {
    val peer = ev.optString("peer")
    val b = NotificationCompat.Builder(ctx, CH_OFFERS)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(ctx.getString(R.string.drop_share))
      .setAutoCancel(true)
      .setContentIntent(openApp(ctx))
    if (!ev.optBoolean("ok")) {
      b.setContentText(ctx.getString(R.string.drop_failed, ev.optString("error").take(200)))
    } else {
      val text = ev.optString("text").takeIf { ev.has("text") && !ev.isNull("text") }
      val link = ev.optString("link").takeIf { ev.has("link") && !ev.isNull("link") }
      if (text != null) {
        // Text immer in die Zwischenablage (Schreiben ist auch im Hintergrund erlaubt)
        try {
          (ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("Snap Drop", text))
        } catch (_: Throwable) {}
        b.setContentText(ctx.getString(if (link != null) R.string.drop_link_copied else R.string.drop_text_copied, peer))
        val uri = link?.let { Uri.parse(it) }
        if (uri != null && (uri.scheme == "https" || uri.scheme == "http") && !uri.host.isNullOrEmpty()) {
          val view = Intent(Intent.ACTION_VIEW, uri).addCategory(Intent.CATEGORY_BROWSABLE).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
          b.addAction(0, ctx.getString(R.string.drop_open), PendingIntent.getActivity(ctx, 3, view, PendingIntent.FLAG_IMMUTABLE))
        }
      } else {
        val n = ev.optJSONArray("files")?.length() ?: 0
        b.setContentText(ctx.getString(R.string.drop_saved, ctx.resources.getQuantityString(R.plurals.drop_files, n, n), peer))
      }
    }
    notify(ctx, NOTE_DONE + (id % 400).toInt(), b.build())
  }

  private fun closePopup(id: Long) {
    val a = popup?.get() ?: return
    if (a.offerId == id) a.runOnUiThread { a.finish() }
  }

  /** Antwort aus Mitteilung oder Popup (bei laufender Übertragung: abbrechen). */
  fun respond(ctx: Context, id: Long, accept: Boolean) {
    if (id < 0) return
    cancel(ctx, noteId(id))
    closePopup(id)
    val app = ctx.applicationContext
    scope.launch {
      try {
        val core = CoreHolder.get(app)
        if (accept) {
          core.request("""{"DropRespond":{"id":$id,"accept":true,"block":false}}""")
        } else {
          // Ablehnen bzw. – läuft die Übertragung schon – abbrechen
          core.request("""{"DropCancel":{"id":$id}}""")
        }
      } catch (e: Throwable) {
        android.util.Log.w("SnapDrop", "Antwort nicht übermittelt: ${e.message}")
      }
    }
  }
}

/** Knöpfe der Mitteilung (nur von der eigenen App aufrufbar). */
class DropActionReceiver : BroadcastReceiver() {
  override fun onReceive(ctx: Context, intent: Intent) {
    DropNotify.respond(ctx, intent.getLongExtra("id", -1), intent.getBooleanExtra("accept", false))
  }
}

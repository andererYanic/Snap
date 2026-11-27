package de.snap.mobile

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.media.ToneGenerator
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat

/**
 * Gerät finden: ein eigenes Gerät (mit dem Recht „Klingeln lassen“) lässt dieses klingeln.
 * Spielt den Weckton auf dem Wecker-Kanal mit voller Lautstärke (auch im Lautlos-Modus),
 * höchstens so lange, wie der Kern vorgibt (30 s), und stellt danach die Lautstärke zurück.
 * Stoppen über die Mitteilung, in der App oder vom anderen Gerät aus.
 */
object RingAlarm {
  const val CH_RING = "find_device"
  const val ACTION_STOP = "de.snap.mobile.RING_STOP"
  private const val NOTE = 4098
  private const val MAX_SECS = 60
  private val main = Handler(Looper.getMainLooper())
  private var player: MediaPlayer? = null
  private var tone: ToneGenerator? = null
  private var savedVolume = -1
  private val timeout = Runnable { stopNow() }
  private var appCtx: Context? = null

  /** Ereignis `Ring` aus dem Kern (beliebiger Thread). */
  fun handle(ctx: Context, name: String, secs: Int) {
    val app = ctx.applicationContext
    main.post { if (secs > 0) start(app, name, secs.coerceAtMost(MAX_SECS)) else stopNow() }
  }

  private fun start(ctx: Context, name: String, secs: Int) {
    stopNow()
    appCtx = ctx
    val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    try {
      savedVolume = am.getStreamVolume(AudioManager.STREAM_ALARM)
      am.setStreamVolume(AudioManager.STREAM_ALARM, am.getStreamMaxVolume(AudioManager.STREAM_ALARM), 0)
    } catch (_: Throwable) {
      savedVolume = -1 // z. B. „Bitte nicht stören“ verbietet es: dann in der eingestellten Lautstärke
    }
    if (!play(ctx)) {
      try {
        tone = ToneGenerator(AudioManager.STREAM_ALARM, ToneGenerator.MAX_VOLUME).apply {
          startTone(ToneGenerator.TONE_CDMA_EMERGENCY_RINGBACK, secs * 1000)
        }
      } catch (_: Throwable) {}
    }
    showNote(ctx, name, secs)
    main.postDelayed(timeout, secs * 1000L)
  }

  private fun play(ctx: Context): Boolean {
    val uri = listOf(RingtoneManager.TYPE_ALARM, RingtoneManager.TYPE_RINGTONE, RingtoneManager.TYPE_NOTIFICATION)
      .firstNotNullOfOrNull { RingtoneManager.getActualDefaultRingtoneUri(ctx, it) ?: RingtoneManager.getDefaultUri(it) }
      ?: return false
    return try {
      player = MediaPlayer().apply {
        setAudioAttributes(
          AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_ALARM)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build(),
        )
        setDataSource(ctx, uri)
        isLooping = true
        prepare()
        start()
      }
      true
    } catch (e: Throwable) {
      android.util.Log.i("SnapRing", "Weckton nicht abspielbar: ${e.message}")
      player?.release()
      player = null
      false
    }
  }

  private fun showNote(ctx: Context, name: String, secs: Int) {
    if (Build.VERSION.SDK_INT >= 26) {
      val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
      nm.createNotificationChannel(
        NotificationChannel(CH_RING, ctx.getString(R.string.ring_channel), NotificationManager.IMPORTANCE_HIGH).apply {
          setSound(null, null) // den Ton spielt RingAlarm selbst
        },
      )
    }
    val stop = PendingIntent.getBroadcast(
      ctx, 7, Intent(ctx, RingStopReceiver::class.java).setAction(ACTION_STOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    val open = PendingIntent.getActivity(
      ctx, 8, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK), PendingIntent.FLAG_IMMUTABLE,
    )
    val n = NotificationCompat.Builder(ctx, CH_RING)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(ctx.getString(R.string.ring_title))
      .setContentText(ctx.getString(R.string.ring_text, name.take(64)))
      .setContentIntent(open)
      .setDeleteIntent(stop)
      .addAction(0, ctx.getString(R.string.ring_stop), stop)
      .setPriority(NotificationCompat.PRIORITY_MAX)
      .setCategory(NotificationCompat.CATEGORY_ALARM)
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setTimeoutAfter(secs * 1000L)
      .setOngoing(true)
      .build()
    DropNotify.notify(ctx, NOTE, n)
  }

  /** Sofort aufhören (Hauptthread). */
  fun stopNow() {
    main.removeCallbacks(timeout)
    try { player?.stop() } catch (_: Throwable) {}
    player?.release()
    player = null
    try { tone?.stopTone() } catch (_: Throwable) {}
    tone?.release()
    tone = null
    val ctx = appCtx ?: return
    if (savedVolume >= 0) {
      try {
        (ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager).setStreamVolume(AudioManager.STREAM_ALARM, savedVolume, 0)
      } catch (_: Throwable) {}
      savedVolume = -1
    }
    (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(NOTE)
  }
}

/** „Stopp“ in der Mitteilung (nur von der eigenen App auslösbar). */
class RingStopReceiver : BroadcastReceiver() {
  override fun onReceive(ctx: Context, intent: Intent) {
    if (intent.action == RingAlarm.ACTION_STOP) RingAlarm.handle(ctx, "", 0)
  }
}

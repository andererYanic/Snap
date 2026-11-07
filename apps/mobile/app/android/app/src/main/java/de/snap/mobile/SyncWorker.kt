package de.snap.mobile

import android.content.Context
import android.os.Build
import android.os.Environment
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import kotlinx.coroutines.delay
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * Hintergrundarbeit ohne Google-Dienste (AndroidX WorkManager, läuft lokal):
 * Abgleich mit gekoppelten Geräten, Kamera-Upload-Regeln und fällige Sicherungen.
 * Android plant den Lauf akkuschonend (etwa stündlich, nicht bei niedrigem Akku).
 */
class SyncWorker(ctx: Context, params: WorkerParameters) : CoroutineWorker(ctx, params) {
  override suspend fun doWork(): Result {
    // Ohne Dateizugriff kein Kern (die App fragt beim ersten Start danach)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !Environment.isExternalStorageManager()) {
      return Result.success()
    }
    return try {
      val core = CoreHolder.get(applicationContext)
      core.request("\"Sync\"")
      // Warten, bis anstehende Uploads erledigt sind (höchstens 8 Minuten)
      val deadline = System.currentTimeMillis() + 8 * 60_000
      while (System.currentTimeMillis() < deadline && pendingUploads(core.request("\"UploadList\"")) > 0) {
        delay(15_000)
      }
      Result.success()
    } catch (e: Exception) {
      Result.retry()
    }
  }

  private fun pendingUploads(json: String): Int {
    val items = JSONObject(json).optJSONObject("Uploads")?.optJSONArray("items") ?: return 0
    var n = 0
    for (i in 0 until items.length()) {
      val state = items.getJSONObject(i).optString("state")
      if (state == "pending") n++
    }
    return n
  }

  companion object {
    fun schedule(ctx: Context) {
      val request = PeriodicWorkRequestBuilder<SyncWorker>(1, TimeUnit.HOURS)
        .setConstraints(
          Constraints.Builder()
            .setRequiresBatteryNotLow(true)
            .setRequiredNetworkType(NetworkType.CONNECTED)
            .build(),
        )
        .build()
      WorkManager.getInstance(ctx).enqueueUniquePeriodicWork("snap-sync", ExistingPeriodicWorkPolicy.KEEP, request)
    }
  }
}

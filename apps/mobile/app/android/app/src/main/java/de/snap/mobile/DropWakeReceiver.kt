package de.snap.mobile

import android.bluetooth.le.BluetoothLeScanner
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.ParcelUuid

/**
 * Treffer der Weck-Suche (von Android zugestellt, auch wenn Snap nicht läuft) sowie Neustart
 * des Geräts bzw. App-Update (Suche neu einrichten). Prüft das Tag ohne den Rust-Kern.
 */
class DropWakeReceiver : BroadcastReceiver() {
  override fun onReceive(ctx: Context, intent: Intent) {
    try {
      when (intent.action) {
        Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED -> DropWake.sync(ctx)
        DropWake.ACTION_SCAN -> handleScan(ctx, intent)
      }
    } catch (e: Throwable) {
      android.util.Log.w("SnapDrop", "Wecken: ${e.message}")
    }
  }

  private fun handleScan(ctx: Context, intent: Intent) {
    if (!DropWake.prefs(ctx).getBoolean(DropWake.K_ENABLED, false)) return
    if (intent.getIntExtra(BluetoothLeScanner.EXTRA_ERROR_CODE, 0) != 0) return
    if (intent.getIntExtra(BluetoothLeScanner.EXTRA_CALLBACK_TYPE, ScanSettings.CALLBACK_TYPE_ALL_MATCHES) ==
      ScanSettings.CALLBACK_TYPE_MATCH_LOST
    ) return
    val results: List<ScanResult> = if (Build.VERSION.SDK_INT >= 33) {
      intent.getParcelableArrayListExtra(BluetoothLeScanner.EXTRA_LIST_SCAN_RESULT, ScanResult::class.java)
    } else {
      @Suppress("DEPRECATION")
      intent.getParcelableArrayListExtra(BluetoothLeScanner.EXTRA_LIST_SCAN_RESULT)
    } ?: return
    val uuid = ParcelUuid(DropWake.WAKE)
    for (r in results.take(20)) {
      val m = DropWake.match(ctx, r.scanRecord?.getServiceData(uuid)) ?: continue
      if (DropWake.allowWake(ctx)) DropBgService.wake(ctx, m.name)
      return
    }
  }
}

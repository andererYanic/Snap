package de.snap.mobile

import android.Manifest
import android.annotation.SuppressLint
import android.app.PendingIntent
import android.bluetooth.BluetoothManager
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelUuid
import org.json.JSONArray
import java.io.File
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.UUID
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * Snap Drop im Hintergrund: Wecken per Bluetooth LE (siehe `core/src/drop/wake.rs`).
 *
 * - Empfänger: eine Suche mit Hardware-Filter auf die Weck-UUID, deren Treffer Android als
 *   [PendingIntent] zustellt – Snap muss dafür nicht laufen. Erst ein passendes Tag (Kontakt,
 *   aktuelle Epoche ± 1) startet den kurzen Empfangsdienst.
 * - Sender: kurze, nicht verbindbare Werbung mit dem Tag des gewählten Kontakts (≤ 3 Minuten).
 * - Die Weck-Schlüssel liegen in einer privaten Datei des Kerns (0600); hier wird nur gelesen.
 * - Eingaben aus der Luft sind feindlich: feste Länge, Version, Vergleich in konstanter Zeit,
 *   Drosselung (höchstens alle 20 s, 30 je Stunde).
 */
object DropWake {
  val WAKE: UUID = UUID.fromString("f0c14dec-5cac-439c-b3db-34b731b2c067")
  const val ACTION_SCAN = "de.snap.mobile.drop.WAKE_SCAN"
  private const val VERSION: Byte = 1
  private const val FLAG_GENERIC = 1
  private const val EPOCH_SECS = 900L
  private const val MAX_KEYS = 500
  private const val MIN_GAP_MS = 20_000L
  private const val MAX_PER_HOUR = 30
  private const val PREFS = "snap_drop_background"

  const val K_ENABLED = "enabled"
  const val K_WLAN = "wlan"
  const val K_WHO = "who" // "contacts" oder "visibility"
  const val K_EVERYONE_UNTIL = "everyone_until"
  private const val K_LAST_WAKE = "last_wake"
  private const val K_WAKES = "wakes"
  private const val K_WAKES_SINCE = "wakes_since"

  fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

  fun tag(key: ByteArray, epoch: Long): ByteArray {
    val mac = Mac.getInstance("HmacSHA256")
    mac.init(SecretKeySpec(key, "HmacSHA256"))
    mac.update("snap-wake-v1".toByteArray(Charsets.US_ASCII))
    mac.update(ByteBuffer.allocate(8).putLong(epoch).array())
    return mac.doFinal().copyOf(8)
  }

  /** Gleiche Rechnung wie im Kern (fester Testvektor aus `wake.rs`). */
  fun selfTest(): Boolean = try {
    hex(tag(ByteArray(32) { 0x11 }, 1_000_000)) == "57c4dc25019c290f"
  } catch (_: Throwable) {
    false
  }

  private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }

  private fun unhex(s: String): ByteArray? {
    if (s.length != 64 || !s.all { it in '0'..'9' || it in 'a'..'f' }) return null
    return ByteArray(32) { i -> s.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
  }

  class Key(val contact: String, val name: String, val key: ByteArray)

  /** Weck-Schlüssel aus der privaten Datei des Kerns (fehlt sie, gibt es keine). */
  fun keys(ctx: Context): List<Key> = try {
    val f = File(ctx.filesDir, "snap/drop-wake.json")
    if (!f.isFile || f.length() > 512 * 1024) {
      emptyList()
    } else {
      val arr = JSONArray(f.readText())
      (0 until minOf(arr.length(), MAX_KEYS)).mapNotNull { i ->
        val o = arr.optJSONObject(i) ?: return@mapNotNull null
        val k = unhex(o.optString("key")) ?: return@mapNotNull null
        Key(o.optString("contact"), o.optString("name").take(60), k)
      }
    }
  } catch (_: Throwable) {
    emptyList()
  }

  private fun now() = System.currentTimeMillis() / 1000

  private fun payload(tag: ByteArray?): ByteArray {
    val out = ByteArray(10)
    out[0] = VERSION
    out[1] = if (tag == null) FLAG_GENERIC.toByte() else 0
    tag?.copyInto(out, 2)
    return out
  }

  /** Ergebnis einer passenden Weck-Werbung: Name des Kontakts oder `generic` (jemand sucht). */
  class Match(val name: String?, val generic: Boolean)

  fun match(ctx: Context, data: ByteArray?): Match? {
    if (data == null || data.size != 10 || data[0] != VERSION) return null
    val p = prefs(ctx)
    if ((data[1].toInt() and FLAG_GENERIC) != 0) {
      // „Jemand sucht“: nur, wenn erlaubt und ein „Alle“-Fenster noch läuft
      if (p.getString(K_WHO, "contacts") != "visibility") return null
      if (p.getLong(K_EVERYONE_UNTIL, 0) <= now()) return null
      return Match(null, true)
    }
    val t = data.copyOfRange(2, 10)
    val e = Math.floorDiv(now(), EPOCH_SECS)
    for (k in keys(ctx)) {
      for (d in -1L..1L) {
        if (MessageDigest.isEqual(tag(k.key, e + d), t)) return Match(k.name, false)
      }
    }
    return null
  }

  /** Drosselung gegen Dauerwecken (Akku): höchstens alle 20 s und 30-mal je Stunde. */
  @Synchronized
  fun allowWake(ctx: Context): Boolean {
    val p = prefs(ctx)
    val nowMs = System.currentTimeMillis()
    if (nowMs - p.getLong(K_LAST_WAKE, 0) < MIN_GAP_MS) return false
    var since = p.getLong(K_WAKES_SINCE, 0)
    var count = p.getInt(K_WAKES, 0)
    if (nowMs - since > 3_600_000) {
      since = nowMs
      count = 0
    }
    if (count >= MAX_PER_HOUR) return false
    p.edit().putLong(K_LAST_WAKE, nowMs).putLong(K_WAKES_SINCE, since).putInt(K_WAKES, count + 1).apply()
    return true
  }

  private fun granted(ctx: Context, p: String) =
    ctx.checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED

  fun canScan(ctx: Context) =
    if (Build.VERSION.SDK_INT >= 31) granted(ctx, Manifest.permission.BLUETOOTH_SCAN) else granted(ctx, Manifest.permission.ACCESS_FINE_LOCATION)

  fun canAdvertise(ctx: Context) =
    Build.VERSION.SDK_INT < 31 || granted(ctx, Manifest.permission.BLUETOOTH_ADVERTISE)

  private fun adapter(ctx: Context) = (ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager?)?.adapter

  private fun scanIntent(ctx: Context): PendingIntent {
    // Veränderlich, weil Android die Suchergebnisse als Extras anhängt.
    val flags = PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
    return PendingIntent.getBroadcast(ctx, 7, Intent(ctx, DropWakeReceiver::class.java).setAction(ACTION_SCAN), flags)
  }

  /** Suche und Dauerdienst an die Einstellungen anpassen (App-Start, Neustart, Änderung). */
  fun sync(ctx: Context) {
    val p = prefs(ctx)
    val on = p.getBoolean(K_ENABLED, false) && selfTest()
    if (on) startScan(ctx) else stopScan(ctx)
    if (on && p.getBoolean(K_WLAN, false)) DropBgService.startAlways(ctx) else DropBgService.stopAlways(ctx)
  }

  @SuppressLint("MissingPermission")
  private fun startScan(ctx: Context) {
    if (Build.VERSION.SDK_INT < 26 || !canScan(ctx)) return // Suche mit PendingIntent ab Android 8
    val a = adapter(ctx) ?: return
    if (!a.isEnabled) return
    val scanner = a.bluetoothLeScanner ?: return
    val filter = ScanFilter.Builder()
      .setServiceData(ParcelUuid(WAKE), byteArrayOf(VERSION), byteArrayOf(-1))
      .build()
    // Mit Hardware-Filter meldet Android ein Gerät nur einmal (statt bei jedem Paket).
    val callback = if (a.isOffloadedFilteringSupported) ScanSettings.CALLBACK_TYPE_FIRST_MATCH else ScanSettings.CALLBACK_TYPE_ALL_MATCHES
    val settings = ScanSettings.Builder()
      .setScanMode(ScanSettings.SCAN_MODE_LOW_POWER)
      .setCallbackType(callback)
      .build()
    try {
      scanner.startScan(listOf(filter), settings, scanIntent(ctx))
    } catch (e: Throwable) {
      if (callback != ScanSettings.CALLBACK_TYPE_ALL_MATCHES) {
        try {
          scanner.startScan(listOf(filter), ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_POWER).build(), scanIntent(ctx))
        } catch (e2: Throwable) {
          android.util.Log.w("SnapDrop", "Weck-Suche nicht startbar: ${e2.message}")
        }
      } else {
        android.util.Log.w("SnapDrop", "Weck-Suche nicht startbar: ${e.message}")
      }
    }
  }

  @SuppressLint("MissingPermission")
  private fun stopScan(ctx: Context) {
    if (Build.VERSION.SDK_INT < 26 || !canScan(ctx)) return
    try {
      adapter(ctx)?.bluetoothLeScanner?.stopScan(scanIntent(ctx))
    } catch (_: Throwable) {}
  }

  // --- Sender ---

  private var advertising: AdvertiseCallback? = null

  /** Tag des Kontakts für jetzt (null, wenn unbekannt). */
  fun tagFor(ctx: Context, contact: String): ByteArray? {
    val k = keys(ctx).firstOrNull { it.contact == contact } ?: return null
    return tag(k.key, Math.floorDiv(now(), EPOCH_SECS))
  }

  /** Kurz wecken: mit Tag (ein Kontakt) oder ohne (jemand sucht). Höchstens 3 Minuten. */
  @Synchronized
  @SuppressLint("MissingPermission")
  fun advertise(ctx: Context, tag: ByteArray?, seconds: Int): Boolean {
    stopAdvertise(ctx)
    if (!canAdvertise(ctx)) return false
    val a = adapter(ctx) ?: return false
    if (!a.isEnabled) return false
    val adv = a.bluetoothLeAdvertiser ?: return false
    val settings = AdvertiseSettings.Builder()
      .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
      .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
      .setConnectable(false)
      .setTimeout(seconds.coerceIn(5, 180) * 1000)
      .build()
    // Nur Weck-UUID und Tag – kein Name, keine feste Kennung.
    val data = AdvertiseData.Builder().setIncludeDeviceName(false).setIncludeTxPowerLevel(false)
      .addServiceData(ParcelUuid(WAKE), payload(tag)).build()
    val cb = object : AdvertiseCallback() {
      override fun onStartFailure(errorCode: Int) {
        android.util.Log.w("SnapDrop", "Weck-Werbung fehlgeschlagen ($errorCode)")
      }
    }
    return try {
      adv.startAdvertising(settings, data, cb)
      advertising = cb
      true
    } catch (e: Throwable) {
      false
    }
  }

  @Synchronized
  @SuppressLint("MissingPermission")
  fun stopAdvertise(ctx: Context) {
    val cb = advertising ?: return
    advertising = null
    try {
      adapter(ctx)?.bluetoothLeAdvertiser?.stopAdvertising(cb)
    } catch (_: Throwable) {}
  }
}

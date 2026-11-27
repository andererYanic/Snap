package de.snap.mobile

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.net.wifi.WifiNetworkSpecifier
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.util.Base64
import com.facebook.react.bridge.Arguments
import com.facebook.react.bridge.Promise
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod
import org.json.JSONObject
import java.util.UUID

/**
 * Snap Drop auf Android: Bluetooth LE zum Finden (Verbindungsdaten per GATT), Multicast-Sperre
 * für mDNS und der Offline-Weg über einen kurzzeitigen LocalOnlyHotspot.
 *
 * Sicherheit:
 * - Bluetooth wird nie gekoppelt. Über Bluetooth gehen nur Verbindungsdaten und die Kurzanfrage;
 *   beides prüft der Kern streng (Fuzz-Ziel `drop_connect`). Danach läuft alles
 *   Ende-zu-Ende-verschlüsselt über QUIC.
 * - Die Werbung enthält nur die Dienst-UUID – keinen Gerätenamen, keine feste Kennung.
 * - Werte sind auf [MAX_VALUE] Byte begrenzt, Anfragen je Gerät gedrosselt.
 * - Ereignisse gehen als JSON an die Oberfläche (`snap-drop-ble`), nie Ausnahmen nach außen.
 */
@SuppressLint("MissingPermission") // Rechte fragt die Oberfläche vor jedem Aufruf ab.
class SnapDropModule(private val ctx: ReactApplicationContext) : ReactContextBaseJavaModule(ctx) {
  companion object {
    val SERVICE: UUID = UUID.fromString("f0c14dec-5cac-439c-b3db-34b731b2c064")
    val INFO: UUID = UUID.fromString("f0c14dec-5cac-439c-b3db-34b731b2c065")
    val REQUEST: UUID = UUID.fromString("f0c14dec-5cac-439c-b3db-34b731b2c066")
    val CCCD: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
    const val MAX_VALUE = 1024
    const val EVENT = "snap-drop-ble"
    const val GATT_TIMEOUT_MS = 12_000L
    const val REREAD_MS = 30_000L
    /** Werbung der geöffneten App läuft (der Hintergrunddienst startet dann keine zweite). */
    @Volatile var foregroundAdvertising = false
  }

  private val main = Handler(Looper.getMainLooper())
  private val bt get() = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager?
  private val wifi get() = ctx.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
  private val conn get() = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

  override fun getName() = "SnapDrop"

  private fun emit(type: String, device: String?, data: ByteArray? = null, extra: String? = null) {
    try {
      val o = JSONObject().put("type", type)
      device?.let { o.put("device", it) }
      data?.let { o.put("data", Base64.encodeToString(it, Base64.NO_WRAP)) }
      extra?.let { o.put("message", it) }
      if (ctx.hasActiveReactInstance()) ctx.emitDeviceEvent(EVENT, o.toString())
    } catch (e: Throwable) {
      android.util.Log.w("SnapDrop", "Ereignis nicht zustellbar: ${e.message}")
    }
  }

  private fun b64(s: String): ByteArray? =
    try { Base64.decode(s, Base64.DEFAULT).takeIf { it.size <= MAX_VALUE } } catch (e: IllegalArgumentException) { null }

  @ReactMethod
  fun addListener(eventName: String) {}

  @ReactMethod
  fun removeListeners(count: Int) {}

  /** Was das Gerät kann: Bluetooth LE (an/aus), Werbung, Hotspot-Beitritt (ab Android 10). */
  @ReactMethod
  fun capabilities(promise: Promise) {
    val adapter = bt?.adapter
    val m = Arguments.createMap()
    m.putBoolean("ble", adapter != null)
    m.putBoolean("bleOn", adapter?.isEnabled == true)
    m.putBoolean("advertise", adapter?.bluetoothLeAdvertiser != null)
    m.putBoolean("joinHotspot", Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)
    m.putInt("sdk", Build.VERSION.SDK_INT)
    promise.resolve(m)
  }

  // --- Multicast (mDNS) ---

  private var multicast: WifiManager.MulticastLock? = null

  @ReactMethod
  fun multicast(on: Boolean) {
    try {
      if (on && multicast == null) {
        multicast = wifi.createMulticastLock("snap-drop").apply { setReferenceCounted(false); acquire() }
      } else if (!on) {
        multicast?.release()
        multicast = null
      }
    } catch (e: Throwable) {
      android.util.Log.w("SnapDrop", "Multicast: ${e.message}")
    }
  }

  // --- Sichtbar sein: Werbung + GATT-Server ---

  @Volatile private var info: ByteArray = ByteArray(0)
  private var server: BluetoothGattServer? = null
  private var advertising: AdvertiseCallback? = null
  private val subscribed = mutableSetOf<BluetoothDevice>()
  private val writeBuf = mutableMapOf<String, ByteArray>()
  private val lastRequest = mutableMapOf<String, Long>()

  private val serverCallback = object : BluetoothGattServerCallback() {
    override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
      if (newState == BluetoothProfile.STATE_DISCONNECTED) {
        synchronized(subscribed) { subscribed.remove(device) }
        synchronized(writeBuf) { writeBuf.remove(device.address) }
      }
    }

    override fun onCharacteristicReadRequest(device: BluetoothDevice, requestId: Int, offset: Int, ch: BluetoothGattCharacteristic) {
      val s = server ?: return
      if (ch.uuid != INFO) { s.sendResponse(device, requestId, BluetoothGatt.GATT_READ_NOT_PERMITTED, offset, null); return }
      val v = info
      if (v.isEmpty()) { s.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, offset, null); return }
      if (offset > v.size) { s.sendResponse(device, requestId, BluetoothGatt.GATT_INVALID_OFFSET, offset, null); return }
      s.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, v.copyOfRange(offset, v.size))
    }

    override fun onCharacteristicWriteRequest(
      device: BluetoothDevice, requestId: Int, ch: BluetoothGattCharacteristic,
      preparedWrite: Boolean, responseNeeded: Boolean, offset: Int, value: ByteArray?,
    ) {
      val s = server ?: return
      val bytes = value ?: ByteArray(0)
      val ok = ch.uuid == REQUEST && offset >= 0 && offset + bytes.size <= MAX_VALUE
      if (ok) {
        synchronized(writeBuf) {
          val old = if (offset == 0) ByteArray(0) else writeBuf[device.address] ?: ByteArray(0)
          if (offset == old.size) writeBuf[device.address] = old + bytes
        }
        if (!preparedWrite) finishWrite(device)
      }
      if (responseNeeded) {
        s.sendResponse(device, requestId, if (ok) BluetoothGatt.GATT_SUCCESS else BluetoothGatt.GATT_WRITE_NOT_PERMITTED, offset, null)
      }
    }

    override fun onExecuteWrite(device: BluetoothDevice, requestId: Int, execute: Boolean) {
      if (execute) finishWrite(device) else synchronized(writeBuf) { writeBuf.remove(device.address) }
      server?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, null)
    }

    override fun onDescriptorWriteRequest(
      device: BluetoothDevice, requestId: Int, d: BluetoothGattDescriptor,
      preparedWrite: Boolean, responseNeeded: Boolean, offset: Int, value: ByteArray?,
    ) {
      if (d.uuid == CCCD) {
        val on = value?.contentEquals(BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE) == true
        synchronized(subscribed) { if (on) subscribed.add(device) else subscribed.remove(device) }
      }
      if (responseNeeded) server?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, null)
    }
  }

  /** Kurzanfrage vollständig: gedrosselt (höchstens eine je Gerät alle 5 s) an die Oberfläche. */
  private fun finishWrite(device: BluetoothDevice) {
    val data = synchronized(writeBuf) { writeBuf.remove(device.address) } ?: return
    val now = System.currentTimeMillis()
    val last = synchronized(lastRequest) { lastRequest.put(device.address, now) ?: 0L }
    if (now - last < 5_000 || data.isEmpty()) return
    emit("hotspot", device.address, data)
  }

  /** Startet Werbung und GATT-Server mit den Verbindungsdaten (Base64 aus dem Kern). */
  @ReactMethod
  fun advertise(infoB64: String, promise: Promise) {
    val adapter = bt?.adapter ?: return promise.reject("ble", "Kein Bluetooth")
    if (!adapter.isEnabled) return promise.reject("ble-off", "Bluetooth ist aus")
    val data = b64(infoB64) ?: return promise.reject("data", "Ungültige Verbindungsdaten")
    info = data
    try {
      if (server == null) {
        val s = bt!!.openGattServer(ctx, serverCallback) ?: return promise.reject("ble", "GATT-Server nicht verfügbar")
        val svc = BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY)
        val infoCh = BluetoothGattCharacteristic(
          INFO,
          BluetoothGattCharacteristic.PROPERTY_READ or BluetoothGattCharacteristic.PROPERTY_NOTIFY,
          BluetoothGattCharacteristic.PERMISSION_READ,
        )
        infoCh.addDescriptor(BluetoothGattDescriptor(CCCD, BluetoothGattDescriptor.PERMISSION_READ or BluetoothGattDescriptor.PERMISSION_WRITE))
        svc.addCharacteristic(infoCh)
        svc.addCharacteristic(BluetoothGattCharacteristic(REQUEST, BluetoothGattCharacteristic.PROPERTY_WRITE, BluetoothGattCharacteristic.PERMISSION_WRITE))
        s.addService(svc)
        server = s
      }
      if (advertising == null) {
        val adv = adapter.bluetoothLeAdvertiser ?: return promise.reject("ble", "Bluetooth-Werbung wird nicht unterstützt")
        val cb = object : AdvertiseCallback() {
          override fun onStartFailure(errorCode: Int) {
            advertising = null
            emit("error", null, extra = "Bluetooth-Werbung fehlgeschlagen ($errorCode)")
          }
        }
        val settings = AdvertiseSettings.Builder()
          .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_BALANCED)
          .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
          .setConnectable(true)
          .build()
        // Bewusst ohne Gerätenamen und ohne Herstellerdaten: nur die Dienst-UUID.
        val ad = AdvertiseData.Builder().setIncludeDeviceName(false).setIncludeTxPowerLevel(false)
          .addServiceUuid(ParcelUuid(SERVICE)).build()
        adv.startAdvertising(settings, ad, cb)
        advertising = cb
      }
      foregroundAdvertising = true
      promise.resolve(true)
    } catch (e: Throwable) {
      promise.reject("ble", e.message ?: e.toString())
    }
  }

  /** Neue Verbindungsdaten (z. B. nach dem Hotspot-Beitritt); verbundene Sender werden benachrichtigt. */
  @ReactMethod
  fun updateInfo(infoB64: String) {
    val data = b64(infoB64) ?: return
    info = data
    val s = server ?: return
    val ch = s.getService(SERVICE)?.getCharacteristic(INFO) ?: return
    val devices = synchronized(subscribed) { subscribed.toList() }
    for (d in devices) {
      try {
        // Nur ein Byte als Signal; der Sender liest die Daten danach vollständig.
        if (Build.VERSION.SDK_INT >= 33) {
          s.notifyCharacteristicChanged(d, ch, false, byteArrayOf(1))
        } else {
          @Suppress("DEPRECATION")
          ch.value = byteArrayOf(1)
          @Suppress("DEPRECATION")
          s.notifyCharacteristicChanged(d, ch, false)
        }
      } catch (e: Throwable) {
        android.util.Log.w("SnapDrop", "Benachrichtigung: ${e.message}")
      }
    }
  }

  @ReactMethod
  fun stopAdvertise() {
    foregroundAdvertising = false
    try {
      advertising?.let { bt?.adapter?.bluetoothLeAdvertiser?.stopAdvertising(it) }
    } catch (_: Throwable) {}
    advertising = null
    try { server?.close() } catch (_: Throwable) {}
    server = null
    info = ByteArray(0)
    synchronized(subscribed) { subscribed.clear() }
  }

  // --- Im Hintergrund empfangen (siehe DropWake, DropBgService) ---

  /** Einstellungen übernehmen: Weck-Suche, Dauerdienst über WLAN, wer wecken darf. */
  @ReactMethod
  fun setBackground(enabled: Boolean, wlan: Boolean, who: String) {
    DropWake.prefs(ctx).edit()
      .putBoolean(DropWake.K_ENABLED, enabled)
      .putBoolean(DropWake.K_WLAN, wlan)
      .putString(DropWake.K_WHO, if (who == "visibility") "visibility" else "contacts")
      .apply()
    DropWake.sync(ctx.applicationContext)
  }

  /** Ende eines „Alle“-Fensters (Unix-Sekunden, 0 = keins) – für das Wecken „wie Sichtbarkeit“. */
  @ReactMethod
  fun setEveryoneUntil(until: Double) {
    DropWake.prefs(ctx).edit().putLong(DropWake.K_EVERYONE_UNTIL, until.toLong()).apply()
  }

  @ReactMethod
  fun backgroundState(promise: Promise) {
    val p = DropWake.prefs(ctx)
    val m = com.facebook.react.bridge.Arguments.createMap()
    m.putBoolean("enabled", p.getBoolean(DropWake.K_ENABLED, false))
    m.putBoolean("wlan", p.getBoolean(DropWake.K_WLAN, false))
    m.putString("who", p.getString(DropWake.K_WHO, "contacts"))
    m.putBoolean("batteryExempt", DropBgService.exempt(ctx))
    m.putBoolean("supported", Build.VERSION.SDK_INT >= 26 && DropWake.selfTest())
    promise.resolve(m)
  }

  /** Systemdialog „Akku-Optimierung aufheben“ (nötig, damit das Wecken den Dienst starten darf). */
  @ReactMethod
  fun requestBatteryExempt() {
    if (Build.VERSION.SDK_INT < 23 || DropBgService.exempt(ctx)) return
    try {
      val i = Intent(android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, android.net.Uri.parse("package:${ctx.packageName}"))
      ctx.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: Throwable) {}
  }

  /** Einen Kontakt wecken (Tag aus seinem Weck-Schlüssel). `false`, wenn nicht möglich. */
  @ReactMethod
  fun wake(contact: String, seconds: Int, promise: Promise) {
    val tag = DropWake.tagFor(ctx, contact) ?: return promise.resolve(false)
    promise.resolve(DropWake.advertise(ctx, tag, seconds))
  }

  /** „Jemand sucht“ – weckt nur Geräte mit laufendem „Alle“-Fenster, die das erlauben. */
  @ReactMethod
  fun wakeAll(seconds: Int, promise: Promise) {
    promise.resolve(DropWake.advertise(ctx, null, seconds))
  }

  @ReactMethod
  fun stopWake() {
    DropWake.stopAdvertise(ctx)
  }

  // --- Suchen: Scan + Verbindungsdaten lesen ---

  private var scanning: ScanCallback? = null
  private val lastRead = mutableMapOf<String, Long>()
  private val reading = mutableSetOf<String>()

  @ReactMethod
  fun scan(on: Boolean, promise: Promise) {
    val scanner = bt?.adapter?.takeIf { it.isEnabled }?.bluetoothLeScanner
    try {
      if (!on) {
        scanning?.let { scanner?.stopScan(it) }
        scanning = null
        return promise.resolve(false)
      }
      if (scanner == null) return promise.reject("ble-off", "Bluetooth ist aus")
      if (scanning != null) return promise.resolve(true)
      val cb = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) = found(result.device)
        override fun onBatchScanResults(results: MutableList<ScanResult>) = results.forEach { found(it.device) }
        override fun onScanFailed(errorCode: Int) {
          scanning = null
          emit("error", null, extra = "Bluetooth-Suche fehlgeschlagen ($errorCode)")
        }
      }
      val filter = ScanFilter.Builder().setServiceUuid(ParcelUuid(SERVICE)).build()
      val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build()
      scanner.startScan(listOf(filter), settings, cb)
      scanning = cb
      promise.resolve(true)
    } catch (e: Throwable) {
      promise.reject("ble", e.message ?: e.toString())
    }
  }

  /** Liest die Verbindungsdaten eines gefundenen Geräts (höchstens zwei gleichzeitig, alle 30 s neu). */
  private fun found(device: BluetoothDevice) {
    val now = System.currentTimeMillis()
    synchronized(reading) {
      if (device.address in reading || reading.size >= 2) return
      if (now - (lastRead[device.address] ?: 0L) < REREAD_MS) return
      reading.add(device.address)
      lastRead[device.address] = now
    }
    GattLink(device, onInfo = { data ->
      emit("peer", device.address, data)
    }, onDone = {
      synchronized(reading) { reading.remove(device.address) }
    }).start(readOnly = true)
  }

  // --- Offline-Weg (Sender): Kurzanfrage schreiben, auf „bereit“ warten ---

  private val links = mutableMapOf<String, GattLink>()

  /** Schreibt die Kurzanfrage an ein Gerät und meldet `ready` mit dessen neuen Verbindungsdaten. */
  @ReactMethod
  fun sendRequest(address: String, reqB64: String, promise: Promise) {
    val data = b64(reqB64) ?: return promise.reject("data", "Ungültige Anfrage")
    val adapter = bt?.adapter ?: return promise.reject("ble", "Kein Bluetooth")
    val device = try { adapter.getRemoteDevice(address) } catch (e: IllegalArgumentException) { return promise.reject("data", "Ungültige Adresse") }
    synchronized(links) { links.remove(address)?.close() }
    val link = GattLink(device, onInfo = { info -> emit("ready", address, info) }, onDone = {
      synchronized(links) { links.remove(address) }
    }, request = data, keepMs = 180_000L)
    synchronized(links) { links[address] = link }
    link.start(readOnly = false)
    promise.resolve(true)
  }

  @ReactMethod
  fun closeLink(address: String) {
    synchronized(links) { links.remove(address)?.close() }
  }

  /**
   * Eine GATT-Verbindung als Client: MTU erhöhen, Dienste finden, dann entweder nur
   * Verbindungsdaten lesen oder (Offline-Weg) Benachrichtigungen einschalten, die Kurzanfrage
   * schreiben und nach jedem Signal die Verbindungsdaten neu lesen.
   */
  private inner class GattLink(
    private val device: BluetoothDevice,
    private val onInfo: (ByteArray) -> Unit,
    private val onDone: () -> Unit,
    private val request: ByteArray? = null,
    private val keepMs: Long = GATT_TIMEOUT_MS,
  ) {
    private var gatt: BluetoothGatt? = null
    private var readOnly = true
    @Volatile private var closed = false
    private val timeout = Runnable { close() }

    fun start(readOnly: Boolean) {
      this.readOnly = readOnly
      main.post {
        try {
          gatt = device.connectGatt(ctx, false, cb, BluetoothDevice.TRANSPORT_LE)
          main.postDelayed(timeout, keepMs)
        } catch (e: Throwable) {
          close()
        }
      }
    }

    fun close() {
      if (closed) return
      closed = true
      main.removeCallbacks(timeout)
      try { gatt?.disconnect(); gatt?.close() } catch (_: Throwable) {}
      onDone()
    }

    private fun infoChar(g: BluetoothGatt) = g.getService(SERVICE)?.getCharacteristic(INFO)

    private fun handleValue(value: ByteArray?) {
      val v = value ?: return
      if (v.isEmpty() || v.size > MAX_VALUE) return
      onInfo(v)
      if (readOnly) close()
    }

    private val cb = object : BluetoothGattCallback() {
      override fun onConnectionStateChange(g: BluetoothGatt, status: Int, newState: Int) {
        if (newState == BluetoothProfile.STATE_CONNECTED && status == BluetoothGatt.GATT_SUCCESS) {
          if (!g.requestMtu(247)) g.discoverServices()
        } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
          close()
        }
      }

      override fun onMtuChanged(g: BluetoothGatt, mtu: Int, status: Int) {
        g.discoverServices()
      }

      override fun onServicesDiscovered(g: BluetoothGatt, status: Int) {
        val ch = infoChar(g) ?: return close()
        if (readOnly) {
          g.readCharacteristic(ch)
          return
        }
        g.setCharacteristicNotification(ch, true)
        val d = ch.getDescriptor(CCCD) ?: return close()
        if (Build.VERSION.SDK_INT >= 33) {
          g.writeDescriptor(d, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE)
        } else {
          @Suppress("DEPRECATION")
          d.value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
          @Suppress("DEPRECATION")
          g.writeDescriptor(d)
        }
      }

      override fun onDescriptorWrite(g: BluetoothGatt, d: BluetoothGattDescriptor, status: Int) {
        val req = request ?: return
        val ch = g.getService(SERVICE)?.getCharacteristic(REQUEST) ?: return close()
        if (Build.VERSION.SDK_INT >= 33) {
          g.writeCharacteristic(ch, req, BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT)
        } else {
          @Suppress("DEPRECATION")
          ch.writeType = BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT
          @Suppress("DEPRECATION")
          ch.value = req
          @Suppress("DEPRECATION")
          g.writeCharacteristic(ch)
        }
      }

      override fun onCharacteristicWrite(g: BluetoothGatt, ch: BluetoothGattCharacteristic, status: Int) {
        if (status != BluetoothGatt.GATT_SUCCESS) {
          emit("error", device.address, extra = "Anfrage nicht zugestellt ($status)")
          close()
        }
      }

      // Signal „Daten geändert“ → vollständig neu lesen (lange Werte passen nicht in eine Benachrichtigung).
      override fun onCharacteristicChanged(g: BluetoothGatt, ch: BluetoothGattCharacteristic, value: ByteArray) {
        if (ch.uuid == INFO) g.readCharacteristic(ch)
      }

      @Deprecated("Android < 13")
      override fun onCharacteristicChanged(g: BluetoothGatt, ch: BluetoothGattCharacteristic) {
        if (ch.uuid == INFO) g.readCharacteristic(ch)
      }

      override fun onCharacteristicRead(g: BluetoothGatt, ch: BluetoothGattCharacteristic, value: ByteArray, status: Int) {
        if (status == BluetoothGatt.GATT_SUCCESS && ch.uuid == INFO) handleValue(value) else if (readOnly) close()
      }

      @Deprecated("Android < 13")
      override fun onCharacteristicRead(g: BluetoothGatt, ch: BluetoothGattCharacteristic, status: Int) {
        @Suppress("DEPRECATION")
        if (status == BluetoothGatt.GATT_SUCCESS && ch.uuid == INFO) handleValue(ch.value) else if (readOnly) close()
      }
    }
  }

  // --- Offline-Weg: Hotspot öffnen (Sender) und beitreten (Empfänger) ---

  private var reservation: WifiManager.LocalOnlyHotspotReservation? = null

  /** Öffnet einen LocalOnlyHotspot (Zufalls-SSID und -Passwort vom System). */
  @ReactMethod
  fun startHotspot(promise: Promise) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return promise.reject("old", "Erst ab Android 8")
    reservation?.let { r -> return resolveHotspot(r, promise) }
    try {
      wifi.startLocalOnlyHotspot(object : WifiManager.LocalOnlyHotspotCallback() {
        override fun onStarted(r: WifiManager.LocalOnlyHotspotReservation) {
          reservation = r
          resolveHotspot(r, promise)
          // Spätestens nach 10 Minuten schließen.
          main.postDelayed({ stopHotspot() }, 600_000L)
        }
        override fun onStopped() { reservation = null }
        override fun onFailed(reason: Int) {
          promise.reject("hotspot", "Hotspot nicht möglich ($reason) – ist WLAN-Tethering aktiv?")
        }
      }, main)
    } catch (e: SecurityException) {
      promise.reject("permission", "Berechtigung für Geräte in der Nähe fehlt")
    } catch (e: Throwable) {
      promise.reject("hotspot", e.message ?: e.toString())
    }
  }

  private fun resolveHotspot(r: WifiManager.LocalOnlyHotspotReservation, promise: Promise) {
    val m = Arguments.createMap()
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val c = r.softApConfiguration
      @Suppress("DEPRECATION")
      m.putString("ssid", c.ssid?.trim('"'))
      m.putString("passphrase", c.passphrase)
    } else {
      @Suppress("DEPRECATION")
      val c = r.wifiConfiguration
      @Suppress("DEPRECATION")
      m.putString("ssid", c?.SSID?.trim('"'))
      @Suppress("DEPRECATION")
      m.putString("passphrase", c?.preSharedKey?.trim('"'))
    }
    promise.resolve(m)
  }

  @ReactMethod
  fun stopHotspot() {
    try { reservation?.close() } catch (_: Throwable) {}
    reservation = null
  }

  private var joined: ConnectivityManager.NetworkCallback? = null

  /** Tritt einem Hotspot bei (Android fragt selbst nach) und bindet den Prozess an dieses Netz. */
  @ReactMethod
  fun joinHotspot(ssid: String, passphrase: String, promise: Promise) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return promise.reject("old", "Offline-Empfang erst ab Android 10")
    if (ssid.isEmpty() || ssid.length > 32 || passphrase.length !in 8..63) return promise.reject("data", "Ungültige Hotspot-Daten")
    leaveHotspot()
    try {
      val spec = WifiNetworkSpecifier.Builder().setSsid(ssid).setWpa2Passphrase(passphrase).build()
      val req = NetworkRequest.Builder()
        .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
        .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        .setNetworkSpecifier(spec)
        .build()
      var done = false
      val cb = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
          // Neue Sockets (Drop-Sitzung wird danach neu gestartet) laufen über den Hotspot.
          conn.bindProcessToNetwork(network)
          if (!done) { done = true; promise.resolve(true) }
        }
        override fun onUnavailable() {
          if (!done) { done = true; promise.reject("join", "Verbindung abgelehnt oder nicht möglich") }
        }
        override fun onLost(network: Network) {
          conn.bindProcessToNetwork(null)
          emit("hotspot-lost", null)
        }
      }
      joined = cb
      conn.requestNetwork(req, cb, 60_000)
    } catch (e: Throwable) {
      promise.reject("join", e.message ?: e.toString())
    }
  }

  @ReactMethod
  fun leaveHotspot() {
    try {
      conn.bindProcessToNetwork(null)
      joined?.let { conn.unregisterNetworkCallback(it) }
    } catch (_: Throwable) {}
    joined = null
  }

  override fun invalidate() {
    stopAdvertise()
    try { scanning?.let { bt?.adapter?.bluetoothLeScanner?.stopScan(it) } } catch (_: Throwable) {}
    scanning = null
    synchronized(links) { links.values.toList() }.forEach { it.close() }
    stopHotspot()
    leaveHotspot()
    multicast(false)
    super.invalidate()
  }
}

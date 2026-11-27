package de.snap.mobile

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.content.Context
import android.os.ParcelUuid

/**
 * Bluetooth-Anteil des Empfangs im Hintergrund: Werbung mit der Drop-Dienst-UUID und ein
 * GATT-Server, der nur die Verbindungsdaten zum Lesen anbietet (gleiches Format wie
 * [SnapDropModule]). Kein Offline-Hotspot im Hintergrund – dafür braucht es die App.
 */
@SuppressLint("MissingPermission") // Aufrufer prüft die Rechte
class DropBeacon(private val ctx: Context) {
  private val bt get() = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager?
  private var server: BluetoothGattServer? = null
  private var advertising: AdvertiseCallback? = null
  @Volatile private var info = ByteArray(0)

  val running get() = server != null

  private val callback = object : BluetoothGattServerCallback() {
    override fun onCharacteristicReadRequest(device: BluetoothDevice, requestId: Int, offset: Int, ch: BluetoothGattCharacteristic) {
      val s = server ?: return
      val v = info
      when {
        ch.uuid != SnapDropModule.INFO -> s.sendResponse(device, requestId, BluetoothGatt.GATT_READ_NOT_PERMITTED, offset, null)
        v.isEmpty() -> s.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, offset, null)
        offset < 0 || offset > v.size -> s.sendResponse(device, requestId, BluetoothGatt.GATT_INVALID_OFFSET, offset, null)
        else -> s.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, v.copyOfRange(offset, v.size))
      }
    }

    override fun onDescriptorWriteRequest(
      device: BluetoothDevice, requestId: Int, descriptor: BluetoothGattDescriptor,
      preparedWrite: Boolean, responseNeeded: Boolean, offset: Int, value: ByteArray?,
    ) {
      // Benachrichtigungen werden angenommen (Sender abonnieren sie), aber nie gesendet.
      if (responseNeeded) server?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, null)
    }

    override fun onCharacteristicWriteRequest(
      device: BluetoothDevice, requestId: Int, ch: BluetoothGattCharacteristic,
      preparedWrite: Boolean, responseNeeded: Boolean, offset: Int, value: ByteArray?,
    ) {
      if (responseNeeded) server?.sendResponse(device, requestId, BluetoothGatt.GATT_WRITE_NOT_PERMITTED, offset, null)
    }
  }

  /** Startet bzw. aktualisiert die Werbung mit neuen Verbindungsdaten. */
  fun start(data: ByteArray): Boolean {
    if (data.isEmpty() || data.size > SnapDropModule.MAX_VALUE) return false
    info = data
    val manager = bt ?: return false
    val adapter = manager.adapter ?: return false
    if (!adapter.isEnabled) return false
    return try {
      if (server == null) {
        val s = manager.openGattServer(ctx, callback) ?: return false
        val svc = BluetoothGattService(SnapDropModule.SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY)
        val ch = BluetoothGattCharacteristic(
          SnapDropModule.INFO,
          BluetoothGattCharacteristic.PROPERTY_READ or BluetoothGattCharacteristic.PROPERTY_NOTIFY,
          BluetoothGattCharacteristic.PERMISSION_READ,
        )
        ch.addDescriptor(BluetoothGattDescriptor(SnapDropModule.CCCD, BluetoothGattDescriptor.PERMISSION_READ or BluetoothGattDescriptor.PERMISSION_WRITE))
        svc.addCharacteristic(ch)
        s.addService(svc)
        server = s
      }
      if (advertising == null) {
        val adv = adapter.bluetoothLeAdvertiser ?: return false
        val cb = object : AdvertiseCallback() {
          override fun onStartFailure(errorCode: Int) {
            advertising = null
          }
        }
        val settings = AdvertiseSettings.Builder()
          .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_POWER)
          .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
          .setConnectable(true)
          .build()
        val ad = AdvertiseData.Builder().setIncludeDeviceName(false).setIncludeTxPowerLevel(false)
          .addServiceUuid(ParcelUuid(SnapDropModule.SERVICE)).build()
        adv.startAdvertising(settings, ad, cb)
        advertising = cb
      }
      true
    } catch (e: Throwable) {
      android.util.Log.w("SnapDrop", "Bluetooth im Hintergrund: ${e.message}")
      false
    }
  }

  fun stop() {
    try {
      advertising?.let { bt?.adapter?.bluetoothLeAdvertiser?.stopAdvertising(it) }
    } catch (_: Throwable) {}
    advertising = null
    try { server?.close() } catch (_: Throwable) {}
    server = null
    info = ByteArray(0)
  }
}

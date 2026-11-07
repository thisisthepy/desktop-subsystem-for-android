package io.github.thisisthepy.dsa.agent.platform

import android.annotation.SuppressLint
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.net.wifi.p2p.WifiP2pManager
import android.os.Looper
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.settings.DeviceNamePort

/**
 * The guest's device name, as the agent, a platform app, sets it.
 *
 * The name Settings shows is `Settings.Global.DEVICE_NAME`, which Settings itself writes when
 * the user renames the device. Bluetooth takes its name from `Settings.Secure.bluetooth_name`
 * when its adapter starts, and from the adapter while it runs. Wi-Fi Direct has its own name,
 * which only a platform app may set (`WifiP2pManager.setDeviceName`, not in the SDK). A guest
 * without Bluetooth or Wi-Fi Direct is left as it is.
 */
class AndroidDeviceName(private val context: Context) : DeviceNamePort {
    override fun setDeviceName(name: String) {
        Settings.Global.putString(context.contentResolver, Settings.Global.DEVICE_NAME, name)
    }

    @SuppressLint("MissingPermission")
    override fun setBluetoothName(name: String) {
        val adapter = context.getSystemService(BluetoothManager::class.java)?.adapter ?: return
        Settings.Secure.putString(context.contentResolver, BLUETOOTH_NAME, name)
        try {
            adapter.setName(name)
        } catch (e: SecurityException) {
            // Without leave to talk to the adapter, it takes the name from the setting above
            // when it next starts.
        }
    }

    override fun setWifiDirectName(name: String) {
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_WIFI_DIRECT)) return
        val manager = context.getSystemService(WifiP2pManager::class.java) ?: return
        val channel = manager.initialize(context, Looper.getMainLooper(), null) ?: return
        WifiP2pManager::class.java
            .getMethod(
                "setDeviceName",
                WifiP2pManager.Channel::class.java,
                String::class.java,
                WifiP2pManager.ActionListener::class.java,
            )
            .invoke(manager, channel, name, null)
    }

    private companion object {
        /** Settings.Secure.BLUETOOTH_NAME, which is not in the SDK. */
        const val BLUETOOTH_NAME = "bluetooth_name"
    }
}

package io.github.thisisthepy.dsa.agent.settings

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope

/**
 * Where the agent puts the guest's device name. The platform's is
 * [io.github.thisisthepy.dsa.agent.platform.AndroidDeviceName]; a test's records the calls.
 */
interface DeviceNamePort {
    /** Makes [name] the name Settings shows for the device. */
    fun setDeviceName(name: String)

    /** Makes [name] the name Bluetooth shows other devices, when the guest has Bluetooth. */
    fun setBluetoothName(name: String)

    /** Makes [name] the name Wi-Fi Direct shows other devices, when the guest has it. */
    fun setWifiDirectName(name: String)

    companion object {
        val NONE = object : DeviceNamePort {
            override fun setDeviceName(name: String) {}
            override fun setBluetoothName(name: String) {}
            override fun setWifiDirectName(name: String) {}
        }
    }
}

/**
 * Makes the computer's name the guest's device name, as the host sends it ([DeviceName]) each
 * time the agent connects: the name Settings shows, and the one Bluetooth and Wi-Fi Direct
 * show other devices. Each is set on its own, so one the guest refuses or lacks leaves the
 * others. An empty name leaves the guest's as it is, and a name already set is not set again.
 */
class DeviceNameSync(
    private val port: DeviceNamePort,
    private val log: Log,
) {
    private var last: String? = null

    /** Acts on [frame] when it carries the device name, and says whether it did. */
    fun handle(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        if (!envelope.hasDeviceName()) return false
        apply(envelope.deviceName.name)
        return true
    }

    /** Forgets the name set, for a new connection: the next one applies in full. */
    fun connected() {
        last = null
    }

    private fun apply(name: String) {
        val trimmed = name.trim()
        if (trimmed.isEmpty() || trimmed == last) return
        attempt("device name") { port.setDeviceName(trimmed) }
        attempt("Bluetooth name") { port.setBluetoothName(trimmed) }
        attempt("Wi-Fi Direct name") { port.setWifiDirectName(trimmed) }
        last = trimmed
    }

    private fun attempt(what: String, change: () -> Unit) {
        try {
            change()
            log.info("follows the computer's name for the $what")
        } catch (e: Exception) {
            log.warn("could not follow the computer's name for the $what", e)
        }
    }
}

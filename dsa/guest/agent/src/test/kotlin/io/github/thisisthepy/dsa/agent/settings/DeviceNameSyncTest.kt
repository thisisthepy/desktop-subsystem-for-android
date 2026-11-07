package io.github.thisisthepy.dsa.agent.settings

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.DeviceName
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ScreenState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceNameSyncTest {
    private val calls = mutableListOf<String>()
    private val port = object : DeviceNamePort {
        override fun setDeviceName(name: String) {
            calls += "device $name"
        }

        override fun setBluetoothName(name: String) {
            calls += "bluetooth $name"
        }

        override fun setWifiDirectName(name: String) {
            calls += "wifi direct $name"
        }
    }
    private val log = RecordingLog()
    private val sync = DeviceNameSync(port, log)

    private fun named(name: String): ByteArray =
        Envelope.newBuilder()
            .setDeviceName(DeviceName.newBuilder().setName(name))
            .build()
            .toByteArray()

    @Test
    fun fr17_the_computers_name_becomes_the_guests_device_name() {
        assertTrue(sync.handle(named("BREW-BOOK")))
        assertEquals(
            listOf("device BREW-BOOK", "bluetooth BREW-BOOK", "wifi direct BREW-BOOK"),
            calls,
        )
        // The same name again is no change; a new connection sets it again.
        sync.handle(named("BREW-BOOK"))
        assertEquals(3, calls.size)
        sync.connected()
        sync.handle(named("BREW-BOOK"))
        assertEquals(6, calls.size)
        // A renamed computer is followed.
        sync.handle(named("Living room"))
        assertEquals("device Living room", calls[6])
    }

    @Test
    fun fr17_an_empty_name_leaves_the_guests_as_it_is() {
        assertTrue(sync.handle(named("  ")))
        assertTrue(calls.isEmpty())
    }

    @Test
    fun fr17_a_name_the_guest_refuses_does_not_keep_the_others_from_applying() {
        val refusing = object : DeviceNamePort by port {
            override fun setBluetoothName(name: String) = throw SecurityException("no")
        }
        DeviceNameSync(refusing, log).handle(named("BREW-BOOK"))
        assertEquals(listOf("device BREW-BOOK", "wifi direct BREW-BOOK"), calls)
        assertTrue(log.lines.any { it.startsWith("warn: could not follow the computer's name for the Bluetooth name") })
    }

    @Test
    fun fr17_other_frames_are_not_the_device_name() {
        val screen = Envelope.newBuilder().setScreenState(ScreenState.newBuilder().setOn(true)).build()
        assertFalse(sync.handle(screen.toByteArray()))
        assertFalse(sync.handle(byteArrayOf(1, 2, 3)))
        assertTrue(calls.isEmpty())
    }
}

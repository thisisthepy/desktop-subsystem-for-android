package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.FakePinRequest
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PinRequestsTest {
    private val sent = mutableListOf<Envelope>()
    private var channelUp = true
    private val scheduler = FakeScheduler()
    private val log = RecordingLog()
    private val pins = PinRequests(
        { frame ->
            if (channelUp) sent += decode(frame)
            channelUp
        },
        log,
        later = { delay, work -> scheduler.postDelayed(delay, work) },
    )

    private fun request(id: String = "ana") =
        FakePinRequest("org.example.chat", LauncherShortcut(id, "Ana", pinned = true), appLabel = "Chat")

    @Test
    fun fr7_a_pin_request_asks_the_host_with_the_app_and_the_shortcut() {
        assertTrue(pins.requested(request()))
        assertTrue(pins.requested(request("bo")))

        val asked = sent.map { it.pinShortcutRequest }
        assertEquals(listOf("org.example.chat", "org.example.chat"), asked.map { it.packageName })
        assertEquals("Chat", asked[0].appLabel)
        assertEquals("ana", asked[0].shortcut.id)
        assertEquals("Ana", asked[0].shortcut.shortLabel)
        assertTrue(asked[0].shortcut.pinned)
        assertNotEquals(asked[0].requestId, asked[1].requestId)
    }

    @Test
    fun fr7_yes_pins_the_shortcut_and_no_does_not() {
        val yes = request("ana")
        val no = request("bo")
        pins.requested(yes)
        pins.requested(no)
        val (yesId, noId) = sent.map { it.pinShortcutRequest.requestId }

        pins.answered(yesId, accepted = true)
        pins.answered(noId, accepted = false)

        assertEquals(1, yes.accepted)
        assertEquals(0, no.accepted)
        assertEquals(0, pins.pending)
    }

    @Test
    fun fr7_an_answer_to_no_waiting_request_does_nothing() {
        val waiting = request()
        pins.requested(waiting)
        val id = sent.single().pinShortcutRequest.requestId

        pins.answered(id + 100, accepted = true)
        pins.answered(id, accepted = true)
        pins.answered(id, accepted = true)

        assertEquals(1, waiting.accepted)
        assertTrue(log.lines.any { it.startsWith("warn: the host answered pin request") })
    }

    @Test
    fun fr7_a_request_with_no_answer_lapses() {
        val waiting = request()
        pins.requested(waiting)
        val id = sent.single().pinShortcutRequest.requestId

        scheduler.advance(PinRequests.LAPSE_MILLIS)
        pins.answered(id, accepted = true)

        assertEquals(0, waiting.accepted)
        assertEquals(0, pins.pending)
    }

    @Test
    fun fr7_a_request_the_host_could_not_be_asked_about_or_a_lost_connection_drops_it() {
        channelUp = false
        assertFalse(pins.requested(request()))
        assertEquals(0, pins.pending)

        channelUp = true
        pins.requested(request())
        pins.disconnected()
        assertEquals(0, pins.pending)
    }
}

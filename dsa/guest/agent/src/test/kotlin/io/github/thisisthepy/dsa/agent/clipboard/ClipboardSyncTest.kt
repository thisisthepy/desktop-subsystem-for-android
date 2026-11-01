package io.github.thisisthepy.dsa.agent.clipboard

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.ClipboardSharing
import io.github.thisisthepy.dsa.control.v1.ClipboardText
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's clipboard: holds one clip, and calls its listener on each change, as Android does. */
class FakeClipboard : GuestClipboard {
    var clip: String? = null
    val set = mutableListOf<String>()
    var refuseReads = false
    private var listener: (() -> Unit)? = null

    override fun text(): String? {
        if (refuseReads) throw SecurityException("not the default input method")
        return clip
    }

    override fun setText(text: String) {
        set += text
        clip = text
        listener?.invoke()
    }

    override fun listen(onChange: () -> Unit) {
        listener = onChange
    }

    /** An app in the guest copies [text]; null copies something that is not text. */
    fun copy(text: String?) {
        clip = text
        listener?.invoke()
    }
}

class ClipboardSyncTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val clipboard = FakeClipboard()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            clipboard = clipboard,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun fromHost(text: String) {
        daemon.deliver(Envelope.newBuilder().setClipboardText(ClipboardText.newBuilder().setText(text)).build().toByteArray())
        scheduler.runDue()
    }

    private fun sharing(on: Boolean) {
        daemon.deliver(Envelope.newBuilder().setClipboardSharing(ClipboardSharing.newBuilder().setOn(on)).build().toByteArray())
        scheduler.runDue()
    }

    private fun copyInGuest(text: String?) {
        clipboard.copy(text)
        scheduler.runDue()
    }

    private fun sentToHost(): List<String> =
        daemon.sentMessages().filter { it.hasClipboardText() }.map { it.clipboardText.text }

    @Test
    fun fr13_text_copied_on_the_host_becomes_the_guests_primary_clip() {
        started()
        fromHost("copied on the host")
        assertEquals(listOf("copied on the host"), clipboard.set)
    }

    @Test
    fun fr13_text_copied_in_the_guest_goes_to_the_host() {
        started()
        copyInGuest("안녕 from Android")
        assertEquals(listOf("안녕 from Android"), sentToHost())
    }

    @Test
    fun fr13_the_clip_the_host_set_is_not_sent_back() {
        started()
        fromHost("from the host")
        // Setting the clip called the listener, which read the same text.
        assertEquals(emptyList<String>(), sentToHost())
        copyInGuest("then copied in Android")
        assertEquals(listOf("then copied in Android"), sentToHost())
    }

    @Test
    fun fr13_text_the_guest_sent_is_not_set_again_when_the_host_echoes_it() {
        started()
        copyInGuest("from the guest")
        fromHost("from the guest")
        assertEquals(emptyList<String>(), clipboard.set)
    }

    @Test
    fun fr13_with_sharing_off_nothing_crosses_and_turning_it_on_resumes() {
        started()
        sharing(false)
        copyInGuest("secret in the guest")
        fromHost("secret on the host")
        assertEquals(emptyList<String>(), sentToHost())
        assertEquals(emptyList<String>(), clipboard.set)

        sharing(true)
        copyInGuest("shared again")
        fromHost("from the host again")
        assertEquals(listOf("shared again"), sentToHost())
        assertEquals(listOf("from the host again"), clipboard.set)
    }

    @Test
    fun fr13_a_new_connection_shares_the_clipboard_again() {
        started()
        sharing(false)
        locator.up = null
        daemon.die()
        scheduler.runDue()
        val restarted = FakeEndpoint()
        locator.up = restarted
        scheduler.advance(1000)
        clipboard.copy("after the reconnection")
        scheduler.runDue()
        assertEquals(
            listOf("after the reconnection"),
            restarted.sentMessages().filter { it.hasClipboardText() }.map { it.clipboardText.text },
        )
    }

    @Test
    fun fr13_text_longer_than_the_limit_does_not_cross_either_way() {
        started()
        val long = "x".repeat(ClipboardSync.MAX_BYTES + 1)
        copyInGuest(long)
        fromHost(long)
        assertEquals(emptyList<String>(), sentToHost())
        assertEquals(emptyList<String>(), clipboard.set)
        assertTrue(log.lines.any { it.contains("longer than") })

        // Korean text counts in bytes of UTF-8: three for each syllable.
        val korean = "한".repeat(ClipboardSync.MAX_BYTES / 3 + 1)
        copyInGuest(korean)
        assertEquals(emptyList<String>(), sentToHost())
        val most = "y".repeat(ClipboardSync.MAX_BYTES)
        copyInGuest(most)
        assertEquals(listOf(most), sentToHost())
    }

    @Test
    fun fr13_a_clip_that_is_not_text_stays_in_the_guest() {
        started()
        copyInGuest(null)
        assertEquals(emptyList<String>(), sentToHost())
    }

    @Test
    fun nfr5_a_clipboard_the_agent_may_not_read_is_logged_and_the_agent_goes_on() {
        started()
        clipboard.refuseReads = true
        copyInGuest("refused")
        assertEquals(emptyList<String>(), sentToHost())
        assertTrue(log.lines.any { it.contains("could not read Android's clipboard") })
        fromHost("still taken")
        assertEquals(listOf("still taken"), clipboard.set)
    }
}

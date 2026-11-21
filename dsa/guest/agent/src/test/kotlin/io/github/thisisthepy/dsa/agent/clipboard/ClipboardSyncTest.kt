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
import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.control.v1.ClipboardImage
import io.github.thisisthepy.dsa.control.v1.ClipboardSharing
import io.github.thisisthepy.dsa.control.v1.ClipboardText
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's clipboard: holds one clip, and calls its listener on each change, as Android does. */
class FakeClipboard : GuestClipboard {
    var clip: String? = null
    var image: ByteArray? = null
    val set = mutableListOf<String>()
    val setImages = mutableListOf<ByteArray>()
    var refuseReads = false
    private var listener: (() -> Unit)? = null

    override fun text(): String? {
        if (refuseReads) throw SecurityException("not the default input method")
        return clip
    }

    override fun image(): ByteArray? {
        if (refuseReads) throw SecurityException("not the default input method")
        return image
    }

    override fun setImage(png: ByteArray) {
        setImages += png
        clip = null
        image = png
        listener?.invoke()
    }

    override fun setText(text: String) {
        set += text
        clip = text
        image = null
        listener?.invoke()
    }

    override fun listen(onChange: () -> Unit) {
        listener = onChange
    }

    /** An app in the guest copies the image [png]. */
    fun copyImage(png: ByteArray) {
        clip = null
        image = png
        listener?.invoke()
    }

    /** An app in the guest copies what the clipboard holds now, as set on this fake. */
    fun copyKeeping() {
        listener?.invoke()
    }

    /** An app in the guest copies [text]; null copies something that is not text. */
    fun copy(text: String?) {
        clip = text
        image = null
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

    /** A PNG of [length] bytes: the signature, then filler. */
    private fun png(length: Int, fill: Int = 7): ByteArray {
        val bytes = ByteArray(length) { fill.toByte() }
        byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a).copyInto(bytes)
        return bytes
    }

    private fun imageFromHost(clipId: Int, png: ByteArray, pieceBytes: Int = ClipboardSync.IMAGE_PIECE_BYTES) {
        var offset = 0
        do {
            val end = minOf(offset + pieceBytes, png.size)
            val piece = ClipboardImage.newBuilder()
                .setClipId(clipId)
                .setSize(png.size.toLong())
                .setOffset(offset.toLong())
                .setData(ByteString.copyFrom(png, offset, end - offset))
            daemon.deliver(Envelope.newBuilder().setClipboardImage(piece).build().toByteArray())
            offset = end
        } while (offset < png.size)
        scheduler.runDue()
    }

    /** The images sent to the host, each put back together from its pieces. */
    private fun imagesSentToHost(): List<ByteArray> {
        val images = mutableListOf<ByteArray>()
        var current: java.io.ByteArrayOutputStream? = null
        for (message in daemon.sentMessages().filter { it.hasClipboardImage() }) {
            val piece = message.clipboardImage
            assertTrue(piece.data.size() <= ClipboardSync.IMAGE_PIECE_BYTES)
            if (piece.offset == 0L) current = java.io.ByteArrayOutputStream()
            piece.data.writeTo(current!!)
            if (current.size().toLong() == piece.size) images += current.toByteArray()
        }
        return images
    }

    @Test
    fun fr13_an_image_copied_on_the_host_pastes_in_android() {
        started()
        val image = png(ClipboardSync.IMAGE_PIECE_BYTES * 2 + 5)
        imageFromHost(1, image)
        assertEquals(1, clipboard.setImages.size)
        assertArrayEquals(image, clipboard.setImages[0])
        // Setting the clip called the listener, which read the same image: nothing goes back.
        assertEquals(0, imagesSentToHost().size)
    }

    @Test
    fun fr13_an_image_copied_in_android_pastes_on_the_host() {
        started()
        val image = png(ClipboardSync.IMAGE_PIECE_BYTES + 100)
        clipboard.image = image
        clipboard.copyImage(image)
        scheduler.runDue()
        val sent = imagesSentToHost()
        assertEquals(1, sent.size)
        assertArrayEquals(image, sent[0])
        assertTrue(daemon.sentMessages().count { it.hasClipboardImage() } > 1)
    }

    @Test
    fun fr13_an_image_that_crossed_is_not_sent_back() {
        started()
        val fromGuest = png(100, fill = 1)
        clipboard.copyImage(fromGuest)
        scheduler.runDue()
        imageFromHost(5, fromGuest)
        assertEquals(0, clipboard.setImages.size)
    }

    @Test
    fun fr13_an_image_larger_than_the_cap_does_not_cross() {
        started()
        val huge = png(ClipboardSync.MAX_IMAGE_BYTES + 1)
        clipboard.copyImage(huge)
        scheduler.runDue()
        assertEquals(0, imagesSentToHost().size)
        // The host says its image is longer than the cap: none of its pieces are kept.
        daemon.deliver(
            Envelope.newBuilder().setClipboardImage(
                ClipboardImage.newBuilder().setClipId(3).setSize(huge.size.toLong()).setOffset(0)
                    .setData(ByteString.copyFrom(huge, 0, 1024)),
            ).build().toByteArray(),
        )
        scheduler.runDue()
        assertEquals(0, clipboard.setImages.size)
        assertTrue(log.lines.any { it.contains("12582912") })
        // A smaller image still crosses.
        val small = png(50)
        imageFromHost(4, small)
        assertEquals(1, clipboard.setImages.size)
    }

    @Test
    fun fr13_with_sharing_off_no_image_crosses() {
        started()
        sharing(false)
        clipboard.copyImage(png(80))
        scheduler.runDue()
        imageFromHost(1, png(90))
        assertEquals(0, imagesSentToHost().size)
        assertEquals(0, clipboard.setImages.size)
    }

    @Test
    fun fr13_text_wins_over_an_image_copied_with_it() {
        started()
        copyInGuest("cells")
        clipboard.image = png(60)
        // The fake clears the image on a copy of text; a program that copies both is like this.
        clipboard.clip = "cells and a picture"
        clipboard.copyKeeping()
        scheduler.runDue()
        assertEquals(listOf("cells", "cells and a picture"), sentToHost())
        assertEquals(0, imagesSentToHost().size)
    }

    @Test
    fun nfr5_an_image_whose_pieces_do_not_follow_is_dropped() {
        started()
        val image = png(ClipboardSync.IMAGE_PIECE_BYTES + 10)
        daemon.deliver(
            Envelope.newBuilder().setClipboardImage(
                ClipboardImage.newBuilder().setClipId(1).setSize(image.size.toLong())
                    .setOffset(ClipboardSync.IMAGE_PIECE_BYTES.toLong())
                    .setData(ByteString.copyFrom(image, ClipboardSync.IMAGE_PIECE_BYTES, 10)),
            ).build().toByteArray(),
        )
        scheduler.runDue()
        assertEquals(0, clipboard.setImages.size)
        imageFromHost(2, image)
        assertEquals(1, clipboard.setImages.size)
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

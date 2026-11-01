package io.github.thisisthepy.dsa.agent.clipboard

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.ClipboardText
import io.github.thisisthepy.dsa.control.v1.Envelope

/** The guest's clipboard, as the agent uses it: its primary clip as text. */
interface GuestClipboard {
    /** The primary clip's text; null when there is no clip, or it is not text. */
    fun text(): String?

    /** Makes [text] the primary clip. */
    fun setText(text: String)

    /** Calls [onChange] on any thread each time the primary clip changes. */
    fun listen(onChange: () -> Unit)

    companion object {
        /** No clipboard: nothing is read, and nothing is set. */
        val NONE = object : GuestClipboard {
            override fun text(): String? = null
            override fun setText(text: String) = Unit
            override fun listen(onChange: () -> Unit) = Unit
        }
    }
}

/**
 * Keeps the guest's clipboard and the host's in step. Text the host copied becomes the
 * guest's primary clip, and text copied in the guest goes to the host.
 *
 * A clip that came from the host is not sent back when the clipboard says it changed, and
 * the host does the same the other way. Text longer than [MAX_BYTES] does not cross in either
 * direction. The clipboards are taken to be shared when the agent connects; the host says when
 * they are not, and while they are not, nothing crosses.
 *
 * Every method but [start]'s listener runs on the agent's one thread.
 */
class ClipboardSync(
    private val clipboard: GuestClipboard,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val isConnected: () -> Boolean,
) {
    private var sharing = true

    /** The last text that crossed, either way, so it is not sent back. */
    private var last: String? = null

    /** Starts following the clipboard; [post] runs work on the agent's thread. */
    fun start(post: (() -> Unit) -> Unit) {
        try {
            clipboard.listen { post(::changed) }
        } catch (e: Exception) {
            log.warn("cannot follow the clipboard; text copied in Android does not reach the host", e)
        }
    }

    /** A new connection: the host takes the clipboards to be shared, and nothing to have crossed. */
    fun connected() {
        sharing = true
        last = null
    }

    /** Acts on the frame if it is the host's clipboard text or its word on sharing. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.CLIPBOARD_TEXT -> fromHost(envelope.clipboardText.text)
            Envelope.BodyCase.CLIPBOARD_SHARING -> {
                sharing = envelope.clipboardSharing.on
                last = null
                log.info("clipboard sharing is ${if (sharing) "on" else "off"}")
            }
            else -> return false
        }
        return true
    }

    private fun fromHost(text: String) {
        if (!sharing) {
            log.warn("dropped clipboard text from the host; sharing is off")
            return
        }
        if (!fits(text)) {
            log.warn("dropped clipboard text from the host: ${utf8Size(text)} bytes, longer than the $MAX_BYTES that cross")
            return
        }
        if (text == last) return
        last = text
        try {
            clipboard.setText(text)
        } catch (e: Exception) {
            log.warn("could not put the host's clipboard text on Android's clipboard", e)
        }
    }

    /** The primary clip changed: sends its text to the host, unless it came from there. */
    fun changed() {
        if (!sharing || !isConnected()) return
        val text = try {
            clipboard.text()
        } catch (e: Exception) {
            log.warn("could not read Android's clipboard", e)
            return
        } ?: return
        if (text == last) return
        if (!fits(text)) {
            log.warn("dropped text copied in Android: ${utf8Size(text)} bytes, longer than the $MAX_BYTES that cross")
            return
        }
        last = text
        send(Envelope.newBuilder().setClipboardText(ClipboardText.newBuilder().setText(text)).build().toByteArray())
    }

    companion object {
        /**
         * The longest text that crosses, in bytes of UTF-8. A clip reaches the clipboard service
         * in a binder transaction, and a process's binder buffer is 1 MiB for every transaction
         * in flight; as UTF-16 this text takes at most half of it. A longer clip is dropped,
         * never cut, so a paste is the whole text or nothing. The host keeps the same limit.
         */
        const val MAX_BYTES = 256 * 1024

        private fun utf8Size(text: String) = text.toByteArray(Charsets.UTF_8).size

        /** A string never has more UTF-16 units than UTF-8 bytes, so a long one is not encoded. */
        private fun fits(text: String) = text.length <= MAX_BYTES && utf8Size(text) <= MAX_BYTES
    }
}

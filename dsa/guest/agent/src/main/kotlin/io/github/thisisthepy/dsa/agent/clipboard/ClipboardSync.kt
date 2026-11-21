package io.github.thisisthepy.dsa.agent.clipboard

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.ClipboardImage
import io.github.thisisthepy.dsa.control.v1.ClipboardText
import io.github.thisisthepy.dsa.control.v1.Envelope

/** The guest's clipboard, as the agent uses it: its primary clip as text or as an image. */
interface GuestClipboard {
    /** The primary clip's text; null when there is no clip, or it is not text. */
    fun text(): String?

    /**
     * The primary clip's image as PNG; null when there is no clip, or it holds no image. An
     * image of another type is made PNG. At most [ClipboardSync.MAX_IMAGE_BYTES] plus one bytes
     * are read, so an image that is too long is seen to be.
     */
    fun image(): ByteArray?

    /** Makes [text] the primary clip. */
    fun setText(text: String)

    /** Makes the image [png] the primary clip. */
    fun setImage(png: ByteArray)

    /** Calls [onChange] on any thread each time the primary clip changes. */
    fun listen(onChange: () -> Unit)

    companion object {
        /** No clipboard: nothing is read, and nothing is set. */
        val NONE = object : GuestClipboard {
            override fun text(): String? = null
            override fun image(): ByteArray? = null
            override fun setText(text: String) = Unit
            override fun setImage(png: ByteArray) = Unit
            override fun listen(onChange: () -> Unit) = Unit
        }
    }
}

/** What crosses between the clipboards. */
private sealed class Clip {
    data class Text(val text: String) : Clip()

    class Image(val png: ByteArray) : Clip() {
        override fun equals(other: Any?) = other is Image && other.png.contentEquals(png)
        override fun hashCode() = png.contentHashCode()
    }
}

/**
 * Keeps the guest's clipboard and the host's in step. Text and images the host copied become
 * the guest's primary clip, and text and images copied in the guest go to the host. Images
 * cross as PNG, in pieces of at most [IMAGE_PIECE_BYTES], so other messages pass between them.
 *
 * A clip that came from the host is not sent back when the clipboard says it changed, and
 * the host does the same the other way. Text longer than [MAX_BYTES] and images longer than
 * [MAX_IMAGE_BYTES] do not cross in either direction. The clipboards are taken to be shared
 * when the agent connects; the host says when they are not, and while they are not, nothing
 * crosses.
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

    /** The last clip that crossed, either way, so it is not sent back. */
    private var last: Clip? = null

    /** The id of the last image sent to the host. */
    private var lastImageId = 0

    /** The image the host is sending: its id, its whole length, and what came so far. */
    private var incomingId: Int? = null
    private var incomingSize = 0L
    private var incoming: java.io.ByteArrayOutputStream? = null

    /** An image from the host that was refused, whose later pieces are dropped. */
    private var refusedId: Int? = null

    /** Starts following the clipboard; [post] runs work on the agent's thread. */
    fun start(post: (() -> Unit) -> Unit) {
        try {
            clipboard.listen { post(::changed) }
        } catch (e: Exception) {
            log.warn("cannot follow the clipboard; what is copied in Android does not reach the host", e)
        }
    }

    /** A new connection: the host takes the clipboards to be shared, and nothing to have crossed. */
    fun connected() {
        sharing = true
        last = null
        dropIncoming()
        refusedId = null
    }

    /** Acts on the frame if it is the host's clipboard text or image, or its word on sharing. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.CLIPBOARD_TEXT -> fromHost(Clip.Text(envelope.clipboardText.text))
            Envelope.BodyCase.CLIPBOARD_IMAGE -> imagePiece(envelope.clipboardImage)
            Envelope.BodyCase.CLIPBOARD_SHARING -> {
                sharing = envelope.clipboardSharing.on
                last = null
                dropIncoming()
                log.info("clipboard sharing is ${if (sharing) "on" else "off"}")
            }
            else -> return false
        }
        return true
    }

    private fun dropIncoming() {
        incomingId = null
        incoming = null
        incomingSize = 0
    }

    /** Takes a piece of an image from the host, and the whole image once it is there. */
    private fun imagePiece(piece: ClipboardImage) {
        if (!sharing) {
            log.warn("dropped a clipboard image from the host; sharing is off")
            return
        }
        if (piece.clipId == refusedId) return
        if (piece.clipId != incomingId) {
            if (piece.size > MAX_IMAGE_BYTES) {
                refuse(piece.clipId, "${piece.size} bytes of PNG, more than the $MAX_IMAGE_BYTES that cross")
                return
            }
            incomingId = piece.clipId
            incomingSize = piece.size
            incoming = java.io.ByteArrayOutputStream(piece.size.toInt())
        }
        val buffer = incoming ?: return
        if (piece.size != incomingSize || piece.offset != buffer.size().toLong()) {
            refuse(piece.clipId, "a piece at ${piece.offset} came after ${buffer.size()} of $incomingSize")
            return
        }
        if (buffer.size().toLong() + piece.data.size() > incomingSize) {
            refuse(piece.clipId, "its pieces are longer than $incomingSize bytes")
            return
        }
        piece.data.writeTo(buffer)
        if (buffer.size().toLong() == incomingSize) {
            val png = buffer.toByteArray()
            dropIncoming()
            fromHost(Clip.Image(png))
        }
    }

    private fun refuse(clipId: Int, why: String) {
        dropIncoming()
        refusedId = clipId
        log.warn("dropped a clipboard image from the host: $why")
    }

    private fun fromHost(clip: Clip) {
        if (!sharing) {
            log.warn("dropped clipboard text from the host; sharing is off")
            return
        }
        tooLong(clip)?.let {
            log.warn("dropped a clip from the host: $it")
            return
        }
        if (clip == last) return
        last = clip
        try {
            when (clip) {
                is Clip.Text -> clipboard.setText(clip.text)
                is Clip.Image -> clipboard.setImage(clip.png)
            }
        } catch (e: Exception) {
            log.warn("could not put the host's clip on Android's clipboard", e)
        }
    }

    /** The primary clip changed: sends its text or image to the host, unless it came from there. */
    fun changed() {
        if (!sharing || !isConnected()) return
        val clip = try {
            // Text when there is text: an image copied with it is a picture of the text.
            clipboard.text()?.let { Clip.Text(it) } ?: clipboard.image()?.let { Clip.Image(it) }
        } catch (e: Exception) {
            log.warn("could not read Android's clipboard", e)
            return
        } ?: return
        if (clip == last) return
        tooLong(clip)?.let {
            log.warn("dropped what was copied in Android: $it")
            return
        }
        last = clip
        when (clip) {
            is Clip.Text ->
                send(Envelope.newBuilder().setClipboardText(ClipboardText.newBuilder().setText(clip.text)).build().toByteArray())
            is Clip.Image -> sendImage(clip.png)
        }
    }

    private fun sendImage(png: ByteArray) {
        lastImageId += 1
        val id = lastImageId
        var offset = 0
        do {
            val end = minOf(offset + IMAGE_PIECE_BYTES, png.size)
            val piece = ClipboardImage.newBuilder()
                .setClipId(id)
                .setSize(png.size.toLong())
                .setOffset(offset.toLong())
                .setData(ByteString.copyFrom(png, offset, end - offset))
            if (!send(Envelope.newBuilder().setClipboardImage(piece).build().toByteArray())) return
            offset = end
        } while (offset < png.size)
    }

    /** Why [clip] may not cross, or null when it may. */
    private fun tooLong(clip: Clip): String? = when (clip) {
        is Clip.Text -> if (fits(clip.text)) null else "${utf8Size(clip.text)} bytes of text, longer than the $MAX_BYTES that cross"
        is Clip.Image -> when {
            clip.png.size > MAX_IMAGE_BYTES -> "${clip.png.size} bytes of PNG, longer than the $MAX_IMAGE_BYTES that cross"
            !isPng(clip.png) -> "the image is not a PNG"
            else -> null
        }
    }

    companion object {
        /**
         * The longest text that crosses, in bytes of UTF-8. A clip reaches the clipboard service
         * in a binder transaction, and a process's binder buffer is 1 MiB for every transaction
         * in flight; as UTF-16 this text takes at most half of it. A longer clip is dropped,
         * never cut, so a paste is the whole text or nothing. The host keeps the same limit.
         */
        const val MAX_BYTES = 256 * 1024

        /**
         * The longest PNG that crosses. An image never goes through binder (the clip holds a
         * `content://` URI), and the host carries one image in one frame of its local API, which
         * must stay under 16 MiB. A longer image is dropped, never cut. The host keeps the same
         * limit.
         */
        const val MAX_IMAGE_BYTES = 12 * 1024 * 1024

        /** The longest piece of an image on the control channel. */
        const val IMAGE_PIECE_BYTES = 256 * 1024

        private val PNG_SIGNATURE = byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)

        fun isPng(bytes: ByteArray) =
            bytes.size >= PNG_SIGNATURE.size && PNG_SIGNATURE.indices.all { bytes[it] == PNG_SIGNATURE[it] }

        private fun utf8Size(text: String) = text.toByteArray(Charsets.UTF_8).size

        /** A string never has more UTF-16 units than UTF-8 bytes, so a long one is not encoded. */
        private fun fits(text: String) = text.length <= MAX_BYTES && utf8Size(text) <= MAX_BYTES
    }
}

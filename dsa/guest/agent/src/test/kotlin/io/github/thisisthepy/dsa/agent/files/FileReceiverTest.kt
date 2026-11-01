package io.github.thisisthepy.dsa.agent.files

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.FileAction
import io.github.thisisthepy.dsa.control.v1.FileChunk
import io.github.thisisthepy.dsa.control.v1.FileProgress
import io.github.thisisthepy.dsa.control.v1.OpenFile
import java.io.File
import java.nio.file.Files
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FileReceiverTest {
    private val root: File = Files.createTempDirectory("dsa-host-files").toFile()
    private var now = 1_000_000L
    private val store = TransferStore(root, clock = { now })
    private val log = RecordingLog()
    private val sent = mutableListOf<FileProgress>()
    private val opened = mutableListOf<Pair<StoredFile, FileAction>>()
    private var openerFails = false
    private val receiver = FileReceiver(
        store,
        { file, action ->
            if (openerFails) throw IllegalStateException("no chooser")
            opened += file to action
        },
        { frame -> sent += decode(frame).fileProgress; true },
        log,
    )

    @After
    fun cleanUp() {
        root.deleteRecursively()
    }

    private fun open(id: Int, name: String, size: Long, action: FileAction = FileAction.FILE_ACTION_VIEW) {
        receiver.handle(
            Envelope.newBuilder().setOpenFile(
                OpenFile.newBuilder().setTransferId(id).setName(name).setSize(size).setAction(action),
            ).build(),
        )
    }

    private fun chunk(id: Int, offset: Long, data: ByteArray) {
        receiver.handle(
            Envelope.newBuilder().setFileChunk(
                FileChunk.newBuilder().setTransferId(id).setOffset(offset).setData(ByteString.copyFrom(data)),
            ).build(),
        )
    }

    @Test
    fun fr9_a_file_in_chunks_is_reassembled_and_offered_to_the_apps() {
        val content = ByteArray(300_000) { (it % 251).toByte() }
        open(4, "photo.jpg", content.size.toLong(), FileAction.FILE_ACTION_SEND)
        var offset = 0
        while (offset < content.size) {
            val end = minOf(offset + 131_072, content.size)
            chunk(4, offset.toLong(), content.copyOfRange(offset, end))
            offset = end
        }

        assertEquals(listOf(131_072L, 262_144L, 300_000L), sent.map { it.received })
        assertTrue(sent.all { it.error.isEmpty() && it.transferId == 4 })
        val (file, action) = opened.single()
        assertEquals(FileAction.FILE_ACTION_SEND, action)
        assertEquals("photo.jpg", file.name)
        assertArrayEquals(content, file.file.readBytes())
        assertEquals(root, file.file.parentFile.parentFile)
    }

    @Test
    fun fr9_an_empty_file_is_offered_at_once() {
        open(1, "empty.txt", 0)
        assertEquals(1, opened.size)
        assertEquals(listOf(0L), sent.map { it.received })
    }

    @Test
    fun nfr5_a_piece_out_of_order_or_beyond_the_size_ends_the_transfer() {
        open(1, "a.bin", 4)
        chunk(1, 2, byteArrayOf(1, 2))
        assertTrue(sent.last().error.contains("out of order"))

        open(2, "b.bin", 4)
        chunk(2, 0, byteArrayOf(1, 2, 3, 4, 5))
        assertTrue(sent.last().error.contains("more than"))

        // A piece of a transfer that has ended is dropped without an answer.
        val answers = sent.size
        chunk(2, 0, byteArrayOf(1))
        assertEquals(answers, sent.size)
        assertEquals(emptyList<Pair<StoredFile, FileAction>>(), opened)
        assertEquals("the folders of failed transfers are removed", 0, root.listFiles()!!.size)
    }

    @Test
    fun nfr5_a_name_that_leaves_the_folder_or_a_file_too_large_is_refused() {
        for (name in listOf("../escape", "a/b", "..", "", "a\u0000b", "x".repeat(256))) {
            open(1, name, 1)
            assertTrue("$name was taken", sent.last().error.isNotEmpty())
        }
        open(2, "big.iso", FileReceiver.MAX_FILE_BYTES + 1)
        assertTrue(sent.last().error.contains("MiB"))
        open(3, "a.txt", 1, FileAction.FILE_ACTION_UNSPECIFIED)
        assertTrue(sent.last().error.isNotEmpty())
        assertEquals(0, root.listFiles()!!.size)
    }

    @Test
    fun fr9_a_file_no_app_can_be_asked_about_is_reported_and_removed() {
        openerFails = true
        open(1, "a.txt", 1)
        chunk(1, 0, byteArrayOf(7))
        assertTrue(sent.last().error.contains("no app"))
        assertEquals(0, root.listFiles()!!.size)
    }

    @Test
    fun fr9_old_transfers_are_cleaned_up_and_a_new_connection_drops_unfinished_ones() {
        open(1, "old.txt", 1)
        chunk(1, 0, byteArrayOf(1))
        val old = opened.single().first
        assertTrue(old.file.exists())

        now += TransferStore.KEEP_MILLIS + 1
        open(2, "new.txt", 2)
        assertFalse("an hour-old transfer is removed when the next begins", old.file.exists())

        receiver.connected()
        assertEquals("an unfinished transfer is dropped with its connection", 0, root.listFiles()!!.size)
        chunk(2, 0, byteArrayOf(1, 2))
        assertEquals(1, opened.size)
    }
}

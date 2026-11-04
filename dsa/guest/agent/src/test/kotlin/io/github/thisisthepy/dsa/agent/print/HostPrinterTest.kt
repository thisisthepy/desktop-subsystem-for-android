package io.github.thisisthepy.dsa.agent.print

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PrintJobResult
import io.github.thisisthepy.dsa.control.v1.PrintOutcome
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** A spooler job that records how it ended. */
class FakePrintJob(private val document: ByteArray?) : QueuedPrintJob {
    override val details = PrintDetails(
        packageName = "",
        appLabel = "",
        documentName = "Report",
        pageCount = 2,
        copies = 3,
        color = true,
        duplex = false,
        landscape = true,
        mediaWidthMils = 8500,
        mediaHeightMils = 11000,
    )
    val events = mutableListOf<String>()

    override fun readDocument(limit: Long): ByteArray? = document?.takeIf { it.size <= limit }

    override fun started() {
        events += "started"
    }

    override fun completed() {
        events += "completed"
    }

    override fun canceled() {
        events += "canceled"
    }

    override fun failed(reason: String) {
        events += "failed: $reason"
    }
}

class HostPrinterTest {
    private val sent = mutableListOf<Envelope>()
    private val later = ArrayDeque<() -> Unit>()
    private var connected = true
    private var sendWorks = true
    private val printer = HostPrinter(
        send = { frame ->
            if (sendWorks) sent += Envelope.parseFrom(frame)
            sendWorks
        },
        log = RecordingLog(),
        isConnected = { connected },
        post = { work -> later.addLast(work) },
    )

    private fun runPosted() {
        while (later.isNotEmpty()) later.removeFirst()()
    }

    private fun result(id: Int, outcome: PrintOutcome, message: String = ""): Envelope =
        Envelope.newBuilder()
            .setPrintJobResult(PrintJobResult.newBuilder().setJobId(id).setOutcome(outcome).setMessage(message))
            .build()

    @Test
    fun fr26_a_queued_document_goes_to_the_host_whole_in_pieces() {
        val pdf = ByteArray(HostPrinter.PIECE_BYTES * 2 + 5) { (it % 7).toByte() }
        val job = FakePrintJob(pdf)
        printer.queued(job)
        runPosted()
        val offer = sent.first().printJob
        assertEquals(pdf.size.toLong(), offer.size)
        assertEquals("Report", offer.documentName)
        assertEquals(3, offer.copies)
        assertTrue(offer.landscape)
        val pieces = sent.drop(1).map { it.printJobChunk }
        assertEquals(3, pieces.size)
        assertEquals(listOf(0L, HostPrinter.PIECE_BYTES.toLong(), 2L * HostPrinter.PIECE_BYTES), pieces.map { it.offset })
        val joined = pieces.fold(ByteArray(0)) { all, piece -> all + piece.data.toByteArray() }
        assertArrayEquals(pdf, joined)
        assertEquals(listOf("started"), job.events)
    }

    @Test
    fun fr26_the_hosts_answer_ends_the_job_the_same_way() {
        val printed = FakePrintJob(byteArrayOf(1))
        val canceled = FakePrintJob(byteArrayOf(2))
        val failed = FakePrintJob(byteArrayOf(3))
        printer.queued(printed)
        printer.queued(canceled)
        printer.queued(failed)
        val ids = sent.filter { it.hasPrintJob() }.map { it.printJob.jobId }
        assertTrue(printer.handle(result(ids[0], PrintOutcome.PRINT_OUTCOME_PRINTED)))
        assertTrue(printer.handle(result(ids[1], PrintOutcome.PRINT_OUTCOME_CANCELED)))
        assertTrue(printer.handle(result(ids[2], PrintOutcome.PRINT_OUTCOME_FAILED, "out of paper")))
        assertEquals(listOf("started", "completed"), printed.events)
        assertEquals(listOf("started", "canceled"), canceled.events)
        assertEquals(listOf("started", "failed: out of paper"), failed.events)
    }

    @Test
    fun fr26_a_job_canceled_in_android_is_withdrawn_from_the_host() {
        val job = FakePrintJob(byteArrayOf(1))
        printer.queued(job)
        val id = sent.first().printJob.jobId
        printer.cancelRequested(job)
        assertEquals(id, sent.last().printJobCancel.jobId)
        assertEquals(listOf("started", "canceled"), job.events)
        // A late answer changes nothing.
        printer.handle(result(id, PrintOutcome.PRINT_OUTCOME_PRINTED))
        assertEquals(listOf("started", "canceled"), job.events)
    }

    @Test
    fun fr26_a_job_without_the_host_fails_at_once() {
        connected = false
        val job = FakePrintJob(byteArrayOf(1))
        printer.queued(job)
        assertTrue(sent.isEmpty())
        assertTrue(job.events.single().startsWith("failed"))
    }

    @Test
    fun fr26_a_document_that_cannot_be_read_or_sent_fails() {
        val unreadable = FakePrintJob(null)
        printer.queued(unreadable)
        assertTrue(unreadable.events.single().startsWith("failed"))
        sendWorks = false
        val unsent = FakePrintJob(byteArrayOf(1))
        printer.queued(unsent)
        assertTrue(unsent.events.last().startsWith("failed"))
    }

    @Test
    fun fr26_jobs_of_a_lost_connection_fail_when_the_agent_connects_again() {
        val job = FakePrintJob(byteArrayOf(1))
        printer.queued(job)
        printer.connected()
        assertTrue(job.events.last().startsWith("failed"))
    }

    @Test
    fun fr26_other_frames_are_not_the_printers() {
        assertEquals(false, printer.handle(Envelope.getDefaultInstance()))
    }
}

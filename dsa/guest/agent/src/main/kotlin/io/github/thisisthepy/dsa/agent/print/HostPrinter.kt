package io.github.thisisthepy.dsa.agent.print

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PrintJob
import io.github.thisisthepy.dsa.control.v1.PrintJobCancel
import io.github.thisisthepy.dsa.control.v1.PrintJobChunk
import io.github.thisisthepy.dsa.control.v1.PrintOutcome

/** What the user chose for a job in Android's print dialog, and what the app said of it. */
data class PrintDetails(
    val packageName: String,
    val appLabel: String,
    val documentName: String,
    /** 0 when the app did not say. */
    val pageCount: Int,
    val copies: Int,
    val color: Boolean,
    val duplex: Boolean,
    val landscape: Boolean,
    /** The paper's size, in thousandths of an inch; 0 when unknown. */
    val mediaWidthMils: Int,
    val mediaHeightMils: Int,
)

/**
 * One job Android's print spooler gave the agent's print service. The platform's ends the
 * job in the spooler on the main thread, as Android requires; a test's records it.
 */
interface QueuedPrintJob {
    val details: PrintDetails

    /** The job's document, a PDF; null when it cannot be read. Called on the agent's thread. */
    fun readDocument(limit: Long): ByteArray?

    /** The job is being printed. */
    fun started()

    fun completed()

    fun canceled()

    fun failed(reason: String)
}

/**
 * Prints the guest's documents on the host's printers. Android's print dialog lays the
 * document out as a PDF; the agent's print service hands it here, and it goes to the host in
 * pieces. The host shows its own print dialog, prints, and says what became of the job, which
 * ends it in the spooler the same way: completed, canceled or failed with the host's reason.
 *
 * A job the host cannot be told of fails at once, so the app never waits for nothing. A job
 * canceled in Android before the host answered is withdrawn there. When the agent connects
 * again, the jobs the last connection carried are failed: the host that had them is gone.
 *
 * Every method runs on the agent's one thread.
 */
class HostPrinter(
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val isConnected: () -> Boolean,
    private val post: (() -> Unit) -> Unit,
) {
    /** The jobs the host has, or is being sent, by the agent's number for them. */
    private val sent = linkedMapOf<Int, QueuedPrintJob>()
    private var lastId = 0

    /** Sends a job the print service was given to the host. */
    fun queued(job: QueuedPrintJob) {
        if (!isConnected()) {
            job.failed("DSA's host is not connected")
            return
        }
        val document = try {
            job.readDocument(MAX_DOCUMENT_BYTES)
        } catch (e: Exception) {
            log.warn("reading a document to print failed", e)
            null
        }
        if (document == null || document.isEmpty()) {
            job.failed("the document could not be read, or it is larger than ${MAX_DOCUMENT_BYTES / MIB} MiB")
            return
        }
        lastId = if (lastId == Int.MAX_VALUE) 1 else lastId + 1
        val id = lastId
        val d = job.details
        val message = PrintJob.newBuilder()
            .setJobId(id)
            .setPackageName(d.packageName)
            .setAppLabel(d.appLabel)
            .setDocumentName(d.documentName.take(MAX_NAME_CHARS))
            .setSize(document.size.toLong())
            .setPageCount(d.pageCount.coerceAtLeast(0))
            .setCopies(d.copies.coerceAtLeast(1))
            .setColor(d.color)
            .setDuplex(d.duplex)
            .setLandscape(d.landscape)
            .setMediaWidthMils(d.mediaWidthMils.coerceAtLeast(0))
            .setMediaHeightMils(d.mediaHeightMils.coerceAtLeast(0))
        job.started()
        if (!send(Envelope.newBuilder().setPrintJob(message).build().toByteArray())) {
            job.failed("the document could not be sent to DSA's host")
            return
        }
        sent[id] = job
        sendPiece(id, document, 0)
    }

    /** Sends one piece, and the next one after other work on the agent's thread. */
    private fun sendPiece(id: Int, document: ByteArray, offset: Int) {
        val job = sent[id] ?: return
        val end = minOf(document.size, offset + PIECE_BYTES)
        val piece = PrintJobChunk.newBuilder()
            .setJobId(id)
            .setOffset(offset.toLong())
            .setData(ByteString.copyFrom(document, offset, end - offset))
        if (!send(Envelope.newBuilder().setPrintJobChunk(piece).build().toByteArray())) {
            sent.remove(id)
            job.failed("the document could not be sent to DSA's host")
            return
        }
        if (end < document.size) post { sendPiece(id, document, end) }
    }

    /** Android canceled the job: the host drops it. */
    fun cancelRequested(job: QueuedPrintJob) {
        val id = sent.entries.firstOrNull { it.value === job }?.key
        if (id != null) {
            sent.remove(id)
            send(Envelope.newBuilder().setPrintJobCancel(PrintJobCancel.newBuilder().setJobId(id)).build().toByteArray())
        }
        job.canceled()
    }

    /** A new connection: the host that had the last connection's jobs is gone. */
    fun connected() {
        val lost = sent.values.toList()
        sent.clear()
        for (job in lost) job.failed("DSA's host restarted before it printed the document")
    }

    /** Ends the job in the spooler as the host says, if the frame is a print job's result. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.PRINT_JOB_RESULT) return false
        val result = envelope.printJobResult
        val job = sent.remove(result.jobId)
        if (job == null) {
            log.info("the host answered print job ${result.jobId}, which Android no longer has")
            return true
        }
        try {
            when (result.outcome) {
                PrintOutcome.PRINT_OUTCOME_PRINTED -> job.completed()
                PrintOutcome.PRINT_OUTCOME_CANCELED -> job.canceled()
                else -> job.failed(result.message.ifEmpty { "DSA's host could not print the document" })
            }
        } catch (e: Exception) {
            log.warn("ending a print job failed", e)
        }
        return true
    }

    companion object {
        private const val MIB = 1024L * 1024L

        /** The host takes no larger document. */
        const val MAX_DOCUMENT_BYTES = 128L * MIB

        /** Each piece crosses the binder to the channel daemon, whose transactions are small. */
        const val PIECE_BYTES = 256 * 1024

        const val MAX_NAME_CHARS = 256
    }
}

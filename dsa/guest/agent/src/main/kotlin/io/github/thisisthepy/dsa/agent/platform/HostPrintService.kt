package io.github.thisisthepy.dsa.agent.platform

import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.print.PrintAttributes
import android.print.PrinterCapabilitiesInfo
import android.print.PrinterId
import android.print.PrinterInfo
import android.printservice.PrintJob
import android.printservice.PrintService
import android.printservice.PrinterDiscoverySession
import io.github.thisisthepy.dsa.agent.R
import io.github.thisisthepy.dsa.agent.print.PrintDetails
import io.github.thisisthepy.dsa.agent.print.QueuedPrintJob
import java.io.ByteArrayOutputStream
import java.io.FileInputStream

/**
 * The guest's print service. It offers one printer, the host's printers, in Android's print
 * dialog; a job printed to it goes to the agent ([io.github.thisisthepy.dsa.agent.print.HostPrinter]),
 * which sends it to the host, where the user picks the printer in the host's own print dialog.
 * The print spooler calls it on the main thread, and its jobs are ended on the main thread.
 */
class HostPrintService : PrintService() {
    private val jobs = mutableMapOf<android.print.PrintJobId, AndroidPrintJob>()

    override fun onCreatePrinterDiscoverySession(): PrinterDiscoverySession = Discovery()

    override fun onPrintJobQueued(printJob: PrintJob) {
        val agent = AgentApplication.agent
        if (agent == null) {
            printJob.fail(getString(R.string.print_unavailable))
            return
        }
        val job = AndroidPrintJob(printJob) { jobs.remove(printJob.id) }
        jobs[printJob.id] = job
        agent.print(job)
    }

    override fun onRequestCancelPrintJob(printJob: PrintJob) {
        val job = jobs[printJob.id]
        val agent = AgentApplication.agent
        if (job == null || agent == null) {
            printJob.cancel()
            return
        }
        agent.cancelPrint(job)
    }

    /** Offers the one printer whenever Android looks for printers. */
    private inner class Discovery : PrinterDiscoverySession() {
        override fun onStartPrinterDiscovery(priorityList: MutableList<PrinterId>) {
            addPrinters(listOf(printer()))
        }

        override fun onStopPrinterDiscovery() = Unit

        override fun onValidatePrinters(printerIds: MutableList<PrinterId>) {
            addPrinters(listOf(printer()))
        }

        override fun onStartPrinterStateTracking(printerId: PrinterId) {
            addPrinters(listOf(printer()))
        }

        override fun onStopPrinterStateTracking(printerId: PrinterId) = Unit

        override fun onDestroy() = Unit
    }

    private fun printer(): PrinterInfo {
        val id = generatePrinterId(PRINTER)
        val capabilities = PrinterCapabilitiesInfo.Builder(id)
            .addMediaSize(PrintAttributes.MediaSize.ISO_A4, true)
            .addMediaSize(PrintAttributes.MediaSize.NA_LETTER, false)
            .addMediaSize(PrintAttributes.MediaSize.NA_LEGAL, false)
            .addMediaSize(PrintAttributes.MediaSize.ISO_A3, false)
            .addMediaSize(PrintAttributes.MediaSize.ISO_A5, false)
            .addMediaSize(PrintAttributes.MediaSize.NA_TABLOID, false)
            .addResolution(PrintAttributes.Resolution("300", "300 dpi", 300, 300), true)
            .setColorModes(
                PrintAttributes.COLOR_MODE_COLOR or PrintAttributes.COLOR_MODE_MONOCHROME,
                PrintAttributes.COLOR_MODE_COLOR,
            )
            .setDuplexModes(
                PrintAttributes.DUPLEX_MODE_NONE or PrintAttributes.DUPLEX_MODE_LONG_EDGE or
                    PrintAttributes.DUPLEX_MODE_SHORT_EDGE,
                PrintAttributes.DUPLEX_MODE_NONE,
            )
            .setMinMargins(PrintAttributes.Margins.NO_MARGINS)
            .build()
        return PrinterInfo.Builder(id, getString(R.string.print_printer_name), PrinterInfo.STATUS_IDLE)
            .setCapabilities(capabilities)
            .build()
    }

    /** A spooler job, read on the main thread when it is queued and ended there. */
    private class AndroidPrintJob(private val job: PrintJob, private val ended: () -> Unit) : QueuedPrintJob {
        private val main = Handler(Looper.getMainLooper())
        private val data: ParcelFileDescriptor? = job.document.data

        override val details: PrintDetails = run {
            val info = job.info
            val attributes = info.attributes
            val media = attributes.mediaSize
            PrintDetails(
                packageName = "",
                appLabel = "",
                documentName = info.label ?: job.document.info.name ?: "",
                pageCount = job.document.info.pageCount.coerceAtLeast(0),
                copies = info.copies.coerceAtLeast(1),
                color = attributes.colorMode != PrintAttributes.COLOR_MODE_MONOCHROME,
                duplex = attributes.duplexMode != PrintAttributes.DUPLEX_MODE_NONE &&
                    attributes.duplexMode != 0,
                landscape = media != null && !media.isPortrait,
                mediaWidthMils = media?.widthMils ?: 0,
                mediaHeightMils = media?.heightMils ?: 0,
            )
        }

        override fun readDocument(limit: Long): ByteArray? {
            val descriptor = data ?: return null
            FileInputStream(descriptor.fileDescriptor).use { input ->
                val out = ByteArrayOutputStream()
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    val read = input.read(buffer)
                    if (read < 0) break
                    if (out.size() + read > limit) return null
                    out.write(buffer, 0, read)
                }
                return out.toByteArray()
            }
        }

        override fun started() {
            main.post { if (job.isQueued) job.start() }
        }

        override fun completed() = end { job.complete() }

        override fun canceled() = end { job.cancel() }

        override fun failed(reason: String) = end { job.fail(reason) }

        private fun end(action: () -> Unit) {
            main.post {
                try {
                    if (job.isQueued) job.start()
                    if (!job.isCompleted && !job.isCancelled && !job.isFailed) action()
                } finally {
                    ended()
                    try {
                        data?.close()
                    } catch (e: Exception) {
                        // Already closed.
                    }
                }
            }
        }
    }

    private companion object {
        /** The local id of the one printer. */
        const val PRINTER = "host"
    }
}

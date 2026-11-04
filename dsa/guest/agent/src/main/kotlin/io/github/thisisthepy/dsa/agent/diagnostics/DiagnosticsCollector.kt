package io.github.thisisthepy.dsa.agent.diagnostics

import com.google.protobuf.ByteString
import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.DiagnosticsDone
import io.github.thisisthepy.dsa.control.v1.DiagnosticsFile
import io.github.thisisthepy.dsa.control.v1.Envelope

/** One entry the system keeps in its dropbox: a crash, an ANR, a tombstone, the last kernel log. */
class DropboxEntry(val tag: String, val timeMillis: Long, val data: ByteArray)

/** What the agent's part of the diagnostics is read from. */
interface DiagnosticsSource {
    /** The system's dropbox entries, oldest first. */
    fun dropbox(): List<DropboxEntry>

    /** What the system service [service] writes when dumped with [args]; null when there is none. */
    fun dump(service: String, args: List<String>): ByteArray?

    companion object {
        val NONE = object : DiagnosticsSource {
            override fun dropbox(): List<DropboxEntry> = emptyList()
            override fun dump(service: String, args: List<String>): ByteArray? = null
        }
    }
}

/**
 * The agent's part of the diagnostics the host asks for (`CollectDiagnostics`): a summary of
 * the system services' state, as `dumpsys` prints it, and the dropbox, which holds the
 * tombstones of native crashes, the traces of ANRs, the crashes of apps and the kernel's log of
 * the boot before. Each goes to the host as `DiagnosticsFile` pieces, which the control daemon
 * sends on the log port, and the part ends with `DiagnosticsDone`. Nothing is written to the
 * guest's disk; the work runs on a thread of its own, since a dump can take seconds.
 */
class DiagnosticsCollector(
    private val source: DiagnosticsSource,
    private val send: (ByteArray) -> Unit,
    private val log: Log,
    private val run: (Runnable) -> Unit = { Thread(it, "dsa-diagnostics").start() },
) {
    /** Starts gathering if the frame asks for it. False for any other frame. */
    fun handle(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        if (envelope.bodyCase != Envelope.BodyCase.COLLECT_DIAGNOSTICS) return false
        val requestId = envelope.collectDiagnostics.requestId
        run(Runnable { collect(requestId) })
        return true
    }

    /** Gathers and sends everything for request [requestId], then says it is done. */
    fun collect(requestId: Int) {
        val problems = mutableListOf<String>()
        val summary = StringBuilder()
        for ((service, args) in DUMPS) {
            val name = (listOf(service) + args).joinToString(" ")
            summary.append("-------- dumpsys ").append(name).append('\n')
            try {
                val out = source.dump(service, args)
                if (out == null) {
                    problems += "dumpsys $name: no such service"
                } else {
                    summary.append(String(out, Charsets.UTF_8)).append('\n')
                }
            } catch (e: Exception) {
                problems += "dumpsys $name: ${e.message ?: e.javaClass.simpleName}"
            }
        }
        sendFile(requestId, "android/dumpsys.txt", summary.toString().toByteArray(Charsets.UTF_8))
        try {
            var total = 0L
            val names = HashSet<String>()
            for (entry in source.dropbox()) {
                if (total + entry.data.size > MAX_DROPBOX_BYTES) {
                    problems += "the dropbox holds more than ${MAX_DROPBOX_BYTES / (1024 * 1024)} MiB; " +
                        "its oldest entries were sent"
                    break
                }
                total += entry.data.size
                var name = nameOf(entry)
                var n = 2
                while (!names.add(name)) name = "${nameOf(entry)}.${n++}"
                sendFile(requestId, name, entry.data)
            }
        } catch (e: Exception) {
            problems += "dropbox: ${e.message ?: e.javaClass.simpleName}"
        }
        send(
            Envelope.newBuilder()
                .setDiagnosticsDone(
                    DiagnosticsDone.newBuilder().setRequestId(requestId).setError(problems.joinToString("; "))
                )
                .build()
                .toByteArray()
        )
        log.info("sent the agent's part of the diagnostics" + if (problems.isEmpty()) "" else " (not gathered: ${problems.joinToString("; ")})")
    }

    private fun sendFile(requestId: Int, name: String, data: ByteArray) {
        val pieces = maxOf(1, (data.size + PIECE - 1) / PIECE)
        for (i in 0 until pieces) {
            val from = minOf(i * PIECE, data.size)
            val to = minOf(from + PIECE, data.size)
            send(
                Envelope.newBuilder()
                    .setDiagnosticsFile(
                        DiagnosticsFile.newBuilder()
                            .setRequestId(requestId)
                            .setName(name)
                            .setData(ByteString.copyFrom(data, from, to - from))
                            .setLast(i == pieces - 1)
                    )
                    .build()
                    .toByteArray()
            )
        }
    }

    companion object {
        /** The size of each piece of a file. */
        const val PIECE = 256 * 1024

        /** The most of the dropbox one archive carries. */
        const val MAX_DROPBOX_BYTES = 64L * 1024 * 1024

        /** The services and arguments of the summary: what a problem report most often needs. */
        val DUMPS: List<Pair<String, List<String>>> = listOf(
            "activity" to listOf("activities"),
            "activity" to listOf("processes"),
            "window" to listOf("windows"),
            "meminfo" to emptyList(),
            "cpuinfo" to emptyList(),
            "power" to emptyList(),
            "deviceidle" to emptyList(),
            "SurfaceFlinger" to emptyList(),
        )

        /**
         * Where a dropbox entry goes in the archive: native crashes under `android/tombstones/`,
         * ANRs under `android/anr/`, the rest under `android/dropbox/`, each named by its tag and
         * time. Only letters, digits, `_`, `-` and `.` are kept of the tag.
         */
        fun nameOf(entry: DropboxEntry): String {
            val tag = entry.tag.map { if (it.isLetterOrDigit() || it == '_' || it == '-' || it == '.') it else '_' }
                .joinToString("")
                .ifEmpty { "entry" }
                .take(100)
            val folder = when {
                tag.startsWith("SYSTEM_TOMBSTONE") || tag.endsWith("native_crash") -> "tombstones"
                tag.endsWith("_anr") -> "anr"
                else -> "dropbox"
            }
            return "android/$folder/$tag@${entry.timeMillis}.txt"
        }
    }
}

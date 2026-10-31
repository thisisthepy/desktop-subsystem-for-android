package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.GuestMemory
import io.github.thisisthepy.dsa.control.v1.ProcessMemory

/** One process's share of the guest's memory, in KiB. */
data class ProcessPss(val pid: Int, val name: String, val pssKb: Long, val swapPssKb: Long = 0)

/** Where the agent reads the guest's memory from: the kernel and the activity manager. */
interface MemorySource {
    /** The text of /proc/meminfo. */
    fun meminfo(): String

    /** The text of the zram device's mm_stat, or null when the guest has no zram. */
    fun zramMmStat(): String?

    /** The processes the activity manager runs, with their proportional set sizes. */
    fun processes(): List<ProcessPss>
}

/** The fields of /proc/meminfo the report carries, in KiB. */
data class MemInfo(
    val memTotalKb: Long = 0,
    val memFreeKb: Long = 0,
    val memAvailableKb: Long = 0,
    val buffersKb: Long = 0,
    val cachedKb: Long = 0,
    val anonPagesKb: Long = 0,
    val shmemKb: Long = 0,
    val slabReclaimableKb: Long = 0,
    val swapTotalKb: Long = 0,
    val swapFreeKb: Long = 0,
) {
    companion object {
        /**
         * Reads /proc/meminfo's `Name: value kB` lines. A line it does not know, or cannot
         * read, is skipped, so a kernel that adds or drops a line never breaks the report.
         */
        fun parse(text: String): MemInfo {
            val values = HashMap<String, Long>()
            for (line in text.lineSequence()) {
                val colon = line.indexOf(':')
                if (colon <= 0) continue
                val number = line.substring(colon + 1).trim().split(' ', '\t').firstOrNull() ?: continue
                number.toLongOrNull()?.let { values[line.substring(0, colon).trim()] = it }
            }
            fun v(name: String) = values[name] ?: 0L
            return MemInfo(
                memTotalKb = v("MemTotal"),
                memFreeKb = v("MemFree"),
                memAvailableKb = v("MemAvailable"),
                buffersKb = v("Buffers"),
                cachedKb = v("Cached"),
                anonPagesKb = v("AnonPages"),
                shmemKb = v("Shmem"),
                slabReclaimableKb = v("SReclaimable"),
                swapTotalKb = v("SwapTotal"),
                swapFreeKb = v("SwapFree"),
            )
        }
    }
}

/** What zram holds: the size of the pages stored, and the memory it uses to hold them, in KiB. */
data class ZramStat(val originalKb: Long, val usedKb: Long) {
    companion object {
        /**
         * Reads the zram device's mm_stat: byte counts separated by spaces, of which the first
         * is the size of the data stored and the third the memory used to store it. Null when
         * the text has fewer fields than that.
         */
        fun parse(text: String): ZramStat? {
            val fields = text.trim().split(Regex("\\s+")).mapNotNull { it.toLongOrNull() }
            if (fields.size < 3) return null
            return ZramStat(originalKb = fields[0] / 1024, usedKb = fields[2] / 1024)
        }
    }
}

/**
 * Builds the GuestMemory report: the kernel's totals, zram, and the processes with the
 * largest proportional set size, largest first.
 */
fun guestMemory(meminfo: MemInfo, zram: ZramStat?, processes: List<ProcessPss>): GuestMemory {
    val builder = GuestMemory.newBuilder()
        .setMemTotalKb(meminfo.memTotalKb)
        .setMemFreeKb(meminfo.memFreeKb)
        .setMemAvailableKb(meminfo.memAvailableKb)
        .setBuffersKb(meminfo.buffersKb)
        .setCachedKb(meminfo.cachedKb)
        .setAnonPagesKb(meminfo.anonPagesKb)
        .setShmemKb(meminfo.shmemKb)
        .setSlabReclaimableKb(meminfo.slabReclaimableKb)
        .setSwapTotalKb(meminfo.swapTotalKb)
        .setSwapFreeKb(meminfo.swapFreeKb)
        .setZramOriginalKb(zram?.originalKb ?: 0)
        .setZramUsedKb(zram?.usedKb ?: 0)
        .setTotalPssKb(processes.sumOf { it.pssKb })
    processes
        .sortedWith(compareByDescending<ProcessPss> { it.pssKb }.thenBy { it.pid })
        .take(MemoryReporter.TOP_PROCESSES)
        .forEach {
            builder.addTopProcesses(
                ProcessMemory.newBuilder()
                    .setPid(it.pid)
                    .setName(it.name)
                    .setPssKb(it.pssKb)
                    .setSwapPssKb(it.swapPssKb),
            )
        }
    return builder.build()
}

/**
 * Reports the guest's memory to the host: every [intervalMillis] while connected, and at once
 * when the host asks with RequestGuestMemory. A source that fails gives a report without what
 * it could not read, rather than none: the host still sees what the kernel says when the
 * activity manager does not answer.
 */
class MemoryReporter(
    private val source: MemorySource,
    private val scheduler: Scheduler,
    private val log: Log,
    private val isConnected: () -> Boolean,
    private val send: (ByteArray) -> Unit,
    private val intervalMillis: Long = INTERVAL_MILLIS,
) {
    private var generation = 0

    /** Starts the periodic reports on a new connection; the first follows after [firstDelayMillis]. */
    fun connected(firstDelayMillis: Long = FIRST_DELAY_MILLIS) {
        val mine = ++generation
        schedule(mine, firstDelayMillis)
    }

    private fun schedule(mine: Int, delay: Long) {
        scheduler.postDelayed(delay) {
            // A newer connection has its own schedule; this one ends.
            if (mine != generation || !isConnected()) return@postDelayed
            report()
            schedule(mine, intervalMillis)
        }
    }

    /** Reads the guest's memory and sends it to the host now. */
    fun report() {
        val meminfo = try {
            MemInfo.parse(source.meminfo())
        } catch (e: Exception) {
            log.warn("could not read the guest's memory totals", e)
            MemInfo()
        }
        val zram = try {
            source.zramMmStat()?.let(ZramStat::parse)
        } catch (e: Exception) {
            null
        }
        val processes = try {
            source.processes()
        } catch (e: Exception) {
            log.warn("could not read the processes' memory", e)
            emptyList()
        }
        val frame = Envelope.newBuilder().setGuestMemory(guestMemory(meminfo, zram, processes)).build()
        send(frame.toByteArray())
    }

    companion object {
        const val TOP_PROCESSES = 10

        // Often enough for the host's log to show a trend, rare enough that reading every
        // process's memory costs nothing that shows.
        const val INTERVAL_MILLIS = 5 * 60 * 1000L

        // After a connection, boot is still settling; the first report waits for it.
        const val FIRST_DELAY_MILLIS = 60 * 1000L
    }
}

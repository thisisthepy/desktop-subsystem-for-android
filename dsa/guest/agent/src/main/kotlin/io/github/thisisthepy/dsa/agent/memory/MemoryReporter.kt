package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.GuestMemory
import io.github.thisisthepy.dsa.control.v1.ProcessMemory

/**
 * One process's share of the guest's memory, in KiB. [swapPssKb] is the part of [pssKb] that
 * is swapped out to zram, at the size the pages had before compression. [googlePlay] says the
 * process belongs to Google Play services ([GooglePlayPackages]).
 */
data class ProcessPss(
    val pid: Int,
    val name: String,
    val pssKb: Long,
    val swapPssKb: Long = 0,
    val googlePlay: Boolean = false,
) {
    /**
     * [swapPssKb], never more than [pssKb]: the part swapped out is part of the PSS. A larger
     * figure is a count of every page swapped out, its share in other processes included, and
     * it made every total read "0 resident".
     */
    val swappedOutKb: Long get() = swapPssKb.coerceIn(0, pssKb.coerceAtLeast(0))
}

/**
 * Which packages the memory report counts as Google Play services, so the user sees what the
 * Google apps they installed cost apart from what Android and DSA cost.
 */
object GooglePlayPackages {
    /** Google Play services, the Play Store and the services framework, by name. */
    val CORE = setOf("com.google.android.gms", "com.android.vending", "com.google.android.gsf")

    /**
     * Whether [packageName] is one of the Google apps: Google Play itself ([CORE]), an app the
     * Google apps image brought ([fromImage]), or another system app of Google's
     * (`com.google.android.*`), which only that image puts in the system. A Google app the user
     * installed from the store is not a system app and counts as the user's own.
     */
    fun isGooglePlay(packageName: String, system: Boolean, fromImage: Boolean): Boolean =
        fromImage || packageName in CORE || (system && packageName.startsWith("com.google.android."))
}

/** Where the agent reads the guest's memory from: the kernel and the activity manager. */
interface MemorySource {
    /** The text of /proc/meminfo. */
    fun meminfo(): String

    /** The text of the zram device's mm_stat, or null when the guest has no zram. */
    fun zramMmStat(): String?

    /** The processes the activity manager runs, with their proportional set sizes. */
    fun processes(): List<ProcessPss>

    /** The memory the kernel was given, in KiB, counted from its online memory blocks; null when unreadable. */
    fun memOnlineKb(): Long? = null

    /** What free page reporting has handed back to the host, from DSA's kernel; null when unreadable. */
    fun pageReporting(): PageReporting? = null
}

/**
 * Free page reporting, in KiB, as DSA's kernel counts it (`page_reporting.*`): the free memory
 * the host has taken back now, the memory reported since boot, and the reported free memory
 * the host took back in with a neighbour in its fault cluster since boot.
 */
data class PageReporting(val freeReportedKb: Long, val reportedTotalKb: Long, val refaultedTotalKb: Long)

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
    val slabUnreclaimableKb: Long = 0,
    val kernelStackKb: Long = 0,
    val pageTablesKb: Long = 0,
    val vmallocUsedKb: Long = 0,
    val percpuKb: Long = 0,
    val cmaTotalKb: Long = 0,
    val hugetlbKb: Long = 0,
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
                slabUnreclaimableKb = v("SUnreclaim"),
                kernelStackKb = v("KernelStack"),
                pageTablesKb = v("PageTables"),
                vmallocUsedKb = v("VmallocUsed"),
                percpuKb = v("Percpu"),
                cmaTotalKb = v("CmaTotal"),
                hugetlbKb = v("Hugetlb").takeIf { it > 0 } ?: (v("HugePages_Total") * v("Hugepagesize")),
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
fun guestMemory(
    meminfo: MemInfo,
    zram: ZramStat?,
    processes: List<ProcessPss>,
    memOnlineKb: Long? = null,
    pageReporting: PageReporting? = null,
): GuestMemory {
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
        .setSwapPssKb(processes.sumOf { it.swappedOutKb })
        .setSlabUnreclaimableKb(meminfo.slabUnreclaimableKb)
        .setKernelStackKb(meminfo.kernelStackKb)
        .setPageTablesKb(meminfo.pageTablesKb)
        .setVmallocUsedKb(meminfo.vmallocUsedKb)
        .setPercpuKb(meminfo.percpuKb)
        .setCmaTotalKb(meminfo.cmaTotalKb)
        .setHugetlbKb(meminfo.hugetlbKb)
        .setMemOnlineKb(memOnlineKb ?: 0)
        .setFreeReportedKb(pageReporting?.freeReportedKb ?: 0)
        .setReportedTotalKb(pageReporting?.reportedTotalKb ?: 0)
        .setRefaultedTotalKb(pageReporting?.refaultedTotalKb ?: 0)
    // Every process of Google Play services, not only those among the largest: together they
    // are what Google Play costs.
    val googlePlay = processes.filter { it.googlePlay }
    builder
        .setGooglePlayProcesses(googlePlay.size)
        .setGooglePlayPssKb(googlePlay.sumOf { it.pssKb })
        .setGooglePlaySwapPssKb(googlePlay.sumOf { it.swappedOutKb })
    processes
        .sortedWith(compareByDescending<ProcessPss> { it.pssKb }.thenBy { it.pid })
        .take(MemoryReporter.TOP_PROCESSES)
        .forEach {
            builder.addTopProcesses(
                ProcessMemory.newBuilder()
                    .setPid(it.pid)
                    .setName(it.name)
                    .setPssKb(it.pssKb)
                    .setSwapPssKb(it.swappedOutKb)
                    .setGooglePlay(it.googlePlay),
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
        val memOnline = try {
            source.memOnlineKb()
        } catch (e: Exception) {
            null
        }
        val pageReporting = try {
            source.pageReporting()
        } catch (e: Exception) {
            null
        }
        val processes = try {
            source.processes()
        } catch (e: Exception) {
            log.warn("could not read the processes' memory", e)
            emptyList()
        }
        val frame = Envelope.newBuilder()
            .setGuestMemory(guestMemory(meminfo, zram, processes, memOnline, pageReporting))
            .build()
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

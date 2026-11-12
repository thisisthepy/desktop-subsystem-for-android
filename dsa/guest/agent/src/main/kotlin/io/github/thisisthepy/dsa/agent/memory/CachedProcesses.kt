package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/**
 * One app process as the activity manager lists it. [state] is the framework's process state
 * (the values of ActivityManager's PROCESS_STATE_* constants), [hasActivities] whether the
 * process holds any activity, and [persistent] whether the framework keeps it running.
 */
data class AppProcess(
    val pid: Int,
    val uid: Int,
    val name: String,
    val packages: List<String>,
    val state: Int,
    val persistent: Boolean = false,
    val hasActivities: Boolean = false,
)

/**
 * What the activity manager's own record of a process says, as `dumpsys activity processes`
 * prints it: the packages it runs ([packages]), the packages whose code it loaded
 * ([dependencies]), and the out-of-memory score it last applied ([setAdj]; null when the record
 * did not say).
 */
data class ProcessRecord(
    val pid: Int,
    val uid: Int,
    val name: String,
    val packages: Set<String>,
    val dependencies: Set<String>,
    val setAdj: Int?,
    val persistent: Boolean,
)

/** The guest's app processes, as far as ending idle cached ones needs them. */
interface BackgroundProcesses {
    /** The app processes running now, from the activity manager's list. */
    fun running(): List<AppProcess>

    /** The activity manager's records of every process; null when they could not be read. */
    fun records(): List<ProcessRecord>?

    /** The memory of [pids] (PSS and its swapped-out part); a process it cannot read is left out. */
    fun memory(pids: List<Int>): List<ProcessPss>

    /** The packages that run a foreground service now. */
    fun foregroundServicePackages(): Set<String>

    /**
     * Has the activity manager end the background processes of [packageName], as
     * `ActivityManager.killBackgroundProcesses` does: those of its own and those that loaded
     * its code, whose score is at least the service level. Persistent processes are never among
     * them.
     */
    fun endBackground(packageName: String)
}

/**
 * Reads the records of `dumpsys activity processes`: each process's block starts with a line
 * `*APP* UID <uid> ProcessRecord{<hash> <pid>:<name>/<user>}` (`*PERS*` for a persistent
 * process) and holds `packageList={...}`, `packageDependencies={...}` and
 * `oom adj: ... set=<score>` lines. Null when no record is found, so text that changed shape
 * reads as unknown rather than as a guest without processes.
 */
object ProcessRecords {
    private val header = Regex("""^\s*\*(APP|PERS)\*\s+UID\s+(\d+)\s+ProcessRecord\{\S+\s+(\d+):([^/}\s]+)""")
    private val packageList = Regex("""^\s+packageList=\{([^}]*)\}""")
    private val dependencyList = Regex("""^\s+packageDependencies=\{([^}]*)\}""")
    private val oomAdj = Regex("""^\s+oom adj:.*\bset=(-?\d+)""")

    fun parse(text: String): List<ProcessRecord>? {
        val records = mutableListOf<ProcessRecord>()
        var current: ProcessRecord? = null
        for (line in text.lineSequence()) {
            val start = header.find(line)
            if (start != null) {
                current?.let { records.add(it) }
                val (kind, uid, pid, name) = start.destructured
                current = ProcessRecord(
                    pid = pid.toInt(),
                    uid = uid.toInt(),
                    name = name,
                    packages = emptySet(),
                    dependencies = emptySet(),
                    setAdj = null,
                    persistent = kind == "PERS",
                )
                continue
            }
            val record = current ?: continue
            // A record's lines are indented under its header; a line back at the section's
            // indent (the next section's title) ends the last one.
            if (line.isNotBlank() && !line.startsWith("    ")) {
                records += record
                current = null
                continue
            }
            packageList.find(line)?.let { current = record.copy(packages = names(it.groupValues[1])) }
            dependencyList.find(line)?.let { current = record.copy(dependencies = names(it.groupValues[1])) }
            oomAdj.find(line)?.let { current = record.copy(setAdj = it.groupValues[1].toIntOrNull()) }
        }
        current?.let { records.add(it) }
        return records.ifEmpty { null }
    }

    private fun names(list: String): Set<String> =
        list.split(',').map { it.trim() }.filter { it.isNotEmpty() }.toSet()
}

/**
 * Ends app processes that stay cached after their work is done, instead of keeping them until
 * the guest runs short of memory, which an idle guest never does.
 *
 * Google Play and other apps start processes for one-off work (a scheduled job, a bound
 * service another app used, an install check) and the activity manager keeps them cached when
 * the work ends. Its own limit ends only cached processes beyond its count, so a handful stays
 * for good: on a desktop that is memory the host never gets back. Here a cached process that
 * holds no activity and whose packages have no task is ended once it has stayed cached for
 * [idleMillis], whether Android dozes or windows are open. A process that comes back soon
 * after it was ended waits twice as long the next time, up to [MAX_IDLE_MILLIS], so work that
 * keeps restarting does not cost a process start every few seconds.
 *
 * Never ended: a persistent process, the system's own, the agent, Google Play services' main
 * processes ([KEPT_PROCESSES], which keep push messages arriving), a process whose packages
 * have a task (a shown or minimized window) or run a foreground service, and any process that
 * is not cached.
 *
 * The activity manager ends processes per package, not per process: ending one package's
 * background processes also ends every process that loaded the package's code and scores at
 * least the service level (500). So before a package's processes are ended, the activity
 * manager's own records are read, and the package is ended only when everything that call
 * would reach (with a margin, everything scoring [REACH_ADJ] or more) is a process this policy
 * would end anyway.
 * When something else would be reached, the process is kept and looked at again after
 * [BLOCKED_RETRY_MILLIS].
 *
 * Each process ended is logged with its memory: `ended cached <name> (pid N, cached T s, no
 * task window): PSS P MiB, R MiB resident and S MiB swapped out to zram`.
 *
 * Not thread safe: every method runs on the agent's scheduler thread.
 */
class CachedProcesses(
    private val processes: BackgroundProcesses,
    private val scheduler: Scheduler,
    private val log: Log,
    private val clock: () -> Long,
    private val isConnected: () -> Boolean,
    private val ownPackage: String,
    private val intervalMillis: Long = INTERVAL_MILLIS,
    private val idleMillis: Long = IDLE_MILLIS,
) {
    private class Cached(val name: String, val since: Long)

    private class Ended(var at: Long, var streak: Int)

    /** Packages with a task, as the agent last read the tasks. */
    private var withTasks: Set<String> = emptySet()

    /** Since when each cached process (by pid) has been cached, as far as this has seen. */
    private val cached = HashMap<Int, Cached>()

    /** When each process name was last ended, and how many times in a row it came back soon. */
    private val ended = HashMap<String, Ended>()

    /** Processes kept because ending them would reach others, and when to look at them again. */
    private val blocked = HashMap<Int, Long>()

    private var unreadable = false

    /** Starts looking at the processes every [intervalMillis], the first time after [firstDelayMillis]. */
    fun start(firstDelayMillis: Long = FIRST_DELAY_MILLIS) {
        scheduler.postDelayed(firstDelayMillis) { tick() }
    }

    private fun tick() {
        try {
            check()
        } catch (e: Exception) {
            log.warn("looking for idle cached processes failed", e)
        }
        scheduler.postDelayed(intervalMillis) { tick() }
    }

    /** Takes the packages that have a task now. */
    fun tasks(packages: Set<String>) {
        withTasks = packages
    }

    /** Looks at the processes once, and ends those that have been cached long enough. */
    fun check() {
        // Without the host the agent does not hear of the tasks, so which processes have a
        // window is not known.
        if (!isConnected()) return
        val now = clock()
        val running = try {
            processes.running()
        } catch (e: Exception) {
            warnOnce("could not list the app processes", e)
            return
        }
        follow(running, now)
        val idle = running.filter { endable(it) && cachedFor(it, now) >= idleFor(it.name, now) }
            .filter { (blocked[it.pid] ?: Long.MIN_VALUE) <= now }
        if (idle.isEmpty()) return
        val foreground = try {
            processes.foregroundServicePackages()
        } catch (e: Exception) {
            warnOnce("could not read which apps run a foreground service", e)
            return
        }
        val ready = idle.filter { process -> process.packages.none { it in foreground } }
        // An app with a foreground service keeps all its processes; it is looked at again later.
        for (process in idle - ready.toSet()) blocked[process.pid] = now + BLOCKED_RETRY_MILLIS
        if (ready.isEmpty()) return
        val records = try {
            processes.records()
        } catch (e: Exception) {
            null
        }
        if (records == null) {
            warnOnce("could not read the activity manager's process records")
            return
        }
        unreadable = false
        val byPid = running.associateBy { it.pid }
        // Every process this policy may end now, idle long enough or not: ending one of them
        // a little early along with another is no harm, ending anything else is.
        val endableNow = running.filter { process ->
            endable(process) && process.packages.none { it in foreground } &&
                records.firstOrNull { it.pid == process.pid }?.let { it.setAdj != null && it.setAdj >= CACHED_ADJ } == true
        }.associateBy { it.pid }
        val gone = HashSet<Int>()
        for (process in ready.sortedBy { it.pid }) {
            if (process.pid in gone || process.pid !in endableNow) continue
            var blocker: ProcessRecord? = null
            val packageName = process.packages.firstOrNull { candidate ->
                val reached = reach(candidate, process.uid, records)
                val other = reached.firstOrNull { it.pid !in endableNow }
                if (other != null && blocker == null) blocker = other
                other == null && reached.any { it.pid == process.pid }
            }
            if (packageName == null) {
                blocked[process.pid] = now + BLOCKED_RETRY_MILLIS
                val other = blocker
                val reason = if (other == null) {
                    "the activity manager's record of it names none of its packages"
                } else {
                    "ending it would also end ${other.name} (pid ${other.pid}, score ${other.setAdj ?: "unknown"})"
                }
                log.info("kept cached ${process.name} (pid ${process.pid}): $reason")
                continue
            }
            val reached = reach(packageName, process.uid, records).map { it.pid }
            val memory = try {
                processes.memory(reached).associateBy { it.pid }
            } catch (e: Exception) {
                emptyMap<Int, ProcessPss>()
            }
            try {
                processes.endBackground(packageName)
            } catch (e: Exception) {
                log.warn("could not end the cached processes of $packageName", e)
                blocked[process.pid] = now + BLOCKED_RETRY_MILLIS
                continue
            }
            for (pid in reached) {
                gone += pid
                val target = byPid[pid] ?: continue
                noteEnded(target.name, now)
                log.info(line(target, memory[pid], now))
                cached.remove(pid)
            }
        }
    }

    /** Notes since when each process has been cached, and forgets the ones gone. */
    private fun follow(running: List<AppProcess>, now: Long) {
        val present = running.associateBy { it.pid }
        cached.keys.retainAll(present.keys)
        blocked.keys.retainAll(present.keys)
        for (process in running) {
            if (process.state >= STATE_CACHED_ACTIVITY) {
                val seen = cached[process.pid]
                if (seen == null || seen.name != process.name) cached[process.pid] = Cached(process.name, now)
            } else {
                cached.remove(process.pid)
                blocked.remove(process.pid)
            }
        }
    }

    /** Whether the policy may end [process] at all, however long it has been cached. */
    private fun endable(process: AppProcess): Boolean =
        process.state >= STATE_CACHED_ACTIVITY &&
            !process.persistent &&
            !process.hasActivities &&
            process.packages.isNotEmpty() &&
            process.name != SYSTEM_PROCESS &&
            process.name !in KEPT_PROCESSES &&
            process.packages.none { it == ownPackage || it in withTasks }

    private fun cachedFor(process: AppProcess, now: Long): Long =
        cached[process.pid]?.let { now - it.since } ?: 0L

    /** How long [name] must stay cached before it is ended: longer when it keeps coming back. */
    private fun idleFor(name: String, now: Long): Long {
        val last = ended[name] ?: return idleMillis
        if (now - last.at >= CHURN_WINDOW_MILLIS) return idleMillis
        return (idleMillis shl last.streak.coerceAtMost(MAX_STREAK)).coerceAtMost(MAX_IDLE_MILLIS.coerceAtLeast(idleMillis))
    }

    private fun noteEnded(name: String, now: Long) {
        val last = ended[name]
        if (last != null && now - last.at < CHURN_WINDOW_MILLIS) {
            last.streak++
            last.at = now
        } else {
            ended[name] = Ended(now, 1)
        }
    }

    /**
     * The processes that ending [packageName]'s background processes could reach: those that
     * run it (in its app's uid) or loaded its code, and that are not persistent, at [REACH_ADJ]
     * or above. A record without a score counts as reached.
     */
    private fun reach(packageName: String, uid: Int, records: List<ProcessRecord>): List<ProcessRecord> =
        records.filter { record ->
            !record.persistent &&
                (record.setAdj == null || record.setAdj >= REACH_ADJ) &&
                ((packageName in record.packages && appId(record.uid) == appId(uid)) || packageName in record.dependencies)
        }

    private fun line(process: AppProcess, memory: ProcessPss?, now: Long): String {
        val seconds = cachedFor(process, now) / 1000
        val head = "ended cached ${process.name} (pid ${process.pid}, cached $seconds s, no task window)"
        if (memory == null) return "$head: memory unknown"
        val pss = memory.pssKb / 1024
        val swapped = memory.swappedOutKb / 1024
        val resident = (memory.pssKb - memory.swappedOutKb).coerceAtLeast(0) / 1024
        return "$head: PSS $pss MiB, $resident MiB resident and $swapped MiB swapped out to zram"
    }

    private fun warnOnce(message: String, error: Throwable? = null) {
        if (unreadable) return
        unreadable = true
        log.warn("$message; idle cached processes stay until memory runs short", error)
    }

    companion object {
        /** How often the processes are looked at. */
        const val INTERVAL_MILLIS = 15_000L

        /**
         * How long a process stays cached before it is ended: past the quiet spell after a
         * job or a bound call ends, in which the same work often comes back.
         */
        const val IDLE_MILLIS = 30_000L

        /** The longest a process that keeps coming back waits before it is ended again. */
        const val MAX_IDLE_MILLIS = 8 * 60_000L

        /** A process that comes back within this long of being ended counts as coming back soon. */
        const val CHURN_WINDOW_MILLIS = 15 * 60_000L

        private const val MAX_STREAK = 8

        /** How long a process kept because ending it would reach others waits to be looked at again. */
        const val BLOCKED_RETRY_MILLIS = 5 * 60_000L

        /** Boot starts and caches many processes at once; the first look waits until it settles. */
        const val FIRST_DELAY_MILLIS = 2 * 60_000L

        /** ActivityManager.PROCESS_STATE_CACHED_ACTIVITY: from here on a process is cached. */
        const val STATE_CACHED_ACTIVITY = 16

        /** The activity manager's lowest score of a cached process (CACHED_APP_MIN_ADJ). */
        const val CACHED_ADJ = 900

        /**
         * The score from which a process counts as reachable when a package's background
         * processes are ended. The activity manager itself goes down to the service level
         * (500); this goes down to the perceptible level's lowest (250), so a process whose
         * score is moving at that moment is not caught either.
         */
        const val REACH_ADJ = 250

        /** The system server's process. */
        const val SYSTEM_PROCESS = "system"

        /**
         * Google Play services' main processes: they hold the push connection that delivers
         * every app's messages, and the work other apps start through them.
         */
        val KEPT_PROCESSES = setOf(
            "com.google.android.gms",
            "com.google.android.gms.persistent",
            "com.google.process.gapps",
        )

        private const val PER_USER_RANGE = 100_000

        private fun appId(uid: Int): Int = uid % PER_USER_RANGE
    }
}

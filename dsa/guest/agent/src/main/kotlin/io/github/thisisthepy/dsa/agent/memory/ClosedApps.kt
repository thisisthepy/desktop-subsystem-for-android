package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/**
 * The guest's memory as the kernel counts it (/proc/meminfo), in KiB: all it has (MemTotal,
 * which falls when the host takes memory back from the VM), what is free (MemFree, which free
 * page reporting hands to the host) and what could be had without swapping (MemAvailable).
 */
data class GuestMemInfo(val totalKb: Long, val freeKb: Long, val availableKb: Long)

/**
 * What one app's running processes hold: their IDs, their PSS together and the part of it
 * swapped out to zram, in KiB. Memory the app holds in the host's graphics renderer (its
 * textures and buffers) is the host's and not in it.
 */
data class PackageMemory(val pids: List<Int>, val pssKb: Long, val swappedOutKb: Long)

/** The guest's app processes, as far as ending a closed app needs them. */
interface AppProcesses {
    /** Whether the app runs a foreground service: music, a call, a download the user sees. */
    fun hasForegroundService(packageName: String): Boolean

    /**
     * Whether a process of the app is visible to the user or in front: one of its activities
     * shows, in whichever task, even behind a dialog.
     */
    fun showsActivity(packageName: String): Boolean = false

    /** Ends every process of the app, as "am force-stop" does. */
    fun stop(packageName: String)

    /** The guest's memory now; null when it cannot be read. */
    fun memory(): GuestMemInfo?

    /**
     * The app's running processes (the package's own and `<package>:<name>`) and what they
     * hold; empty when none runs, null when that cannot be read.
     */
    fun packageMemory(packageName: String): PackageMemory? = null

    /** Turns the app off if it is installed and on. True when it was turned off now. */
    fun disable(packageName: String): Boolean

    /** Keeps at most [max] cached app processes, however much memory is free. */
    fun limitCachedProcesses(max: Int)

    companion object {
        val NONE = object : AppProcesses {
            override fun hasForegroundService(packageName: String) = false
            override fun stop(packageName: String) = Unit
            override fun memory(): GuestMemInfo? = null
            override fun disable(packageName: String) = false
            override fun limitCachedProcesses(max: Int) = Unit
        }
    }
}

/**
 * Ends an app's processes as soon as the user closes its last task window on the host
 * ([closing], then the task's end), and never for a task that ended any other way. Right
 * before the stop it looks again: an app with a window, an activity in a task, or one that
 * shows is kept. Otherwise this ends them instead of leaving them
 * cached as a phone does: on a desktop a closed window means the user is done with the app,
 * and memory a cached process holds is memory the host does not get back. The guest's
 * control daemon sees the same task end and drops the caches a moment later, so what the
 * processes held goes back to the host within seconds.
 *
 * An app stays when the user still expects it to run: one with a widget placed on the
 * desktop, or one running a foreground service (playing music, for example). The host's Stop
 * ([stop] with `force`) ends even those.
 *
 * What the app held is read before anything ends it: when the host closes the app's last
 * window ([closing], before the task is removed, since removing it already ends the app's
 * processes), or else just before the processes are ended. [settleMillis] later one line says
 * what went: `closed <package>: its N processes held P MiB (PSS, Z MiB of it in zram) and
 * ended; the guest's free memory rose F MiB (available memory A MiB) in T ms, counted from the
 * window's close`. When the VM itself shrank meanwhile (the host took memory back), that part
 * is named too, since memory the host took is not free in the guest any more. A process of the
 * app still running then is named with its ID. The app's memory in the host's graphics renderer
 * is not in these figures: the renderer frees it as the app's connections close and logs that.
 *
 * Not thread safe: the agent calls it from its scheduler's thread.
 */
class ClosedApps(
    private val processes: AppProcesses,
    private val hasPlacedWidget: (String) -> Boolean,
    private val scheduler: Scheduler,
    private val log: Log,
    private val clock: () -> Long,
    private val settleMillis: Long = SETTLE_MILLIS,
) {
    private var open: Set<String> = emptySet()

    /** What an app held when its last window began to close, by package. */
    private class Before(val at: Long, val guest: GuestMemInfo?, val app: PackageMemory?, val fromClose: Boolean)

    private val closing = HashMap<String, Before>()

    /**
     * Sets the guest up for a desktop at the agent's start: few cached processes, and the
     * phone-only apps that start by themselves turned off.
     */
    fun configure() {
        try {
            processes.limitCachedProcesses(MAX_CACHED_PROCESSES)
            log.info("the activity manager keeps at most $MAX_CACHED_PROCESSES cached processes")
        } catch (e: Exception) {
            log.warn("could not limit the cached processes", e)
        }
        for (packageName in DESKTOP_UNNEEDED) {
            try {
                if (processes.disable(packageName)) log.info("turned off $packageName, which a desktop does not use")
            } catch (e: Exception) {
                log.warn("could not turn off $packageName", e)
            }
        }
    }

    /**
     * The host is closing [packageName]'s last task window: what the app and the guest hold is
     * read now, before the task's removal ends the app's processes, so the line logged once the
     * app has ended counts what the close gave back.
     */
    fun closing(packageName: String) {
        closing.keys.removeAll { closing.getValue(it).at + BEFORE_KEPT_MILLIS < clock() }
        closing[packageName] = Before(clock(), guestOrNull(), appOrNull(packageName), fromClose = true)
    }

    /**
     * Takes the packages that have a task window now, and [inTasks], those with an activity in
     * any running task (another app's task, or their own under a dialog of another app that
     * the framework now counts as its root); ends those that had either and have neither.
     */
    fun tasks(packages: Set<String>, inTasks: Set<String> = emptySet()) {
        val now = packages + inTasks
        val closed = open - now
        open = now
        for (packageName in closed) {
            // Only the user's close of the app's last window ends it. A task that ended by
            // itself (a dialog the app answered, an activity that finished, a task the framework
            // renamed or moved between displays for a moment) says nothing about whether the
            // user is done with the app; its idle processes go with the cached process limit.
            if (!closedByHost(packageName)) {
                log.info("closed $packageName: its task ended without the user closing its window, so it is not stopped")
                continue
            }
            stop(packageName, force = false)
        }
        // A close that left the app a window (another task of it) is forgotten.
        closing.keys.retainAll { it !in now }
    }

    /** Whether the host closed [packageName]'s last window a moment ago. */
    private fun closedByHost(packageName: String): Boolean =
        closing[packageName]?.let { it.at + BEFORE_KEPT_MILLIS >= clock() } == true

    /** How many times an app that still showed an activity has been looked at again, by package. */
    private val rechecks = HashMap<String, Int>()

    /**
     * Ends [packageName]'s processes and logs what that gave back. Without [force] an app with
     * a placed widget or a foreground service is kept. Returns whether it was ended.
     */
    fun stop(packageName: String, force: Boolean): Boolean {
        if (!force) {
            val keep = try {
                when {
                    hasPlacedWidget(packageName) -> "it has a widget on the desktop"
                    processes.hasForegroundService(packageName) -> "it runs a foreground service"
                    else -> null
                }
            } catch (e: Exception) {
                "whether it is still in use could not be read"
            }
            if (keep != null) {
                closing.remove(packageName)
                log.info("closed $packageName: kept running because $keep")
                return false
            }
            if (shows(packageName)) {
                keepWhileShown(packageName)
                return false
            }
            // Looked at again right before the stop: a window or an activity in a task that
            // came back meanwhile (the app reopened) keeps it.
            if (packageName in open) {
                rechecks.remove(packageName)
                closing.remove(packageName)
                log.info("closed $packageName: not stopped because it has a window or an activity in a task again")
                return false
            }
        }
        rechecks.remove(packageName)
        val before = closing.remove(packageName)?.takeIf { it.at + BEFORE_KEPT_MILLIS >= clock() }
            ?: Before(clock(), guestOrNull(), appOrNull(packageName), fromClose = false)
        val why = if (force) "the host asked to stop it" else "it has no task window and no activity in any task"
        log.info("force-stopping $packageName: $why")
        try {
            processes.stop(packageName)
        } catch (e: Exception) {
            log.warn("closed $packageName: its processes could not be ended", e)
            return false
        }
        scheduler.postDelayed(settleMillis) {
            val left = appOrNull(packageName)
            log.info(line(packageName, before, guestOrNull(), left))
            // A process that outlived the end (one the system started again for a broadcast, or
            // one that was starting as the app was ended) is ended once more, unless the app has
            // a window again, an activity in a task, or one that shows.
            if (!left?.pids.isNullOrEmpty() && packageName !in open) {
                if (shows(packageName)) {
                    log.info("closed $packageName: its process ${left!!.pids.joinToString(", ")} outlived the end but shows an activity, so it keeps running")
                } else {
                    log.info("force-stopping $packageName again: process ${left!!.pids.joinToString(", ")} outlived the end and the app has no window")
                    try {
                        processes.stop(packageName)
                    } catch (e: Exception) {
                        log.warn("closed $packageName: its processes could not be ended again", e)
                    }
                }
            }
        }
        return true
    }

    /** Whether [packageName] shows an activity; true when that cannot be read, so it is kept. */
    private fun shows(packageName: String): Boolean = try {
        processes.showsActivity(packageName)
    } catch (e: Exception) {
        true
    }

    /**
     * Keeps [packageName], which has no task window but still shows an activity, and looks
     * again [settleMillis] later: a window the user just closed stops showing within moments,
     * and the app is ended then; one the user still sees keeps it running.
     */
    private fun keepWhileShown(packageName: String) {
        closing.remove(packageName)
        val tries = (rechecks[packageName] ?: 0) + 1
        if (tries > MAX_RECHECKS) {
            rechecks.remove(packageName)
            log.info("closed $packageName: kept running because it still shows an activity")
            return
        }
        rechecks[packageName] = tries
        log.info("closed $packageName: not stopped yet because it still shows an activity; looking again in $settleMillis ms")
        scheduler.postDelayed(settleMillis) {
            if (packageName in open) rechecks.remove(packageName) else stop(packageName, force = false)
        }
    }

    /** The line that says what ending [packageName] gave back, from [before] to [guest] and [left]. */
    private fun line(packageName: String, before: Before, guest: GuestMemInfo?, left: PackageMemory?): String {
        val held = before.app
        val ended = when {
            held == null -> "what its processes held could not be read"
            held.pids.isEmpty() -> "none of its processes was running"
            else -> "its ${held.pids.size} process${if (held.pids.size == 1) "" else "es"} held " +
                "${held.pssKb / 1024} MiB (PSS, ${held.swappedOutKb / 1024} MiB of it in zram) and ended"
        }
        val counted = if (before.fromClose) "counted from the window's close" else "counted from the end of its processes"
        val then = before.guest
        val change = if (then == null || guest == null) {
            "the guest's memory could not be read"
        } else {
            val free = (guest.freeKb - then.freeKb) / 1024
            val available = (guest.availableKb - then.availableKb) / 1024
            val shrunk = (then.totalKb - guest.totalKb) / 1024
            val vm = if (shrunk > 0) ", and the host took $shrunk MiB back from the VM" else ""
            "the guest's free memory ${if (free >= 0) "rose" else "fell"} ${kotlin.math.abs(free)} MiB " +
                "(available memory ${if (available >= 0) "+" else ""}$available MiB)$vm"
        }
        val survivors = left?.pids?.takeIf { it.isNotEmpty() }
            ?.let { "; still running: pid ${it.joinToString(", ")}" } ?: ""
        return "closed $packageName: $ended; $change in ${clock() - before.at} ms, $counted$survivors"
    }

    private fun guestOrNull(): GuestMemInfo? = try {
        processes.memory()
    } catch (e: Exception) {
        null
    }

    private fun appOrNull(packageName: String): PackageMemory? = try {
        processes.packageMemory(packageName)
    } catch (e: Exception) {
        null
    }

    companion object {
        /** How long what an app held as its window began to close stands for its end. */
        const val BEFORE_KEPT_MILLIS = 30_000L

        /**
         * How long after ending an app its memory is counted: long enough for the processes to
         * die and the control daemon's cache drop (about two seconds after the task ends) to
         * finish.
         */
        const val SETTLE_MILLIS = 4000L

        /** How many times an app with no window that still shows an activity is looked at again. */
        const val MAX_RECHECKS = 3

        /**
         * Cached processes the activity manager keeps: a phone keeps 32 so apps resume fast,
         * but here a closed window ends its app, and what stays cached is services and
         * providers started in the background. The activity manager keeps half of this many
         * empty ones for good, so [CachedProcesses] ends those that stay idle.
         */
        const val MAX_CACHED_PROCESSES = 8

        /**
         * Apps the build has that start by themselves and do nothing on a desktop:
         * Cuttlefish's host monitoring service, which reports to a Cuttlefish launcher, and a
         * phone carrier's device financing lock. The image removes or withdraws them too; this
         * covers a build that still has them.
         */
        val DESKTOP_UNNEEDED = listOf(
            "com.android.google.gce.gceservice",
            "com.android.devicelockcontroller",
        )
    }
}

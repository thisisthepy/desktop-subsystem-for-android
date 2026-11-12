package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/** The guest's app processes, as far as ending a closed app needs them. */
interface AppProcesses {
    /** Whether the app runs a foreground service: music, a call, a download the user sees. */
    fun hasForegroundService(packageName: String): Boolean

    /** Ends every process of the app, as "am force-stop" does. */
    fun stop(packageName: String)

    /** The guest's free memory now, in KiB (MemFree), which is what the host can get back. */
    fun freeKb(): Long

    /** Turns the app off if it is installed and on. True when it was turned off now. */
    fun disable(packageName: String): Boolean

    /** Keeps at most [max] cached app processes, however much memory is free. */
    fun limitCachedProcesses(max: Int)

    companion object {
        val NONE = object : AppProcesses {
            override fun hasForegroundService(packageName: String) = false
            override fun stop(packageName: String) = Unit
            override fun freeKb() = 0L
            override fun disable(packageName: String) = false
            override fun limitCachedProcesses(max: Int) = Unit
        }
    }
}

/**
 * Ends an app's processes as soon as its last task window closes, instead of leaving them
 * cached as a phone does: on a desktop a closed window means the user is done with the app,
 * and memory a cached process holds is memory the host does not get back. The guest's
 * control daemon sees the same task end and drops the caches a moment later, so what the
 * processes held goes back to the host within seconds.
 *
 * An app stays when the user still expects it to run: one with a widget placed on the
 * desktop, or one running a foreground service (playing music, for example). The host's Stop
 * ([stop] with `force`) ends even those.
 *
 * Each end is logged as `closed <package>: returned N MiB in T ms`, N being how much more
 * memory is free [settleMillis] later than just before.
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

    /**
     * Sets the guest up for a desktop at the agent's start: few cached processes, and the
     * phone-only apps that start by themselves turned off.
     */
    fun configure() {
        try {
            processes.limitCachedProcesses(MAX_CACHED_PROCESSES)
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

    /** Takes the packages that have a task window now; ends those that had one and have not. */
    fun tasks(packages: Set<String>) {
        val closed = open - packages
        open = packages
        for (packageName in closed) stop(packageName, force = false)
    }

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
                log.info("closed $packageName: kept running because $keep")
                return false
            }
        }
        val before = freeOrNull()
        val started = clock()
        try {
            processes.stop(packageName)
        } catch (e: Exception) {
            log.warn("closed $packageName: its processes could not be ended", e)
            return false
        }
        scheduler.postDelayed(settleMillis) {
            val after = freeOrNull()
            val returned = if (before != null && after != null) ((after - before).coerceAtLeast(0) / 1024).toString() else "?"
            log.info("closed $packageName: returned $returned MiB in ${clock() - started} ms")
        }
        return true
    }

    private fun freeOrNull(): Long? = try {
        processes.freeKb()
    } catch (e: Exception) {
        null
    }

    companion object {
        /**
         * How long after ending an app its memory is counted: long enough for the processes to
         * die and the control daemon's cache drop (about two seconds after the task ends) to
         * finish.
         */
        const val SETTLE_MILLIS = 4000L

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

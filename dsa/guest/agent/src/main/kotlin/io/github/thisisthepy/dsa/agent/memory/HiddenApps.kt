package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/**
 * Trims the apps the user cannot see. On a phone the framework sends an app
 * TRIM_MEMORY_UI_HIDDEN when its UI goes to the background, and lower levels as it sinks; here
 * every task is a freeform window that stays "visible" to Android while its host window is
 * minimized, covered by other windows or on another virtual desktop, so the framework never
 * does. The host says which task windows the user can see (`TasksShown`) and whether any is
 * shown at all (`ScreenState`); an app none of whose tasks is seen is hidden.
 *
 * Once an app has been hidden for [HIDDEN_AFTER_MILLIS] it is sent TRIM_MEMORY_UI_HIDDEN, on
 * which its UI toolkit and ART let their caches go. Once it has been hidden for
 * [BACKGROUND_AFTER_MILLIS] it is sent TRIM_MEMORY_BACKGROUND and the activity manager
 * compacts its process fully, which pages its memory out to zram at once instead of waiting
 * for memory pressure. Each is sent once in a spell of being hidden; when the app is seen again
 * nothing is undone, since it reads back what it needs as it draws. The control daemon already
 * keeps less page cache for such a task and pages out processes with no window; this reaches
 * the app's own heap, which only the app can let go of.
 *
 * Each app that goes out of sight is logged once for that spell (`<package> is out of the user's
 * sight`), so a log shows the host's word arrived before any trim.
 *
 * An app playing audio (a player of it is started: its decoder and buffers are in use; a paused
 * player, which video apps and games keep for as long as they run, is not playing) or running a
 * foreground service is doing work the user asked for. It is sent TRIM_MEMORY_UI_HIDDEN all the
 * same, which only lets go of what it holds to draw, but neither TRIM_MEMORY_BACKGROUND nor the
 * compaction, which would page its decoder and buffers out and could break up the sound. An app
 * whose state cannot be looked up counts as busy. A busy app is looked at again every
 * [RECHECK_MILLIS] while it stays hidden, and once it is no longer busy it is sent the background
 * level and compacted at that look. The agent itself is never trimmed.
 *
 * Requests go one at a time through [AppMemoryPort.requestTrim]: the next is sent once the
 * last is carried out, or after [REQUEST_TIMEOUT_MILLIS]. When the activity manager does not
 * send a trim, its reason is logged as the trim script passed it on.
 *
 * Android refuses a background trim level to the process of a task it shows, which a hidden
 * app's task still is. DSA's framework sends it all the same to an app it was told the host
 * hides ([AppMemoryPort.setHiddenApps]), so the agent tells it whenever the hidden apps change.
 * A framework without that change is told nothing, and the trims are asked for as before; that
 * is logged once.
 *
 * Not thread safe: every method runs on the agent's scheduler thread.
 */
class HiddenApps(
    private val port: AppMemoryPort,
    private val scheduler: Scheduler,
    private val log: Log,
    private val clock: () -> Long,
    private val playing: () -> Set<String>,
    private val hasForegroundService: (String) -> Boolean,
    private val ownPackage: String,
) {
    /** How far a hidden app's trim has gone. */
    private class Hidden(val since: Long, var step: Int = 0, var keptFor: String? = null)

    private class Request(val process: String, val level: String, val compact: Boolean, val why: String)

    private var tasks: Map<Int, String> = emptyMap()
    private var seenTasks: Set<Int>? = null
    private var screenOn = true
    private val hidden = HashMap<String, Hidden>()
    private val queue = ArrayDeque<Request>()
    private var inFlight: Long? = null
    private var inFlightRequest: Request? = null
    private var sentAt = 0L

    /** Whether the framework sends a trim to an app the host hides; null until asked. */
    private var hiddenTrim: Boolean? = null

    /** The hidden apps the framework last took, or null when it has taken none since. */
    private var toldHidden: Set<String>? = null
    private var tellFailed = false
    private var lastNumber = 0L
    private var pollPending = false
    private var recheckPending = false

    /** Takes the app of each task the host is told of, by task. */
    fun tasks(byTask: Map<Int, String>) {
        tasks = byTask
        follow()
    }

    /** Takes the tasks whose window the user can see now. */
    fun shown(taskIds: Set<Int>) {
        seenTasks = taskIds
        answeredFor = tasks.keys
        follow()
    }

    /** The tasks there were when the host last said which can be seen. */
    private var answeredFor: Set<Int> = emptySet()

    /** Takes whether the host shows any task window. */
    fun screen(on: Boolean) {
        screenOn = on
        follow()
    }

    private fun follow() {
        val now = clock()
        val packages = tasks.values.filter { it.isNotEmpty() && it != ownPackage }.toSet()
        val seen = when {
            !screenOn -> emptySet()
            // A task newer than the host's last word is opening its window: seen until the host
            // says otherwise. Counted hidden, a reopened app was logged out of sight as its
            // window opened, and trimmed if the host's word came late.
            else -> seenTasks?.let { ids ->
                tasks.filterKeys { it in ids || it !in answeredFor }.values.toSet()
            } ?: packages
        }
        val nowHidden = packages - seen
        hidden.keys.retainAll(nowHidden)
        for (packageName in nowHidden) {
            if (packageName in hidden) continue
            hidden[packageName] = Hidden(now)
            val why = if (!screenOn) "no task window is shown" else "the host says none of its windows can be seen"
            log.info(
                "$packageName is out of the user's sight ($why); it is trimmed after ${HIDDEN_AFTER_MILLIS / 1000} s, " +
                    "and compacted after ${BACKGROUND_AFTER_MILLIS / 1000} s unless it plays audio or runs a foreground service",
            )
            scheduler.postDelayed(HIDDEN_AFTER_MILLIS) { check() }
            scheduler.postDelayed(BACKGROUND_AFTER_MILLIS) { check() }
        }
        tellHidden()
    }

    /** Asks the framework once whether it sends a trim to an app the host hides, and logs the answer. */
    private fun supportsHiddenTrim(): Boolean = hiddenTrim ?: run {
        val supported = try {
            port.hiddenTrimSupported()
        } catch (e: Exception) {
            false
        }
        if (supported) {
            log.info("the framework sends the apps the host hides their memory trims, although Android shows their tasks")
        } else {
            log.info(
                "this framework has no dsa-hidden-trim: the apps the host hides are sent their memory trims through " +
                    "send-trim-memory as before, which Android refuses while it shows their tasks",
            )
        }
        hiddenTrim = supported
        supported
    }

    /**
     * Tells the framework which apps the host hides, when that changed. One that did not take
     * them is told again with the next change of the host's word; that is logged once.
     */
    private fun tellHidden() {
        if (!supportsHiddenTrim()) return
        val now = hidden.keys.toSet()
        if (now == toldHidden) return
        val took = try {
            port.setHiddenApps(now)
        } catch (e: Exception) {
            false
        }
        if (took) {
            toldHidden = now
            tellFailed = false
            return
        }
        toldHidden = null
        if (!tellFailed) {
            log.warn("the activity manager did not take the apps the host hides, so it may refuse their memory trims")
        }
        tellFailed = true
    }

    /** Sends each hidden app the trims that are due. */
    fun check() {
        val now = clock()
        var recheck = false
        for ((packageName, state) in hidden.entries.sortedBy { it.key }) {
            val hiddenFor = now - state.since
            val due = when {
                state.step < 2 && hiddenFor >= BACKGROUND_AFTER_MILLIS -> 2
                state.step < 1 && hiddenFor >= HIDDEN_AFTER_MILLIS -> 1
                else -> continue
            }
            // A busy app still lets its drawing caches go, but its process is not compacted.
            val busy = if (due == 2) busy(packageName) else null
            if (busy != null) {
                if (state.step < 1) {
                    queue += Request(packageName, "HIDDEN", false, "hidden ${hiddenFor / 1000} s")
                    state.step = 1
                }
                if (state.keptFor != busy) {
                    log.info("kept hidden $packageName from TRIM_MEMORY_BACKGROUND and its compaction: $busy")
                }
                state.keptFor = busy
                recheck = true
                continue
            }
            state.keptFor = null
            val seconds = hiddenFor / 1000
            if (due == 2) {
                queue += Request(packageName, "BACKGROUND", true, "hidden $seconds s")
            } else {
                queue += Request(packageName, "HIDDEN", false, "hidden $seconds s")
            }
            state.step = due
        }
        if (recheck && !recheckPending) {
            recheckPending = true
            scheduler.postDelayed(RECHECK_MILLIS) {
                recheckPending = false
                check()
            }
        }
        sendNext()
    }

    /** Why [packageName] is not to be trimmed now, or null. */
    private fun busy(packageName: String): String? {
        val sounding = try {
            packageName in playing()
        } catch (e: Exception) {
            true
        }
        if (sounding) return "it plays audio"
        val working = try {
            hasForegroundService(packageName)
        } catch (e: Exception) {
            true
        }
        return if (working) "it runs a foreground service" else null
    }

    private fun sendNext() {
        val waiting = inFlight
        if (waiting != null) {
            // "<number> <result>"; a script from before the result answers the number alone.
            val answer = port.trimDone().trim()
            val done = answer.substringBefore(' ') == waiting.toString()
            if (!done && clock() - sentAt < REQUEST_TIMEOUT_MILLIS) {
                poll()
                return
            }
            inFlight = null
            val asked = inFlightRequest
            inFlightRequest = null
            if (done && asked != null) report(asked, answer.substringAfter(' ', ""))
        }
        val request = queue.removeFirstOrNull() ?: return
        // A request asked for before the app was seen again is dropped.
        if (request.process !in hidden) {
            sendNext()
            return
        }
        val number = clock().let { if (it <= lastNumber) lastNumber + 1 else it }
        lastNumber = number
        try {
            port.requestTrim("$number ${request.level} ${if (request.compact) 1 else 0} ${request.process}")
        } catch (e: Exception) {
            log.warn("could not ask for ${request.process} to be trimmed", e)
            sendNext()
            return
        }
        inFlight = number
        inFlightRequest = request
        sentAt = clock()
        val what = if (request.compact) {
            "${levelName(request.level)} and a full compaction"
        } else {
            levelName(request.level)
        }
        log.info("trimming ${request.process}, which the user cannot see (${request.why}): $what")
        poll()
    }

    /**
     * Logs what the trim script said came of [request] when the activity manager did not send
     * the trim: "refused <reason>", the activity manager's own reason, or "invalid". "sent", and
     * no result at all from an older script, are not logged: the script logs each trim it sends.
     */
    private fun report(request: Request, result: String) {
        val word = result.substringBefore(' ')
        val reason = result.substringAfter(' ', "")
        when (word) {
            "refused" -> log.warn(
                "the activity manager did not send ${request.process} ${levelName(request.level)}: " +
                    reason.ifEmpty { "it gave no reason" },
            )
            "invalid" -> log.warn("the trim script did not take the request for ${request.process}")
        }
    }

    private fun levelName(level: String) = if (level == "BACKGROUND") "TRIM_MEMORY_BACKGROUND" else "TRIM_MEMORY_UI_HIDDEN"

    /** Looks for the answer to the request in flight again shortly; once, however often asked. */
    private fun poll() {
        if (pollPending) return
        pollPending = true
        scheduler.postDelayed(POLL_MILLIS) {
            pollPending = false
            sendNext()
        }
    }

    companion object {
        /** How long an app stays hidden before TRIM_MEMORY_UI_HIDDEN: past a quick look elsewhere. */
        const val HIDDEN_AFTER_MILLIS = 10_000L

        /** How long an app stays hidden before TRIM_MEMORY_BACKGROUND and its compaction. */
        const val BACKGROUND_AFTER_MILLIS = 2 * 60_000L

        /** How often an app kept untrimmed because it was busy is looked at again. */
        const val RECHECK_MILLIS = 60_000L

        /** How often the answer to a trim request is looked for. */
        const val POLL_MILLIS = 500L

        /** How long a trim request may take before the next is sent anyway. */
        const val REQUEST_TIMEOUT_MILLIS = 15_000L
    }
}

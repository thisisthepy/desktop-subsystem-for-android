package io.github.thisisthepy.dsa.agent

/**
 * Says when one piece of a thread's work ran long. Everything else on that thread waited for
 * it: on the agent's thread that is the host's requests, a window's new size among them, so a
 * resize that is slow for no reason the trace shows can be matched against what held the
 * thread. At most one line every [quietMillis], with how many more there were meanwhile.
 */
class SlowWork(
    private val thread: String,
    private val log: Log,
    private val slowMillis: Long = SLOW_MILLIS,
    private val quietMillis: Long = QUIET_MILLIS,
) {
    private var lastLogged = Long.MIN_VALUE / 2
    private var unlogged = 0

    /** [work] (the work's class, which names where it was posted) ran from [startMillis] to [endMillis]. */
    fun ran(work: Any, startMillis: Long, endMillis: Long) {
        val took = endMillis - startMillis
        if (took < slowMillis) return
        if (endMillis - lastLogged < quietMillis) {
            unlogged++
            return
        }
        val more = if (unlogged > 0) "; $unlogged more since the last line" else ""
        log.warn("the $thread thread spent $took ms on one piece of work (${work.javaClass.name}); what was posted after it waited$more")
        lastLogged = endMillis
        unlogged = 0
    }

    private var stuckSince = Long.MIN_VALUE

    /**
     * A piece of work that began at [startMillis] has not ended at [nowMillis]. Said once for
     * that piece, with where the thread is ([stack], read only when it is said), because work
     * that never ends is never reported by [ran], and a thread that no longer answers the host
     * would otherwise leave nothing in the log.
     */
    fun stuck(work: Any, startMillis: Long, nowMillis: Long, stack: () -> List<String>) {
        if (nowMillis - startMillis < STUCK_MILLIS || stuckSince == startMillis) return
        stuckSince = startMillis
        val took = nowMillis - startMillis
        log.warn("the $thread thread has been in one piece of work for $took ms and is still in it (${work.javaClass.name}), at: ${stack().take(STACK_LINES).joinToString(" <- ")}")
    }

    companion object {
        const val STUCK_MILLIS = 3_000L
        const val STACK_LINES = 12
        const val SLOW_MILLIS = 250L
        const val QUIET_MILLIS = 10_000L
    }
}

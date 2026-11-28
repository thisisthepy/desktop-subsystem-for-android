package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.Scheduler

/**
 * Gives back what something keeps only to draw again soon, once it has not been used for
 * [idleMillis]: [release] runs once, [idleMillis] after the last [used], and not again until it
 * is used again. Each use only notes the time, and one check waits on [scheduler] at a time, so
 * a burst of uses (an animation) costs nothing more than a single one.
 *
 * Used for the buffers of a widget's window, which the agent's image reader keeps free between
 * the widget's rare frames, and for the GPU memory the agent's renderer keeps after a starting
 * window is gone. Both run on one thread, the one [scheduler] posts to, with [clock] its time in
 * milliseconds.
 */
class IdleRelease(
    private val scheduler: Scheduler,
    private val clock: () -> Long,
    private val idleMillis: Long,
    private val release: () -> Unit,
) {
    private var lastUsed = 0L
    private var checkPending = false
    private var released = true
    private var closed = false

    /** It was used now; the release waits [idleMillis] from now. */
    fun used() {
        if (closed) return
        lastUsed = clock()
        released = false
        if (!checkPending) checkAfter(idleMillis)
    }

    /** It is gone: nothing more is released. */
    fun close() {
        closed = true
    }

    private fun checkAfter(millis: Long) {
        checkPending = true
        scheduler.postDelayed(millis) { check() }
    }

    private fun check() {
        checkPending = false
        if (closed || released) return
        val idle = clock() - lastUsed
        if (idle < idleMillis) {
            // Used again since this check was posted: wait out the rest after that use.
            checkAfter(idleMillis - idle)
            return
        }
        released = true
        release()
    }
}

package io.github.thisisthepy.dsa.agent.input

import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent

/**
 * Pointer events waiting for the input thread, which takes them from any thread.
 *
 * Each injection is a binder call into the framework, and a mouse can report a thousand
 * moves a second. A press behind a queue of moves would wait for all of them, so a move that
 * has not been injected yet is replaced by a newer one of the same pointer. Nothing but moves
 * is ever replaced: a press, a release or a scroll always arrives, and in order.
 */
class PointerQueue(
    private val scheduler: Scheduler,
    private val nanos: () -> Long = System::nanoTime,
    /** Takes each event, with how long it waited in nanoseconds. */
    private val handle: (PointerEvent, Long) -> Unit,
) {
    private class Waiting(val event: PointerEvent, val arrived: Long)

    private val lock = Any()
    private val queue = ArrayList<Waiting>()
    private var draining = false

    /** Queues [event]. Called from any thread. */
    fun add(event: PointerEvent) {
        val waiting = Waiting(event, nanos())
        val wake = synchronized(lock) {
            if (!replaceMove(waiting)) queue.add(waiting)
            val idle = !draining
            draining = true
            idle
        }
        if (wake) scheduler.post(::drain)
    }

    /** Puts [waiting] in the place of a waiting move of the same pointer, if it may. */
    private fun replaceMove(waiting: Waiting): Boolean {
        val event = waiting.event
        if (!event.isMove()) return false
        for (index in queue.indices.reversed()) {
            val queued = queue[index].event
            // A move is not carried past anything else its pointer's gesture depends on.
            if (!queued.isMove()) return false
            if (queued.taskId == event.taskId && queued.pointerId == event.pointerId && queued.tool == event.tool) {
                if (queued.action != event.action || queued.buttons != event.buttons) return false
                queue[index] = waiting
                return true
            }
        }
        return false
    }

    private fun drain() {
        while (true) {
            val next = synchronized(lock) {
                if (queue.isEmpty()) {
                    draining = false
                    return
                }
                queue.removeAt(0)
            }
            handle(next.event, nanos() - next.arrived)
        }
    }

    private fun PointerEvent.isMove() =
        action == PointerAction.POINTER_ACTION_MOVE || action == PointerAction.POINTER_ACTION_HOVER_MOVE
}

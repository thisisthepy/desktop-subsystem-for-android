package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Locale

/**
 * The agent's part of the input trace, which says where a tap's time goes on its way to the
 * screen. It logs one line per press and release, in the shape the host's and the
 * hwcomposer's lines have, so the logs can be merged on their `utc` time:
 *
 * `input-trace stage=agent.inject task=7 action=POINTER_ACTION_DOWN utc=2026-10-26T06:23:03.123Z mono=1234.000 queued=0.2 inject=0.9`.
 *
 * `queued` is how long the event waited for the input thread, and `inject` how long the
 * framework took to take it, both in milliseconds. Off unless the boot asks for it.
 */
class InputTrace(
    private val log: Log,
    private val enabled: Boolean = false,
    private val monoMillis: () -> Double = { System.nanoTime() / 1_000_000.0 },
    private val wallMillis: () -> Long = System::currentTimeMillis,
) {
    fun injected(event: PointerEvent, queuedNanos: Long, injectNanos: Long) {
        if (!enabled) return
        when (event.action) {
            PointerAction.POINTER_ACTION_DOWN, PointerAction.POINTER_ACTION_UP, PointerAction.POINTER_ACTION_CANCEL -> Unit
            else -> return
        }
        val utc = UTC.format(Instant.ofEpochMilli(wallMillis()))
        log.info(
            "$TAG stage=agent.inject task=${event.taskId} action=${event.action} utc=$utc " +
                "mono=${fmt(monoMillis(), 3)} queued=${fmt(queuedNanos / NANOS_PER_MILLI, 1)} " +
                "inject=${fmt(injectNanos / NANOS_PER_MILLI, 1)}",
        )
    }

    private fun fmt(value: Double, places: Int) = String.format(Locale.ROOT, "%.${places}f", value)

    companion object {
        const val TAG = "input-trace"

        /** Set by `androidboot.hardware.dsa_input_trace=1` on the kernel command line. */
        const val PROPERTY = "ro.boot.hardware.dsa_input_trace"
        private const val NANOS_PER_MILLI = 1_000_000.0
        private val UTC: DateTimeFormatter =
            DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)
    }
}

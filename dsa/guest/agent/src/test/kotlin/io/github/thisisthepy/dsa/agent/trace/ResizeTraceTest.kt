package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.windowing.Size
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ResizeTraceTest {
    private var mono = 1000.0
    private val log = RecordingLog()

    private fun trace(detailed: Boolean = false) =
        ResizeTrace(log, detailed = { detailed }, monoMillis = { mono }, wallMillis = { 1_792_995_783_123L })

    @Test
    fun fr4_a_resize_is_summarised_once_its_bounds_read_back_at_the_new_size() {
        val trace = trace()
        trace.received(7, Size(800, 600), live = true)
        mono += 5.0
        trace.applied(4.0)
        mono += 20.0
        // The old bounds are not the end of the resize.
        trace.boundsRead(7, 640, 480)
        trace.boundsRead(7, 800, 600)
        assertEquals(
            listOf(
                "info: resize-trace task=7 size=800x600 stage=agent.summary " +
                    "utc=2026-10-26T06:23:03.123Z mono=1025.000 applied=5.0 bounds=25.0",
            ),
            log.lines,
        )
        // Later reads at that size are not another resize.
        trace.boundsRead(7, 800, 600)
        assertEquals(1, log.lines.size)
    }

    @Test
    fun fr4_the_summary_measures_from_the_first_request_for_a_size_and_says_how_long_it_queued() {
        val trace = trace()
        // The live size, which waited 300 ms for the agent's thread.
        trace.received(7, Size(800, 600), live = true, queuedMillis = 300.0)
        mono += 5.0
        // The host reconfigures the task at the size it gave it live.
        trace.received(7, Size(800, 600), live = false, queuedMillis = 0.5)
        mono += 10.0
        trace.boundsRead(7, 800, 600)
        assertEquals(
            listOf(
                "info: resize-trace task=7 size=800x600 stage=agent.summary " +
                    "utc=2026-10-26T06:23:03.123Z mono=1015.000 applied=none bounds=15.0 queued=300.0",
            ),
            log.lines,
        )
    }

    @Test
    fun fr4_stage_lines_come_only_when_asked_for_and_once_per_size() {
        trace(detailed = false).received(1, Size(10, 10), live = false)
        assertTrue(log.lines.isEmpty())

        val trace = trace(detailed = true)
        trace.received(1, Size(10, 10), live = true)
        trace.received(1, Size(10, 10), live = true)
        trace.applied(1.0)
        trace.applied(1.0)
        assertEquals(2, log.lines.size)
        assertTrue(log.lines[0], log.lines[0].contains("stage=agent.received"))
        assertTrue(log.lines[1], log.lines[1].endsWith("stage=agent.applied utc=2026-10-26T06:23:03.123Z mono=1000.000 took=1.0"))
    }

    @Test
    fun fr4_summaries_are_rate_limited_during_a_drag() {
        val trace = trace()
        for (step in 0 until 5) {
            val size = Size(500 + step, 400)
            trace.received(3, size, live = true)
            trace.applied(1.0)
            mono += 100.0
            trace.boundsRead(3, size.width, size.height)
        }
        // At 1100 ms the first; 1200 to 1500 within the interval, then 1600 is past it.
        trace.received(3, Size(600, 400), live = false)
        mono += 100.0
        trace.boundsRead(3, 600, 400)
        assertEquals(2, log.lines.size)
        assertTrue(log.lines[1], log.lines[1].endsWith("skipped=4"))
    }

    @Test
    fun fr4_a_size_kept_within_the_tasks_limits_lands_at_the_size_given_and_says_so() {
        val trace = trace()
        // Asked below the task's smallest width, which the agent gives instead.
        trace.received(7, Size(500, 600), live = true)
        trace.asked(7, 640, 600)
        trace.applied(2.0)
        mono += 30.0
        trace.boundsRead(7, 640, 600)
        assertEquals(2, log.lines.size)
        assertEquals(
            "info: task 7 takes 640x600, not the 500x600 the host asked for: " +
                "a task stays within its smallest size and the display",
            log.lines[0],
        )
        assertEquals(
            "info: resize-trace task=7 size=640x600 stage=agent.summary " +
                "utc=2026-10-26T06:23:03.123Z mono=1030.000 applied=0.0 bounds=30.0 asked=500x600",
            log.lines[1],
        )
        // The next step of the drag, given the same smallest width, is not said again.
        mono += 1000.0
        trace.received(7, Size(480, 600), live = true)
        trace.asked(7, 640, 600)
        assertEquals(2, log.lines.size)
        // A size the task takes as asked lands at that size.
        trace.received(7, Size(700, 600), live = true)
        trace.asked(7, 700, 600)
        trace.boundsRead(7, 700, 600)
        assertEquals(3, log.lines.size)
        assertTrue(log.lines[2], log.lines[2].contains("size=700x600 stage=agent.summary"))
        assertTrue(log.lines[2], !log.lines[2].contains("asked="))
    }
}

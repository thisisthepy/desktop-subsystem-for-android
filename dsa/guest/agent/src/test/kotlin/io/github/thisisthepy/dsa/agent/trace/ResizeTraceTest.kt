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
}

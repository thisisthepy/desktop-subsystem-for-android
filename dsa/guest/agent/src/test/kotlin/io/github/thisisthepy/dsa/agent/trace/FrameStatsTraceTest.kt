package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class FrameStatsTraceTest {
    private val header = "Flags,FrameTimelineVsyncId,IntendedVsync,Vsync,InputEventId,HandleInputStart,AnimationStart," +
        "PerformTraversalsStart,DrawStart,FrameDeadline,FrameStartTime,FrameInterval,WorkloadTarget,SyncQueued," +
        "SyncStart,IssueDrawCommandsStart,SwapBuffers,FrameCompleted,DequeueBufferDuration,QueueBufferDuration," +
        "GpuCompleted,SwapBuffersCompleted,DisplayPresentTime,CommandSubmissionCompleted,"

    /** A frame meant for [vsyncMs], each stage taking the milliseconds the test checks for. */
    private fun row(vsyncMs: Long, flags: Int = 0): String {
        val v = vsyncMs * 1_000_000
        fun at(ms: Double) = v + (ms * 1_000_000).toLong()
        return listOf(
            flags.toLong(), 1, v, at(0.5), 0, at(2.0), at(2.5), at(3.0), at(7.0), at(16.0), at(0.5), 16_666_666, 0,
            at(8.0), at(8.5), at(9.0), at(12.0), at(13.0), 200_000, 100_000, at(11.0), at(13.0), 0, at(12.5),
        ).joinToString(",") + ","
    }

    private fun dump(vararg windows: List<String>) = buildString {
        appendLine("** Graphics info for pid 1234 [com.android.settings] **")
        for (rows in windows) {
            appendLine("---PROFILEDATA---")
            appendLine(header)
            rows.forEach(::appendLine)
            appendLine("---PROFILEDATA---")
            appendLine("View hierarchy:")
        }
    }

    private val scheduler = FakeScheduler()
    private val log = RecordingLog()
    private val dumps = mutableListOf<String>()
    private var answer = dump(listOf(row(1000)))

    private fun trace(enabled: Boolean = true) = FrameStatsTrace(
        log, enabled, scheduler,
        dump = { pkg -> dumps += pkg; answer },
        monoMillis = { 2000.0 },
        wallMillis = { 2000L },
    )

    @Test
    fun fr5_each_frame_after_a_press_is_logged_with_its_main_and_render_thread_stages() {
        trace().released("com.android.settings")
        scheduler.advance(999)
        assertTrue(dumps.isEmpty())
        scheduler.advance(1)

        assertEquals(listOf("com.android.settings"), dumps)
        assertEquals(
            listOf(
                "info: input-trace stage=app.frame pkg=com.android.settings window=0 utc=1970-01-01T00:00:01.000Z " +
                    "mono=1000.000 late=2.0 input=0.5 animation=0.5 traversal=4.0 record=1.0 main=6.0 " +
                    "sync_wait=0.5 sync=0.5 issue=3.0 swap=1.0 render=4.5 dequeue=0.2 queue=0.1 gpu=2.0 " +
                    "total=13.0 flags=0",
            ),
            log.lines,
        )
    }

    @Test
    fun fr5_a_frame_is_logged_once_however_many_presses_read_it() {
        val trace = trace()
        trace.released("com.android.settings")
        trace.released("com.android.settings")
        scheduler.advance(1000)
        assertEquals(1, dumps.size)

        answer = dump(listOf(row(1000), row(1016, flags = 4)))
        trace.released("com.android.settings")
        scheduler.advance(1000)

        assertEquals(2, log.lines.size)
        assertTrue(log.lines[1], log.lines[1].contains("mono=1016.000"))
        assertTrue(log.lines[1], log.lines[1].endsWith("flags=4"))
    }

    @Test
    fun fr5_every_window_of_the_app_is_read() {
        answer = dump(listOf(row(1000)), listOf(row(1000), row(1016)))
        trace().released("com.android.settings")
        scheduler.advance(1000)

        assertEquals(listOf("window=0", "window=1", "window=1"), log.lines.map { Regex("window=\\d").find(it)!!.value })
    }

    @Test
    fun fr5_a_stage_the_app_did_not_record_is_a_dash() {
        // GpuCompleted (column 20) is 0 when the driver does not report GPU completion.
        val withoutGpu = row(1000).split(',').toMutableList().also { it[20] = "0" }.joinToString(",")
        answer = dump(listOf(withoutGpu))
        trace().released("com.android.settings")
        scheduler.advance(1000)

        assertTrue(log.lines.single(), log.lines.single().contains(" gpu=- "))
    }

    @Test
    fun fr5_nothing_is_read_for_an_unknown_task_or_with_the_trace_off() {
        trace().released(null)
        trace(enabled = false).released("com.android.settings")
        scheduler.advance(2000)

        assertTrue(dumps.isEmpty())
        assertTrue(log.lines.isEmpty())
    }

    @Test
    fun fr5_a_failed_read_is_reported_and_the_next_press_reads_again() {
        val trace = FrameStatsTrace(log, true, scheduler, dump = { error("no gfxinfo") })
        trace.released("com.android.settings")
        scheduler.advance(1000)
        trace.released("com.android.settings")
        scheduler.advance(1000)

        assertEquals(2, log.lines.count { it.startsWith("warn: could not read the frame timings") })
    }
}

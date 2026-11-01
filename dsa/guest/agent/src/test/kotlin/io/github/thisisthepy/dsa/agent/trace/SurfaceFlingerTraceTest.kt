package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SurfaceFlingerTraceTest {
    private val pending = Long.MAX_VALUE
    private val layers = """
        Display 0 HWC layers
        com.android.settings/com.android.settings.Settings#120
        Splash Screen com.android.settings#118
        com.android.launcher3/com.android.launcher3.Launcher#80
        com.android.settings/com.android.settings.Settings#120
    """.trimIndent()

    private fun latency(vararg rows: Triple<Long, Long, Long>) = buildString {
        appendLine("16666666")
        for ((desired, actual, ready) in rows) appendLine("$desired\t$actual\t$ready")
        repeat(3) { appendLine("0\t0\t0") }
    }

    @Test
    fun fr5_only_the_apps_own_layers_are_read_each_once() {
        assertEquals(
            listOf(
                "com.android.settings/com.android.settings.Settings#120",
                "Splash Screen com.android.settings#118",
            ),
            SurfaceFlingerTrace.layersOf(layers, "com.android.settings"),
        )
    }

    @Test
    fun fr5_the_latency_dump_gives_each_frame_and_leaves_out_empty_slots() {
        val frames = SurfaceFlingerTrace.parseLatency(
            latency(Triple(1_000_000_000, 1_016_000_000, 1_004_000_000), Triple(1_016_666_666, pending, 1_020_000_000)),
        )
        assertEquals(
            listOf(
                SurfaceFlingerTrace.Frame(desired = 1_000_000_000, actual = 1_016_000_000, ready = 1_004_000_000),
                SurfaceFlingerTrace.Frame(desired = 1_016_666_666, actual = pending, ready = 1_020_000_000),
            ),
            frames,
        )
    }

    @Test
    fun fr5_a_frame_still_on_its_way_waits_and_those_after_it_wait_with_it() {
        val frames = listOf(
            SurfaceFlingerTrace.Frame(3, 4, 3),
            SurfaceFlingerTrace.Frame(1, 2, 1),
            SurfaceFlingerTrace.Frame(2, pending, 2),
        )
        assertEquals(listOf(SurfaceFlingerTrace.Frame(1, 2, 1)), SurfaceFlingerTrace.newFrames(frames, 0))
        assertTrue(SurfaceFlingerTrace.newFrames(frames, 1).isEmpty())
    }

    @Test
    fun fr5_each_new_frame_of_an_apps_layer_is_logged_once_after_a_press() {
        val log = RecordingLog()
        val scheduler = FakeScheduler()
        val read = mutableListOf<String>()
        var answer = latency(Triple(1_000_000_000, 1_016_000_000, 1_004_000_000))
        val surfaceFlinger = SurfaceFlingerTrace(
            log,
            list = { "com.android.settings/Settings#1\n" },
            latency = { layer -> read += layer; answer },
            monoMillis = { 2000.0 },
            wallMillis = { 2000L },
        )
        val trace = FrameStatsTrace(
            log, true, scheduler,
            dump = { null },
            monoMillis = { 2000.0 },
            wallMillis = { 2000L },
            surfaceFlinger = surfaceFlinger,
        )
        trace.released("com.android.settings")
        scheduler.advance(FrameStatsTrace.DELAY_MILLIS)
        assertEquals(listOf("com.android.settings/Settings#1"), read)
        val line = "info: input-trace stage=sf.frame pkg=com.android.settings layer=com.android.settings/Settings#1 " +
            "utc=1970-01-01T00:00:01.000Z mono=1000.000 desired=1000.000 ready=1004.000 actual=1016.000"
        assertEquals(listOf(line), log.lines)

        answer = latency(
            Triple(1_000_000_000, 1_016_000_000, 1_004_000_000),
            Triple(1_016_666_666, 1_033_000_000, 1_020_000_000),
        )
        trace.released("com.android.settings")
        scheduler.advance(FrameStatsTrace.DELAY_MILLIS)
        assertEquals(2, log.lines.size)
        assertTrue(log.lines[1], log.lines[1].contains("desired=1016.667 ready=1020.000 actual=1033.000"))
    }

    @Test
    fun fr5_whitespace_in_a_layer_name_does_not_split_the_line() {
        val log = RecordingLog()
        SurfaceFlingerTrace(
            log,
            list = { "Splash Screen com.android.settings#118" },
            latency = { latency(Triple(1_000_000, 2_000_000, 1_500_000)) },
        ).read("com.android.settings")
        assertTrue(log.lines.single(), log.lines.single().contains(" layer=Splash_Screen_com.android.settings#118 "))
    }
}

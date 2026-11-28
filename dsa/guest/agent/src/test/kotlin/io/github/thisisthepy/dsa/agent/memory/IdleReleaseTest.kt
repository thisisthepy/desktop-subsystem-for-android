package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.FakeScheduler
import org.junit.Assert.assertEquals
import org.junit.Test

class IdleReleaseTest {
    private val scheduler = FakeScheduler()
    private var releases = 0
    private val idle = IdleRelease(scheduler, { scheduler.now }, 5_000) { releases++ }

    @Test
    fun nfr1_an_idle_widget_window_gives_back_its_buffers_once_after_its_last_frame() {
        idle.used()
        scheduler.advance(4_999)
        assertEquals(0, releases)
        scheduler.advance(1)
        assertEquals(1, releases)
        // Nothing more while it stays idle.
        scheduler.advance(60_000)
        assertEquals(1, releases)
    }

    @Test
    fun nfr1_each_frame_postpones_the_release_to_after_the_last_one() {
        idle.used()
        scheduler.advance(3_000)
        idle.used()
        scheduler.advance(3_000)
        idle.used()
        scheduler.advance(4_999)
        assertEquals(0, releases)
        scheduler.advance(1)
        assertEquals(1, releases)
    }

    @Test
    fun nfr1_a_burst_of_frames_waits_on_a_single_check() {
        // An animation: a frame every 100 ms for two seconds.
        repeat(20) {
            idle.used()
            scheduler.advance(100)
        }
        scheduler.advance(4_900)
        assertEquals(1, releases)
    }

    @Test
    fun nfr1_a_window_drawn_again_after_its_release_is_released_again_once_idle() {
        idle.used()
        scheduler.advance(5_000)
        idle.used()
        scheduler.advance(5_000)
        assertEquals(2, releases)
    }

    @Test
    fun nfr1_nothing_is_released_after_the_window_closes() {
        idle.used()
        scheduler.advance(1_000)
        idle.close()
        scheduler.advance(10_000)
        idle.used()
        scheduler.advance(10_000)
        assertEquals(0, releases)
    }

    @Test
    fun nfr1_the_agents_renderer_is_trimmed_once_a_second_after_the_last_starting_window_goes() {
        var trims = 0
        val renderer = IdleRelease(scheduler, { scheduler.now }, 1_000) { trims++ }
        // Two apps opened together: their starting windows go half a second apart.
        renderer.used()
        scheduler.advance(500)
        renderer.used()
        scheduler.advance(999)
        assertEquals(0, trims)
        scheduler.advance(1)
        assertEquals(1, trims)
    }

    @Test
    fun nfr1_nothing_is_released_before_the_first_use() {
        scheduler.advance(60_000)
        assertEquals(0, releases)
    }
}

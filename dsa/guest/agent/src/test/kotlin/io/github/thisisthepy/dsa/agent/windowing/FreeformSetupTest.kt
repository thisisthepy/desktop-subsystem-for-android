package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FreeformSetupTest {
    private val scheduler = FakeScheduler()

    @Test
    fun fr3_the_display_becomes_a_freeform_display() {
        val windowing = FakeWindowing()
        val setup = FreeformSetup(windowing, scheduler, RecordingLog())

        setup.start()
        scheduler.advance(500)

        assertTrue(windowing.freeformEnabled)
        assertEquals(WINDOWING_MODE_FREEFORM, windowing.mode)
        assertTrue(setup.freeform)
    }

    @Test
    fun fr3_the_display_mode_is_set_again_until_the_window_manager_takes_it() {
        val windowing = FakeWindowing(acceptAfter = 3)
        val setup = FreeformSetup(windowing, scheduler, RecordingLog(), retryMillis = 500)

        setup.start()
        scheduler.advance(10_000)

        assertEquals(4, windowing.sets)
        assertTrue(setup.freeform)
    }

    @Test
    fun fr3_the_setup_gives_up_after_its_attempts_and_says_so() {
        val windowing = FakeWindowing(acceptAfter = Int.MAX_VALUE)
        val log = RecordingLog()
        val setup = FreeformSetup(windowing, scheduler, log, retryMillis = 500, attempts = 5)

        setup.start()
        scheduler.advance(60_000)

        assertEquals(5, windowing.sets)
        assertFalse(setup.freeform)
        assertTrue(log.lines.last().contains("still not a freeform display"))
    }
}

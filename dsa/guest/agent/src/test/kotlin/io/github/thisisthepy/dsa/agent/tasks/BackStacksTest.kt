package io.github.thisisthepy.dsa.agent.tasks

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BackStacksTest {
    private val stacks = BackStacks()

    private fun read(activities: Int, top: String, now: Long, shows: Boolean = true) =
        stacks.update(7, TaskStack(activities, top, topShows = shows), now)

    @Test
    fun fr22_a_splash_left_under_the_main_screen_gives_the_icon() {
        // The splash shows for a moment and starts the main screen, staying under it.
        assertFalse(read(1, "app/.Splash", now = 0))
        assertFalse("the main screen shows the icon", read(2, "app/.Main", now = 1_500))
        // Time on the main screen does not make the splash a screen to go back to.
        assertFalse(read(2, "app/.Main", now = 60_000))
        // A real second screen opened from the main one gives the arrow, and Back from it
        // returns to the main screen, which shows the icon again.
        stacks.interacted(7)
        assertTrue(read(3, "app/.Detail", now = 61_000))
        assertFalse(read(2, "app/.Main", now = 62_000))
    }

    @Test
    fun fr22_a_real_second_screen_gives_the_arrow() {
        assertFalse(read(1, "app/.Main", now = 0))
        // The user taps something on the main screen, which opens the second one.
        stacks.interacted(7)
        assertTrue("past the main screen", read(2, "app/.Detail", now = 800))
        // Back on the main screen: the icon again.
        assertFalse(read(1, "app/.Main", now = 2_000))
        // A screen the user stayed on long enough counts without a tap, as when the app opens
        // the next screen itself.
        val untouched = BackStacks()
        assertFalse(untouched.update(8, TaskStack(1, "app/.Main"), 0))
        assertTrue(untouched.update(8, TaskStack(2, "app/.Detail"), BackStacks.SEEN_AFTER_MS))
    }

    @Test
    fun fr22_a_trampoline_or_a_finished_splash_is_no_place_to_go_back_to() {
        // A Theme.NoDisplay activity that only starts the main one, even touched and old.
        assertFalse(read(1, "app/.Trampoline", now = 0, shows = false))
        stacks.interacted(7)
        assertFalse(read(2, "app/.Main", now = 10_000))
        // A finishing splash is gone from the count, and the main screen is the first one.
        val finishing = BackStacks()
        assertFalse(finishing.update(9, TaskStack(1, "app/.Splash"), 0))
        assertFalse(finishing.update(9, TaskStack(1, "app/.Main"), 1_000))
    }

    @Test
    fun fr22_a_task_read_late_trusts_what_is_below_its_top() {
        // The agent started after the task: nothing says the screens below were not seen.
        assertTrue(stacks.update(5, TaskStack(2, "app/.Detail"), 0))
        assertFalse(stacks.update(5, TaskStack(1, "app/.Main"), 100))
        stacks.retain(emptyList())
        // Forgotten once gone: a new task of the same id starts afresh.
        assertFalse(stacks.update(5, TaskStack(1, "app/.Main"), 200))
    }
}

package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ForcedDensityTest {
    private val actions = FakeActions()
    private val log = RecordingLog()
    private val forced = ForcedDensity(actions, log)

    @Test
    fun fr4_a_forced_density_marks_the_task_before_it_is_given() {
        actions.forcedDensity = true

        assertTrue(forced.beforeDensity(7, forced = true))

        assertEquals(listOf(7 to true), actions.forcedMarks)
    }

    @Test
    fun fr4_a_marked_task_is_marked_once() {
        actions.forcedDensity = true
        forced.beforeDensity(7, forced = true)

        assertTrue(forced.beforeDensity(7, forced = true))

        assertEquals(listOf(7 to true), actions.forcedMarks)
    }

    @Test
    fun fr4_a_forced_density_is_not_given_where_the_framework_cannot_mark_the_task() {
        // An image without the framework change: given the density, the app would restart.
        actions.forcedDensity = false

        assertFalse(forced.beforeDensity(7, forced = true))
        assertTrue(actions.forcedMarks.isEmpty())
        assertFalse(forced.available)
    }

    @Test
    fun fr4_a_forced_density_is_not_given_when_the_framework_refuses_the_mark() {
        actions.forcedDensity = true
        actions.marksTaken = false

        assertFalse(forced.beforeDensity(7, forced = true))

        // Asked again next time, as nothing was marked.
        actions.marksTaken = true
        assertTrue(forced.beforeDensity(7, forced = true))
        assertEquals(listOf(7 to true, 7 to true), actions.forcedMarks)
    }

    @Test
    fun fr4_a_density_without_forced_takes_the_mark_away_and_is_given() {
        actions.forcedDensity = true
        forced.beforeDensity(7, forced = true)

        assertTrue(forced.beforeDensity(7, forced = false))

        assertEquals(listOf(7 to true, 7 to false), actions.forcedMarks)
    }

    @Test
    fun fr4_an_ordinary_density_never_touches_a_mark() {
        // App, System and Auto: the agent gives the density as it always did.
        assertTrue(forced.beforeDensity(7, forced = false))

        assertTrue(actions.forcedMarks.isEmpty())
        assertEquals(0, actions.forcedDensityAsked)
    }

    @Test
    fun fr4_the_framework_is_asked_once_whether_it_can_mark_a_task() {
        actions.forcedDensity = true

        assertTrue(forced.available)
        assertTrue(forced.available)
        forced.beforeDensity(7, forced = true)

        assertEquals(1, actions.forcedDensityAsked)
    }

    @Test
    fun fr4_an_ended_task_is_forgotten() {
        actions.forcedDensity = true
        forced.beforeDensity(7, forced = true)

        forced.running(emptySet())
        // The mark ended with the task, so an unforced density asks the framework for nothing.
        assertTrue(forced.beforeDensity(7, forced = false))

        assertEquals(listOf(7 to true), actions.forcedMarks)
    }
}

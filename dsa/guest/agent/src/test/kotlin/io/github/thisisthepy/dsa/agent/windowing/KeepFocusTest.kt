package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class KeepFocusTest {
    private val actions = FakeActions()
    private val log = RecordingLog()
    private val keep = KeepFocus(actions, log)

    private val game = "com.ustwo.monumentvalley"

    @Test
    fun fr44_the_framework_hears_the_apps_that_keep_their_focus() {
        actions.keepFocus = true

        keep.hostSays(setOf(game))

        assertEquals(listOf(setOf(game)), actions.keepFocusLists)
    }

    @Test
    fun fr44_the_first_list_is_given_even_when_every_app_is_off() {
        // The window manager may hold the list of an agent that ran before this one.
        actions.keepFocus = true

        keep.hostSays(emptySet())

        assertEquals(listOf(emptySet<String>()), actions.keepFocusLists)
    }

    @Test
    fun fr44_the_same_list_is_given_once_and_a_change_again() {
        actions.keepFocus = true
        keep.hostSays(setOf(game))
        keep.hostSays(setOf(game))
        keep.hostSays(emptySet())

        assertEquals(listOf(setOf(game), emptySet()), actions.keepFocusLists)
        assertEquals(1, actions.keepFocusAsked)
    }

    @Test
    fun fr44_an_image_without_the_framework_change_is_not_asked() {
        actions.keepFocus = false

        keep.hostSays(setOf(game))
        keep.hostSays(setOf(game, "com.example.notes"))

        assertTrue(actions.keepFocusLists.isEmpty())
        assertFalse(keep.available)
        assertEquals(1, actions.keepFocusAsked)
    }

    @Test
    fun fr44_a_list_the_framework_refused_is_given_again() {
        actions.keepFocus = true
        actions.keepFocusTaken = false
        keep.hostSays(setOf(game))

        actions.keepFocusTaken = true
        keep.hostSays(setOf(game))

        assertEquals(listOf(setOf(game), setOf(game)), actions.keepFocusLists)
    }
}

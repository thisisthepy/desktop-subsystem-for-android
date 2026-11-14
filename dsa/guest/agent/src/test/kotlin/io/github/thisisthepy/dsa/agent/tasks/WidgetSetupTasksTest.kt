package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A widget's setup runs in a freeform task whose root is the agent's own activity. The host
 * must get a window for it, as the app's, focused, so the user sees the setup and can finish
 * or cancel it.
 */
class WidgetSetupTasksTest {
    private val setupActivity = "$AGENT.platform.WidgetConfigureActivity"
    private val setups = WidgetSetupTasks(AGENT, setupActivity)
    private val flo = "skplanet.musicmate"

    @Test
    fun fr12_a_widget_setup_runs_in_a_visible_task_window_of_the_app() {
        // The agent's activity alone, before the setup starts: no window.
        assertEquals(AGENT, setups.owner(9, AGENT, setupActivity, AGENT, 1))
        assertFalse(setups.isSetup(9))
        // Android's proxy that starts the setup: still no window.
        assertEquals(AGENT, setups.owner(9, AGENT, setupActivity, "android", 2))
        // The app's setup on top: the task is the app's.
        assertEquals(flo, setups.owner(9, AGENT, setupActivity, flo, 3))
        assertTrue(setups.isSetup(9))

        // The host gets one window for it, under the app's name and focused.
        val sent = mutableListOf<Envelope>()
        val reporter = TaskReporter(AGENT) { frame -> sent += decode(frame); true }
        val task = appTask(9, packageName = setups.owner(9, AGENT, setupActivity, flo, 3)!!, label = "FLO", focused = true)
        reporter.update(listOf(task))
        val update = sent.single { it.bodyCase == Envelope.BodyCase.TASK_UPDATED }.taskUpdated
        assertEquals(flo, update.packageName)
        assertEquals("FLO", update.label)
        assertTrue(update.focused)

        // An Android screen over the setup keeps the window the app's.
        assertEquals(flo, setups.owner(9, AGENT, setupActivity, "android", 4))
        // The setup ended and only the agent's activity is left: the window goes.
        assertEquals(AGENT, setups.owner(9, AGENT, setupActivity, AGENT, 1))
        assertFalse(setups.isSetup(9))
        reporter.update(listOf(appTask(9, packageName = AGENT)))
        assertEquals(Envelope.BodyCase.TASK_REMOVED, sent.last().bodyCase)
    }

    @Test
    fun fr12_only_the_agents_setup_activity_makes_a_task_the_setups_apps() {
        // An app's own task is its root's, whatever is on top.
        assertEquals("org.example.mail", setups.owner(3, "org.example.mail", "org.example.mail.Main", flo, 2))
        assertFalse(setups.isSetup(3))
        // The agent's other activities stay the agent's.
        assertEquals(AGENT, setups.owner(4, AGENT, "$AGENT.platform.HomeActivity", flo, 2))
        assertFalse(setups.isSetup(4))
        assertEquals(null, setups.owner(5, null, null, null, 0))
    }

    @Test
    fun fr12_ended_setup_tasks_are_forgotten() {
        setups.owner(9, AGENT, setupActivity, flo, 3)
        setups.retain(listOf(1, 2))
        assertFalse(setups.isSetup(9))
        // A new task that reuses the ID starts from nothing.
        assertEquals(AGENT, setups.owner(9, AGENT, setupActivity, "android", 2))
    }
}

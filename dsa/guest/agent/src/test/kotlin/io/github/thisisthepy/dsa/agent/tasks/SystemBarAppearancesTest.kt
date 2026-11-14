package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Test
import io.github.thisisthepy.dsa.control.v1.StatusBarAppearance as Wire

class SystemBarAppearancesTest {
    private val appearances = SystemBarAppearances()

    private val store = appTask(1, packageName = "com.android.vending", focused = true)
    private val mail = appTask(2, packageName = "org.example.mail")

    private fun light(packageName: String? = store.packageName) =
        BarsReport(SystemBarAppearances.APPEARANCE_LIGHT_STATUS_BARS, packageName = packageName)

    private fun dark(packageName: String? = store.packageName) = BarsReport(0, packageName = packageName)

    private fun appearanceOf(taskId: Int, tasks: List<TaskSnapshot> = listOf(store, mail)): StatusBarAppearance =
        appearances.apply(tasks).single { it.taskId == taskId }.statusBar

    @Test
    fun fr22_an_app_that_asks_for_a_light_status_bar_is_reported_light() {
        appearances.report(light())
        assertEquals(StatusBarAppearance.LIGHT, appearanceOf(1))
        // The caption bar's own flag, which an app on a desktop window may set apart, counts
        // as well.
        appearances.report(BarsReport(SystemBarAppearances.APPEARANCE_LIGHT_CAPTION_BARS, packageName = store.packageName))
        assertEquals(StatusBarAppearance.LIGHT, appearanceOf(1))
        // The other task has said nothing.
        assertEquals(StatusBarAppearance.UNKNOWN, appearanceOf(2))
    }

    @Test
    fun fr22_an_app_that_clears_light_after_asking_for_it_is_reported_dark() {
        appearances.report(light())
        appearances.report(dark())
        assertEquals(StatusBarAppearance.DARK, appearanceOf(1))
    }

    @Test
    fun fr22_an_app_that_never_asked_for_light_has_expressed_nothing() {
        // An app that never calls setSystemBarsAppearance has the flag clear too, so the host
        // reads the tone from its pixels.
        appearances.report(dark())
        assertEquals(StatusBarAppearance.UNKNOWN, appearanceOf(1))
        assertEquals(StatusBarAppearance.UNKNOWN, appearanceOf(2))
    }

    @Test
    fun fr22_a_tab_switch_changes_the_appearance_without_waiting_for_a_task_description() {
        val sent = mutableListOf<Envelope>()
        val reporter = TaskReporter(AGENT) { frame ->
            sent += decode(frame)
            true
        }
        // The framework's tasks do not change between the reads: only the reports do.
        val tasks = listOf(store)
        appearances.report(light())
        reporter.update(appearances.apply(tasks))
        assertEquals(Wire.STATUS_BAR_APPEARANCE_LIGHT, sent.single().taskUpdated.statusBarAppearance)

        for ((report, wire) in listOf(
            dark() to Wire.STATUS_BAR_APPEARANCE_DARK,
            light() to Wire.STATUS_BAR_APPEARANCE_LIGHT,
            dark() to Wire.STATUS_BAR_APPEARANCE_DARK,
        )) {
            sent.clear()
            appearances.report(report)
            reporter.update(appearances.apply(tasks))
            assertEquals(wire, sent.single().taskUpdated.statusBarAppearance)
        }

        // A read with no report in between sends nothing.
        sent.clear()
        reporter.update(appearances.apply(tasks))
        assertEquals(emptyList<Envelope>(), sent)
    }

    @Test
    fun fr22_a_report_belongs_to_the_task_its_package_runs_in() {
        // The report is for the mail app's window, though the store's task was focused when
        // the tasks were last read.
        appearances.report(light(mail.packageName))
        assertEquals(StatusBarAppearance.LIGHT, appearanceOf(2))
        assertEquals(StatusBarAppearance.UNKNOWN, appearanceOf(1))
        // Its top activity's package counts too: a screen of another app in the task.
        val shared = mail.copy(stack = TaskStack(activities = 2, top = "com.android.intentresolver/.ChooserActivity"))
        appearances.report(dark("com.android.intentresolver"))
        assertEquals(StatusBarAppearance.DARK, appearanceOf(2, listOf(store, shared)))
    }

    @Test
    fun fr22_a_report_of_a_window_that_is_no_apps_changes_nothing() {
        appearances.report(light())
        appearances.report(dark("android"))
        assertEquals(StatusBarAppearance.LIGHT, appearanceOf(1))
        // A report that names no package belongs to the focused task.
        appearances.report(dark(null))
        assertEquals(StatusBarAppearance.DARK, appearanceOf(1))
    }

    @Test
    fun fr22_of_two_tasks_of_one_app_the_focused_one_takes_the_report() {
        val first = appTask(3, packageName = "org.example.notes")
        val second = appTask(4, packageName = "org.example.notes", focused = true)
        val tasks = listOf(first, second)
        appearances.report(light("org.example.notes"))
        val read = appearances.apply(tasks)
        assertEquals(StatusBarAppearance.UNKNOWN, read.single { it.taskId == 3 }.statusBar)
        assertEquals(StatusBarAppearance.LIGHT, read.single { it.taskId == 4 }.statusBar)
        // With neither focused, the report cannot be placed and changes nothing.
        appearances.report(dark("org.example.notes"))
        val unfocused = appearances.apply(tasks.map { it.copy(focused = false) })
        assertEquals(StatusBarAppearance.LIGHT, unfocused.single { it.taskId == 4 }.statusBar)
    }

    @Test
    fun fr22_a_region_that_covers_the_top_of_a_task_decides_for_it() {
        // The window manager says the store's part of the status bar is light, while the
        // window that controls the bars asks for dark.
        val region = AppearanceArea(SystemBarAppearances.APPEARANCE_LIGHT_STATUS_BARS, store.bounds)
        appearances.report(light())
        appearances.report(BarsReport(0, listOf(region), store.packageName))
        assertEquals(StatusBarAppearance.LIGHT, appearanceOf(1))
        // A region elsewhere leaves the controlling window's appearance to decide.
        val elsewhere = AppearanceArea(SystemBarAppearances.APPEARANCE_LIGHT_STATUS_BARS, TaskBounds(2000, 0, 2100, 40))
        appearances.report(BarsReport(0, listOf(elsewhere), store.packageName))
        assertEquals(StatusBarAppearance.DARK, appearanceOf(1))
    }

    @Test
    fun fr22_a_task_that_ends_is_forgotten() {
        appearances.report(light())
        assertEquals(StatusBarAppearance.LIGHT, appearanceOf(1))
        appearances.apply(listOf(mail))
        // The same task ID again is a new task that has said nothing.
        assertEquals(StatusBarAppearance.UNKNOWN, appearanceOf(1))
    }
}

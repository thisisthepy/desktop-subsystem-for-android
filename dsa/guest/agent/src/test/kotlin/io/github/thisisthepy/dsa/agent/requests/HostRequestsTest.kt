package io.github.thisisthepy.dsa.agent.requests

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.tasks.Messages
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.AppSettingsPage
import io.github.thisisthepy.dsa.control.v1.CloseTask
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.LaunchApp
import io.github.thisisthepy.dsa.control.v1.OpenAppSettings
import io.github.thisisthepy.dsa.control.v1.ResizeTask
import io.github.thisisthepy.dsa.control.v1.SetTaskCaption
import io.github.thisisthepy.dsa.control.v1.SetTaskDensity
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class HostRequestsTest {
    private val actions = FakeActions()
    private val reported = mutableSetOf<Int>()
    private val requests = HostRequests(AGENT, actions) { it in reported }

    private fun launch(packageName: String) =
        Envelope.newBuilder().setLaunchApp(LaunchApp.newBuilder().setPackageName(packageName)).build().toByteArray()

    private fun close(taskId: Int) =
        Envelope.newBuilder().setCloseTask(CloseTask.newBuilder().setTaskId(taskId)).build().toByteArray()

    private fun resize(taskId: Int, width: Int, height: Int, live: Boolean = false) = Envelope.newBuilder()
        .setResizeTask(ResizeTask.newBuilder().setTaskId(taskId).setWidth(width).setHeight(height).setLive(live))
        .build().toByteArray()

    private fun density(taskId: Int, densityDpi: Int) = Envelope.newBuilder()
        .setSetTaskDensity(SetTaskDensity.newBuilder().setTaskId(taskId).setDensityDpi(densityDpi))
        .build().toByteArray()

    private fun launchShortcut(packageName: String, shortcutId: String) = Envelope.newBuilder()
        .setLaunchShortcut(
            io.github.thisisthepy.dsa.control.v1.LaunchShortcut.newBuilder().setPackageName(packageName).setShortcutId(shortcutId),
        )
        .build().toByteArray()

    @Test
    fun fr7_launch_shortcut_starts_the_apps_shortcut() {
        actions.startableShortcuts = setOf("org.example.chat" to "new-chat")

        assertEquals(
            RequestResult.ShortcutLaunched("org.example.chat", "new-chat"),
            requests.handle(launchShortcut("org.example.chat", "new-chat")),
        )
        assertEquals(listOf("org.example.chat" to "new-chat"), actions.shortcutsLaunched)
        assertTrue(actions.launched.isEmpty())
    }

    @Test
    fun fr7_a_shortcut_that_cannot_start_opens_its_app_instead() {
        actions.launchable = setOf("org.example.chat")

        assertEquals(
            RequestResult.ShortcutLaunched("org.example.chat", "gone", appInstead = true),
            requests.handle(launchShortcut("org.example.chat", "gone")),
        )
        assertEquals(listOf("org.example.chat"), actions.launched)
        assertTrue(requests.handle(launchShortcut("org.example.missing", "gone")) is RequestResult.LaunchFailed)
    }

    @Test
    fun fr7_launch_shortcut_naming_no_package_no_shortcut_or_the_agent_starts_nothing() {
        assertTrue(requests.handle(launchShortcut("", "new-chat")) is RequestResult.Malformed)
        assertTrue(requests.handle(launchShortcut("not a/package", "new-chat")) is RequestResult.Malformed)
        assertTrue(requests.handle(launchShortcut("org.example.chat", "")) is RequestResult.Malformed)
        assertTrue(requests.handle(launchShortcut(AGENT, "new-chat")) is RequestResult.LaunchFailed)
        assertTrue(actions.shortcutsLaunched.isEmpty())
        assertTrue(actions.launched.isEmpty())
    }

    @Test
    fun fr4_a_resize_and_a_density_for_a_reported_task_are_handed_back() {
        reported += 3

        assertEquals(RequestResult.Resize(3, Size(1280, 720)), requests.handle(resize(3, 1280, 720)))
        assertEquals(RequestResult.Density(3, 320), requests.handle(density(3, 320)))
    }

    private fun caption(taskId: Int, height: Int) = Envelope.newBuilder()
        .setSetTaskCaption(SetTaskCaption.newBuilder().setTaskId(taskId).setHeight(height))
        .build().toByteArray()

    @Test
    fun fr22_the_reach_of_a_windows_icon_and_buttons_is_handed_back_with_its_height() {
        reported += 3
        val frame = Envelope.newBuilder()
            .setSetTaskCaption(SetTaskCaption.newBuilder().setTaskId(3).setHeight(31).setStartWidth(40).setEndWidth(138))
            .build().toByteArray()

        assertEquals(RequestResult.Caption(3, 31, 40, 138), requests.handle(frame))
        val past = Envelope.newBuilder()
            .setSetTaskCaption(SetTaskCaption.newBuilder().setTaskId(3).setHeight(31).setStartWidth(-1))
            .build().toByteArray()
        assertTrue(requests.handle(past) is RequestResult.Malformed)
    }

    @Test
    fun fr22_a_title_bar_height_for_a_reported_task_is_handed_back() {
        reported += 3

        assertEquals(RequestResult.Caption(3, 62), requests.handle(caption(3, 62)))
        assertEquals(RequestResult.Caption(3, 0), requests.handle(caption(3, 0)))
        assertTrue(requests.handle(caption(4, 62)) is RequestResult.SizeRefused)
        // An unsigned height past Int.MAX_VALUE, and one past any display.
        assertTrue(requests.handle(caption(3, -1)) is RequestResult.Malformed)
        assertTrue(requests.handle(caption(3, 20000)) is RequestResult.Malformed)
        assertTrue(actions.captionInsets.isEmpty())
    }

    @Test
    fun fr4_a_resize_says_whether_the_user_is_still_dragging() {
        reported += 3

        assertEquals(RequestResult.Resize(3, Size(1280, 720), live = true), requests.handle(resize(3, 1280, 720, live = true)))
        // The end of the drag is handed back even when the size did not change.
        assertEquals(RequestResult.Resize(3, Size(1280, 720), live = false), requests.handle(resize(3, 1280, 720)))
        assertTrue(actions.resized.isEmpty())
    }

    @Test
    fun fr4_sizes_for_unknown_tasks_and_impossible_sizes_are_refused() {
        reported += 3

        assertTrue(requests.handle(resize(4, 1280, 720)) is RequestResult.SizeRefused)
        assertTrue(requests.handle(density(4, 320)) is RequestResult.SizeRefused)
        assertTrue(requests.handle(resize(3, 0, 720)) is RequestResult.Malformed)
        // An unsigned size past Int.MAX_VALUE.
        assertTrue(requests.handle(resize(3, -1, 720)) is RequestResult.Malformed)
        assertTrue(requests.handle(density(3, 0)) is RequestResult.Malformed)
        assertTrue(requests.handle(density(3, 5000)) is RequestResult.Malformed)
    }

    @Test
    fun fr3_launch_app_starts_the_package() {
        actions.launchable = setOf("org.example.mail")

        val result = requests.handle(launch("org.example.mail"))

        assertEquals(RequestResult.Launched("org.example.mail"), result)
        assertEquals(listOf("org.example.mail"), actions.launched)
    }

    @Test
    fun fr3_launch_app_of_a_package_with_no_launcher_activity_fails() {
        val result = requests.handle(launch("org.example.none"))

        assertTrue(result is RequestResult.LaunchFailed)
    }

    @Test
    fun fr3_launch_app_naming_no_package_or_the_agent_starts_nothing() {
        assertTrue(requests.handle(launch("")) is RequestResult.Malformed)
        assertTrue(requests.handle(launch(AGENT)) is RequestResult.LaunchFailed)
        assertEquals(emptyList<String>(), actions.launched)
    }

    @Test
    fun fr3_close_task_ends_a_reported_task() {
        reported += 12
        actions.closable = setOf(12)

        val result = requests.handle(close(12))

        assertEquals(RequestResult.Closed(12), result)
        assertEquals(listOf(12), actions.closed)
    }

    @Test
    fun fr3_close_task_of_a_task_the_host_was_never_told_about_is_refused() {
        actions.closable = setOf(3)

        val result = requests.handle(close(3))

        assertTrue(result is RequestResult.CloseRefused)
        assertEquals(emptyList<Int>(), actions.closed)
    }

    @Test
    fun fr3_close_task_the_framework_cannot_end_is_refused() {
        reported += 4

        assertTrue(requests.handle(close(4)) is RequestResult.CloseRefused)
        assertEquals(listOf(4), actions.closed)
    }

    @Test
    fun fr3_task_messages_of_other_guest_clients_are_not_requests() {
        val result = requests.handle(Messages.taskUpdated(appTask(5)))

        assertEquals(RequestResult.NotForTheAgent(Envelope.BodyCase.TASK_UPDATED), result)
        assertTrue(actions.launched.isEmpty() && actions.closed.isEmpty())
    }

    @Test
    fun nfr5_a_frame_that_does_not_decode_is_dropped() {
        val garbage = byteArrayOf(0x52, 0x7f, 0x01)

        assertTrue(requests.handle(garbage) is RequestResult.Malformed)
        assertTrue(requests.handle(ByteArray(0)) is RequestResult.Malformed)
        assertTrue(actions.launched.isEmpty() && actions.closed.isEmpty())
    }

    private fun settings(packageName: String) = Envelope.newBuilder()
        .setOpenAppSettings(OpenAppSettings.newBuilder().setPackageName(packageName))
        .build().toByteArray()

    private fun settings(packageName: String, page: AppSettingsPage) = Envelope.newBuilder()
        .setOpenAppSettings(OpenAppSettings.newBuilder().setPackageName(packageName).setPage(page))
        .build().toByteArray()

    private fun newWindow(packageName: String) = Envelope.newBuilder()
        .setLaunchApp(LaunchApp.newBuilder().setPackageName(packageName).setNewWindow(true))
        .build().toByteArray()

    @Test
    fun fr22_the_system_menu_carries_the_android_window_menu_new_window() {
        actions.launchable = setOf("org.example.notes", "org.example.clock")
        actions.multiWindow = setOf("org.example.notes")

        // An app that supports several windows gets another task.
        assertEquals(RequestResult.Launched("org.example.notes", newWindow = true), requests.handle(newWindow("org.example.notes")))
        assertEquals(listOf("org.example.notes"), actions.newWindows)
        // Any other app is launched as before: its task comes to the front.
        assertEquals(RequestResult.Launched("org.example.clock"), requests.handle(newWindow("org.example.clock")))
        assertEquals(listOf("org.example.notes"), actions.newWindows)
        assertEquals(listOf("org.example.clock"), actions.launched)
        // A plain launch never starts another task.
        assertEquals(RequestResult.Launched("org.example.notes"), requests.handle(launch("org.example.notes")))
        assertEquals(listOf("org.example.notes"), actions.newWindows)
        // The agent never opens a window of its own.
        assertTrue(requests.handle(newWindow(AGENT)) is RequestResult.LaunchFailed)
    }

    @Test
    fun fr22_a_new_window_the_framework_refuses_is_a_failed_launch() {
        actions.multiWindow = setOf("org.example.notes")

        assertTrue(requests.handle(newWindow("org.example.notes")) is RequestResult.LaunchFailed)
        assertTrue(actions.launched.isEmpty())
    }

    @Test
    fun fr22_the_system_menu_carries_the_android_window_menu_settings_pages() {
        val notes = "com.example.notes"
        assertEquals(
            RequestResult.SettingsOpened(notes, SettingsPage.ASPECT_RATIO),
            requests.handle(settings(notes, AppSettingsPage.APP_SETTINGS_PAGE_ASPECT_RATIO)),
        )
        assertEquals(
            RequestResult.SettingsOpened(notes, SettingsPage.OPEN_BY_DEFAULT),
            requests.handle(settings(notes, AppSettingsPage.APP_SETTINGS_PAGE_OPEN_BY_DEFAULT)),
        )
        assertEquals(RequestResult.SettingsOpened(notes), requests.handle(settings(notes, AppSettingsPage.APP_SETTINGS_PAGE_DETAILS)))
        // The list of every app has no pages of its own.
        assertEquals(RequestResult.SettingsOpened(null), requests.handle(settings("", AppSettingsPage.APP_SETTINGS_PAGE_ASPECT_RATIO)))
        assertEquals(
            listOf(SettingsPage.ASPECT_RATIO, SettingsPage.OPEN_BY_DEFAULT, SettingsPage.DETAILS, SettingsPage.DETAILS),
            actions.settingsPages,
        )
    }

    @Test
    fun fr22_a_settings_page_this_build_does_not_know_opens_the_apps_info() {
        val unknown = Envelope.newBuilder()
            .setOpenAppSettings(OpenAppSettings.newBuilder().setPackageName("com.example.notes").setPageValue(99))
            .build().toByteArray()

        assertEquals(RequestResult.SettingsOpened("com.example.notes"), requests.handle(unknown))
        assertEquals(listOf(SettingsPage.DETAILS), actions.settingsPages)
    }

    @Test
    fun fr23_app_info_opens_the_apps_settings_and_manage_apps_opens_the_list() {
        assertEquals(RequestResult.SettingsOpened("com.example.notes"), requests.handle(settings("com.example.notes")))
        assertEquals(RequestResult.SettingsOpened(null), requests.handle(settings("")))
        assertEquals(listOf("com.example.notes", null), actions.settingsOpened)
    }

    @Test
    fun fr23_settings_for_a_name_that_is_not_a_package_are_not_opened() {
        assertTrue(requests.handle(settings("../evil")) is RequestResult.Malformed)
        actions.settingsOpen = false
        assertTrue(requests.handle(settings("com.example.notes")) is RequestResult.SettingsFailed)
        assertEquals(listOf<String?>("com.example.notes"), actions.settingsOpened)
    }
}

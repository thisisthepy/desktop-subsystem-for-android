package io.github.thisisthepy.dsa.agent

import android.view.MotionEvent
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskKind
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.windowing.WINDOWING_MODE_FREEFORM
import io.github.thisisthepy.dsa.control.v1.CloseTask
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.KeyAction
import io.github.thisisthepy.dsa.control.v1.KeyEvent
import io.github.thisisthepy.dsa.control.v1.LaunchApp
import io.github.thisisthepy.dsa.control.v1.ResizeTask
import io.github.thisisthepy.dsa.control.v1.SetTaskDensity
import io.github.thisisthepy.dsa.agent.requests.ResizeOutcome
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent
import io.github.thisisthepy.dsa.control.v1.PointerTool
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentTest {
    private val scheduler = FakeScheduler()
    private val tasks = FakeTasks()
    private val actions = FakeActions()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val windowing = FakeWindowing()
    private val log = RecordingLog()
    private val injector = FakeInjector()

    // Where Android's freeform launch put both Settings and Clock on a real machine.
    private val defaultPlace = TaskBounds(686, 53, 1234, 1027)

    private fun started(source: TaskSource = tasks): Agent {
        locator.up = daemon
        val agent = Agent(AGENT, source, actions, locator, windowing, injector, scheduler, log)
        agent.start()
        scheduler.runDue()
        return agent
    }

    @Test
    fun fr3_at_boot_the_display_is_freeform_and_running_app_tasks_are_reported() {
        tasks.running = listOf(appTask(1), appTask(2, packageName = AGENT))

        started()

        assertEquals(WINDOWING_MODE_FREEFORM, windowing.mode)
        assertEquals(listOf(1), daemon.sentMessages().map { it.taskUpdated.taskId })
    }

    @Test
    fun fr3_a_launched_app_is_reported_once_its_task_appears() {
        started()
        actions.launchable = setOf("org.example.mail")

        daemon.deliver(
            Envelope.newBuilder().setLaunchApp(LaunchApp.newBuilder().setPackageName("org.example.mail")).build().toByteArray(),
        )
        scheduler.runDue()
        assertEquals(listOf("org.example.mail"), actions.launched)

        tasks.change(listOf(appTask(9, packageName = "org.example.mail", label = "Mail", focused = true)))
        scheduler.runDue()

        val update = daemon.sentMessages().single().taskUpdated
        assertEquals(9, update.taskId)
        assertEquals("org.example.mail", update.packageName)
        assertEquals("Mail", update.label)
        assertTrue(update.focused)
    }

    @Test
    fun fr3_closing_a_window_ends_its_task_and_reports_it_removed() {
        tasks.running = listOf(appTask(4))
        started()
        actions.closable = setOf(4)

        daemon.deliver(Envelope.newBuilder().setCloseTask(CloseTask.newBuilder().setTaskId(4)).build().toByteArray())
        scheduler.runDue()
        assertEquals(listOf(4), actions.closed)

        tasks.change(emptyList())
        scheduler.runDue()
        assertEquals(4, daemon.sentMessages().last().taskRemoved.taskId)
    }

    @Test
    fun fr3_a_burst_of_task_changes_is_read_once() {
        var reads = 0
        val counting = object : TaskSource by tasks {
            override fun snapshot() = tasks.snapshot().also { reads++ }
        }
        started(counting)
        reads = 0

        repeat(5) { tasks.change(listOf(appTask(1))) }
        scheduler.runDue()

        assertEquals(1, reads)
    }

    @Test
    fun fr3_after_the_daemon_restarts_the_host_is_told_every_task_again() {
        tasks.running = listOf(appTask(1))
        started()
        locator.up = null
        daemon.die()
        scheduler.runDue()

        val restarted = FakeEndpoint()
        locator.up = restarted
        scheduler.advance(1000)

        assertEquals(listOf(1), restarted.sentMessages().map { it.taskUpdated.taskId })
    }

    @Test
    fun fr3_two_apps_launched_at_the_same_place_get_places_of_their_own() {
        tasks.running = listOf(appTask(4, bounds = defaultPlace))
        started()

        tasks.change(listOf(appTask(5, bounds = defaultPlace), appTask(4, bounds = defaultPlace)))
        scheduler.runDue()

        val target = TaskBounds(0, 0, 548, 974)
        assertEquals(listOf(5 to target), actions.moved)
        // The host never hears of the two tasks overlapping: the moving task is reported at
        // the place it is going to.
        val reported = daemon.sentMessages().associate { it.taskUpdated.taskId to it.taskUpdated.bounds }
        assertEquals(686, reported.getValue(4).left)
        assertEquals(0, reported.getValue(5).left)
        assertEquals(548, reported.getValue(5).right)
    }

    @Test
    fun fr3_a_moved_task_is_not_moved_or_reported_again_once_it_lands() {
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()
        val sent = daemon.sent.size

        // The window manager applies the move after the call returns, without telling the
        // task stack listener; the agent's next check sees it.
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = TaskBounds(0, 0, 548, 974)))
        scheduler.advance(2000)

        assertEquals(1, actions.moved.size)
        assertEquals(sent, daemon.sent.size)
    }

    @Test
    fun fr3_a_move_that_never_lands_is_given_up_and_the_task_reported_where_it_is() {
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()

        scheduler.advance(5000)

        assertEquals(1, actions.moved.size)
        val last = daemon.sentMessages().last().taskUpdated
        assertEquals(5, last.taskId)
        assertEquals(686, last.bounds.left)
        assertTrue(log.lines.any { it.startsWith("warn: task 5 did not move") })
    }

    @Test
    fun fr3_a_task_appearing_on_a_full_display_goes_where_it_overlaps_least() {
        val full = listOf(
            appTask(1, bounds = TaskBounds(0, 0, 548, 974)),
            appTask(2, bounds = TaskBounds(548, 0, 1096, 974)),
            appTask(3, bounds = TaskBounds(1096, 0, 1644, 974)),
        )
        tasks.running = full
        started()

        tasks.change(full + appTask(4, bounds = defaultPlace))
        scheduler.runDue()

        assertEquals(listOf(4 to TaskBounds(1372, 106, 1920, 1080)), actions.moved)
        assertTrue(log.lines.any { it.startsWith("warn: the 1920x1080 display has no free place for task 4") })
    }

    @Test
    fun nfr5_a_failed_move_does_not_stop_the_agent() {
        actions.onMove = { _, _ -> throw IllegalArgumentException("cannot resize") }
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()
        scheduler.advance(1000)

        assertEquals(1, actions.moved.size)
        assertEquals(listOf(4, 5), daemon.sentMessages().map { it.taskUpdated.taskId }.sorted())
    }

    @Test
    fun nfr5_an_unreadable_display_size_leaves_tasks_in_place_and_reported() {
        windowing.size = null
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()

        assertTrue(actions.moved.isEmpty())
        assertEquals(listOf(4, 5), daemon.sentMessages().map { it.taskUpdated.taskId }.sorted())
    }

    @Test
    fun fr3_the_agents_home_task_gets_no_window_is_never_moved_and_cannot_be_ended() {
        // The agent's own home activity fills the display behind every app task.
        val home = appTask(3, packageName = AGENT, kind = TaskKind.OTHER, bounds = TaskBounds(0, 0, 1920, 1080))
        tasks.running = listOf(appTask(4, bounds = defaultPlace), home)
        actions.closable = setOf(3, 4)
        started()

        daemon.deliver(Envelope.newBuilder().setCloseTask(CloseTask.newBuilder().setTaskId(3)).build().toByteArray())
        resize(3, 800, 600)
        density(3, 320)
        scheduler.runDue()

        assertEquals(listOf(4), daemon.sentMessages().filter { it.hasTaskUpdated() }.map { it.taskUpdated.taskId })
        assertTrue(daemon.sentMessages().none { it.hasAppInfo() && it.appInfo.packageName == AGENT })
        assertTrue(actions.closed.isEmpty())
        assertTrue(actions.moved.isEmpty())
        assertTrue(actions.resized.isEmpty())
    }

    private fun resize(taskId: Int, width: Int, height: Int, live: Boolean = false) = daemon.deliver(
        Envelope.newBuilder()
            .setResizeTask(ResizeTask.newBuilder().setTaskId(taskId).setWidth(width).setHeight(height).setLive(live))
            .build().toByteArray(),
    )

    private fun density(taskId: Int, densityDpi: Int) = daemon.deliver(
        Envelope.newBuilder()
            .setSetTaskDensity(SetTaskDensity.newBuilder().setTaskId(taskId).setDensityDpi(densityDpi))
            .build().toByteArray(),
    )

    @Test
    fun fr4_resizing_a_window_gives_its_task_the_size_and_tells_the_host_at_once() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(100, 100, 500, 400)))
        started()

        resize(1, 600, 500)
        scheduler.runDue()

        val target = TaskBounds(100, 100, 700, 600)
        assertEquals(listOf(Triple(1, target, null as Int?)), actions.resized)
        // Reported at the size it is taking, before the window manager applies it.
        val reported = daemon.sentMessages().last().taskUpdated
        assertEquals(1, reported.taskId)
        assertEquals(700, reported.bounds.right)
        assertEquals(600, reported.bounds.bottom)
    }

    @Test
    fun fr4_a_density_and_its_size_are_applied_in_one_change() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 548, 974)))
        started()

        density(1, 320)
        resize(1, 1096, 1080)
        scheduler.runDue()

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 1096, 1080), 320 as Int?)), actions.resized)
    }

    @Test
    fun fr4_a_task_that_grows_into_a_newer_one_moves_the_newer_one() {
        tasks.running = listOf(
            appTask(1, bounds = TaskBounds(0, 0, 500, 400)),
            appTask(2, bounds = TaskBounds(500, 0, 900, 400)),
        )
        started()

        resize(1, 700, 400)
        scheduler.runDue()

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 700, 400), null as Int?)), actions.resized)
        assertEquals(listOf(2 to TaskBounds(700, 0, 1100, 400)), actions.moved)
    }

    @Test
    fun fr4_a_newer_task_that_grows_into_an_older_one_moves_itself() {
        tasks.running = listOf(
            appTask(1, bounds = TaskBounds(500, 0, 900, 400)),
            appTask(2, bounds = TaskBounds(0, 0, 500, 400)),
        )
        started()

        resize(2, 400, 700)
        resize(2, 700, 400)
        scheduler.runDue()

        // Only the latest size counts. At its place it would cover task 1, which is older,
        // so it takes its new size at the first free place instead.
        assertEquals(listOf(Triple(2, TaskBounds(900, 0, 1600, 400), null as Int?)), actions.resized)
        assertTrue(actions.moved.isEmpty())
    }

    @Test
    fun fr4_a_task_is_never_larger_than_the_display_nor_past_its_edge() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(1500, 700, 1900, 1000)))
        started()

        resize(1, 4000, 600)
        scheduler.runDue()

        assertEquals(listOf(Triple(1, TaskBounds(0, 480, 1920, 1080), null as Int?)), actions.resized)
    }

    @Test
    fun fr4_the_next_size_waits_for_the_last_one_to_land() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300)
        scheduler.runDue()
        resize(1, 600, 300)
        scheduler.runDue()
        assertEquals(1, actions.resized.size)

        // The window manager applies the first size; the agent sees it within a frame and
        // asks for the next.
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300)))
        scheduler.advance(20)
        assertEquals(TaskBounds(0, 0, 600, 300), actions.resized.last().second)
    }

    @Test
    fun fr4_a_resize_the_window_manager_never_applies_is_given_up() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300)
        scheduler.advance(3000)

        assertTrue(log.lines.any { it.startsWith("warn: task 1 did not take [0,0][500,300]") })
        assertEquals(400, daemon.sentMessages().last().taskUpdated.bounds.right)
    }

    @Test
    fun fr4_a_framework_without_task_densities_still_resizes_and_says_so() {
        actions.onResize = { _, _, densityDpi -> if (densityDpi == null) ResizeOutcome.APPLIED else ResizeOutcome.BOUNDS_ONLY }
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        density(1, 240)
        resize(1, 600, 450)
        scheduler.runDue()

        assertEquals(TaskBounds(0, 0, 600, 450), actions.resized.single().second)
        assertTrue(log.lines.any { it.startsWith("warn: the framework cannot set a task's density") })
    }

    @Test
    fun fr4_a_resize_the_framework_refuses_leaves_the_task_where_it_is() {
        actions.onResize = { _, _, _ -> ResizeOutcome.REFUSED }
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 600, 450)
        scheduler.runDue()

        assertEquals(400, daemon.sentMessages().last().taskUpdated.bounds.right)
        assertTrue(log.lines.any { it.startsWith("warn: the framework refused to give task 1") })
    }

    @Test
    fun fr4_a_resize_outside_a_drag_is_a_plain_resize() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300)
        scheduler.runDue()

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 500, 300), null as Int?)), actions.resized)
        assertTrue(actions.resizedLive.isEmpty())
        assertTrue(actions.endedLive.isEmpty())
    }

    @Test
    fun fr4_a_live_resize_keeps_the_screen_size_the_task_had_when_the_drag_began() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()
        val atStart = ScreenDp(400, 300, 300)

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 500, 300), atStart)), actions.resizedLive)

        // The step lands. Whatever screen size the framework reports now, the next step
        // holds the one from the start of the drag.
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300)))
        scheduler.advance(20)
        resize(1, 600, 300, live = true)
        scheduler.runDue()

        assertEquals(Triple(1, TaskBounds(0, 0, 600, 300), atStart), actions.resizedLive.last())
        assertEquals(2, actions.resizedLive.size)
        assertTrue(actions.resized.isEmpty())
        assertTrue(actions.endedLive.isEmpty())
        // Reported at the size it is taking, as any resize is.
        assertEquals(600, daemon.sentMessages().last().taskUpdated.bounds.right)
    }

    @Test
    fun fr4_the_end_of_a_drag_reconfigures_the_task_once_even_at_an_unchanged_size() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300)))
        scheduler.advance(20)

        // The drag ends where the last step left the task.
        resize(1, 500, 300)
        scheduler.advance(100)

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 500, 300), null as Int?)), actions.endedLive)
        assertTrue(actions.resized.isEmpty())

        // After the drag, a size is a plain resize again.
        resize(1, 550, 300)
        scheduler.runDue()
        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 550, 300), null as Int?)), actions.resized)
        assertEquals(1, actions.endedLive.size)
    }

    @Test
    fun fr4_the_end_of_a_drag_is_not_lost_behind_a_step_that_has_not_landed() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        resize(1, 600, 300, live = true)
        resize(1, 650, 300)
        scheduler.runDue()
        assertEquals(1, actions.resizedLive.size)
        assertTrue(actions.endedLive.isEmpty())

        // The first step lands; the end of the drag goes out next, at its own size.
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300)))
        scheduler.advance(20)

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 650, 300), null as Int?)), actions.endedLive)
        assertEquals(1, actions.resizedLive.size)
    }

    @Test
    fun fr4_a_drag_that_starts_again_before_its_end_is_applied_still_ends() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        resize(1, 520, 300)
        resize(1, 540, 300, live = true)
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300)))
        scheduler.advance(20)

        // The first drag ends at the latest size; the next step starts a new hold from the
        // screen size the task has then.
        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 540, 300), null as Int?)), actions.endedLive)
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 540, 300)))
        scheduler.advance(20)
        resize(1, 560, 300, live = true)
        scheduler.runDue()
        assertEquals(Triple(1, TaskBounds(0, 0, 560, 300), ScreenDp(540, 300, 300)), actions.resizedLive.last())
    }

    @Test
    fun fr4_the_end_of_a_drag_keeps_the_density_the_host_asked_for() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()
        density(1, 320)
        scheduler.runDue()
        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 400, 300), 320 as Int?)), actions.resized)
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300), densityDpi = 320))
        scheduler.advance(20)

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        assertEquals(ScreenDp(200, 150, 150), actions.resizedLive.single().third)
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300), densityDpi = 320))
        scheduler.advance(20)
        resize(1, 500, 300)
        scheduler.runDue()

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 500, 300), 320 as Int?)), actions.endedLive)
    }

    @Test
    fun fr4_a_density_during_a_drag_ends_the_hold_and_the_next_step_holds_anew() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300)))
        started()

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300)))
        scheduler.advance(20)
        density(1, 320)
        resize(1, 600, 300, live = true)
        scheduler.runDue()
        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 600, 300), 320 as Int?)), actions.endedLive)

        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 600, 300), densityDpi = 320))
        scheduler.advance(20)
        resize(1, 700, 300, live = true)
        scheduler.runDue()
        assertEquals(Triple(1, TaskBounds(0, 0, 700, 300), ScreenDp(300, 150, 150)), actions.resizedLive.last())
    }

    @Test
    fun fr4_a_task_dragged_into_a_newer_one_moves_the_newer_one() {
        tasks.running = listOf(
            appTask(1, bounds = TaskBounds(0, 0, 500, 400)),
            appTask(2, bounds = TaskBounds(500, 0, 900, 400)),
        )
        started()

        resize(1, 700, 400, live = true)
        scheduler.runDue()

        assertEquals(TaskBounds(0, 0, 700, 400), actions.resizedLive.single().second)
        assertEquals(listOf(2 to TaskBounds(700, 0, 1100, 400)), actions.moved)
        val reported = daemon.sentMessages().filter { it.hasTaskUpdated() }.map { it.taskUpdated }
        assertEquals(700, reported.last { it.taskId == 1 }.bounds.right)
        assertEquals(700, reported.last { it.taskId == 2 }.bounds.left)
    }

    @Test
    fun fr4_a_task_whose_screen_size_is_unknown_is_resized_plainly_while_dragged() {
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 400, 300), screenDp = null))
        started()

        resize(1, 500, 300, live = true)
        scheduler.runDue()
        tasks.running = listOf(appTask(1, bounds = TaskBounds(0, 0, 500, 300), screenDp = null))
        scheduler.advance(20)
        resize(1, 500, 300)
        scheduler.runDue()

        assertEquals(listOf(Triple(1, TaskBounds(0, 0, 500, 300), null as Int?)), actions.resized)
        assertTrue(actions.resizedLive.isEmpty())
        assertEquals(1, actions.endedLive.size)
    }

    @Test
    fun fr4_a_change_of_screen_size_alone_is_no_news_to_the_host() {
        tasks.running = listOf(appTask(1))
        started()
        val sent = daemon.sent.size

        tasks.change(listOf(appTask(1, screenDp = ScreenDp(1, 2, 1))))
        scheduler.runDue()

        assertEquals(sent, daemon.sent.size)
    }

    @Test
    fun fr4_the_host_hears_each_tasks_density_and_the_largest_size_it_can_have() {
        tasks.running = listOf(appTask(1, densityDpi = 240))
        started()

        val update = daemon.sentMessages().single().taskUpdated
        assertEquals(240, update.densityDpi)
        assertEquals(1920, update.maxWidth)
        assertEquals(1080, update.maxHeight)

        // A new density is news.
        tasks.change(listOf(appTask(1, densityDpi = 320)))
        scheduler.runDue()
        assertEquals(320, daemon.sentMessages().last().taskUpdated.densityDpi)
    }

    @Test
    fun fr4_a_size_for_a_task_the_host_never_heard_of_is_refused() {
        tasks.running = listOf(appTask(1, packageName = AGENT))
        started()

        resize(1, 600, 450)
        scheduler.runDue()

        assertTrue(actions.resized.isEmpty())
        assertTrue(log.lines.any { it.startsWith("warn: did not resize task 1") })
    }

    @Test
    fun nfr5_a_bad_frame_does_not_stop_the_agent() {
        tasks.running = listOf(appTask(1))
        started()

        daemon.deliver(byteArrayOf(0x0a, 0x7f))
        scheduler.runDue()
        tasks.change(listOf(appTask(1), appTask(2)))
        scheduler.runDue()

        assertEquals(listOf(1, 2), daemon.sentMessages().map { it.taskUpdated.taskId })
    }

    private fun keyFrame(taskId: Int, action: KeyAction, keyCode: Int, meta: Int = 0): ByteArray =
        Envelope.newBuilder().setKeyEvent(
            KeyEvent.newBuilder()
                .setTaskId(taskId)
                .setAction(action)
                .setKeyCode(keyCode)
                .setMetaState(meta),
        ).build().toByteArray()

    @Test
    fun fr16_a_key_from_the_host_focuses_its_task_and_reaches_it() {
        tasks.running = listOf(appTask(4), appTask(5, focused = true))
        started()
        val sentBefore = daemon.sent.size

        daemon.deliver(keyFrame(4, KeyAction.KEY_ACTION_DOWN, android.view.KeyEvent.KEYCODE_DPAD_LEFT))
        daemon.deliver(keyFrame(4, KeyAction.KEY_ACTION_UP, android.view.KeyEvent.KEYCODE_DPAD_LEFT))
        // A key for the task that holds the focus does not move it.
        daemon.deliver(keyFrame(5, KeyAction.KEY_ACTION_DOWN, android.view.KeyEvent.KEYCODE_ENTER))
        scheduler.runDue()

        assertEquals(listOf(4), actions.focused)
        assertEquals(
            listOf(
                android.view.KeyEvent.KEYCODE_DPAD_LEFT to android.view.KeyEvent.ACTION_DOWN,
                android.view.KeyEvent.KEYCODE_DPAD_LEFT to android.view.KeyEvent.ACTION_UP,
                android.view.KeyEvent.KEYCODE_ENTER to android.view.KeyEvent.ACTION_DOWN,
            ),
            injector.keys.map { it.keyCode to it.action },
        )
        // Input is not a request: it reports nothing.
        assertEquals(sentBefore, daemon.sent.size)
    }

    private fun pointerFrame(taskId: Int, action: PointerAction, x: Float, y: Float, buttons: Int = 0): ByteArray =
        Envelope.newBuilder().setPointerEvent(
            PointerEvent.newBuilder()
                .setTaskId(taskId)
                .setAction(action)
                .setTool(PointerTool.POINTER_TOOL_MOUSE)
                .setX(x)
                .setY(y)
                .setButtons(buttons)
                .setPressure(if (buttons != 0) 1f else 0f),
        ).build().toByteArray()

    @Test
    fun fr5_a_right_click_from_the_host_lands_in_its_task_as_a_secondary_click() {
        tasks.running = listOf(appTask(4))
        started()
        val sentBefore = daemon.sent.size

        daemon.deliver(pointerFrame(4, PointerAction.POINTER_ACTION_DOWN, 10.5f, 30f, buttons = 2))
        daemon.deliver(pointerFrame(4, PointerAction.POINTER_ACTION_UP, 10.5f, 30f))
        scheduler.runDue()

        assertEquals(
            listOf(
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_BUTTON_PRESS,
                MotionEvent.ACTION_BUTTON_RELEASE,
                MotionEvent.ACTION_UP,
            ),
            injector.injected.map { it.action },
        )
        val down = injector.injected.first()
        assertEquals(MotionEvent.BUTTON_SECONDARY, down.buttonState)
        // appTask(4) sits at (40, 20) on the display.
        assertEquals(50.5f, down.pointers.single().x)
        assertEquals(50f, down.pointers.single().y)
        assertEquals(MotionEvent.BUTTON_SECONDARY, injector.injected[1].actionButton)
        // Input is not a request: it reports nothing.
        assertEquals(sentBefore, daemon.sent.size)
    }

    @Test
    fun fr5_input_for_a_task_the_host_was_never_told_about_is_dropped() {
        tasks.running = listOf(appTask(4), appTask(5, packageName = AGENT))
        started()

        daemon.deliver(pointerFrame(5, PointerAction.POINTER_ACTION_DOWN, 1f, 1f, buttons = 1))
        scheduler.runDue()

        assertTrue(injector.injected.isEmpty())
    }
}

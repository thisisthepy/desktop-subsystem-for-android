package io.github.thisisthepy.dsa.agent.accessibility

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.AccessibilityAction
import io.github.thisisthepy.dsa.control.v1.AccessibilityTree
import io.github.thisisthepy.dsa.control.v1.AccessibilityWatch
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.NodeAction
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** A node in a test: what it says, its children, and what was done to it. */
class FakeNode(
    override val key: Any,
    var info: NodeInfo,
    var children: List<FakeNode> = emptyList(),
) : NodeHandle {
    val performed = mutableListOf<Pair<NodeAction, String>>()
    var reads = 0

    override fun info(): NodeInfo {
        reads++
        return info
    }

    override fun children(): List<NodeHandle> = children

    override fun perform(action: NodeAction, text: String): Boolean {
        performed += action to text
        return action in info.actions
    }
}

/** The guest's accessibility service in a test: on or off, and the windows of each task. */
class FakeAccessibility : AccessibilitySource {
    var on = false
    val calls = mutableListOf<String>()
    var onChange: (() -> Unit)? = null
    val windows = mutableMapOf<Int, List<FakeNode>>()
    var reads = 0

    /** Whether Android has bound the service once it is on; a slow boot binds it late. */
    var bound = true

    override val connected: Boolean
        get() = on && bound

    override fun reconnect() {
        calls += "reconnect"
    }

    override fun enable(onChange: () -> Unit) {
        on = true
        calls += "enable"
        this.onChange = onChange
    }

    override fun disable() {
        on = false
        calls += "disable"
        onChange = null
    }

    override fun roots(taskId: Int): List<NodeHandle>? {
        if (!on || !bound) return null
        reads++
        return windows[taskId].orEmpty()
    }
}

class AccessibilityExportTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val service = FakeAccessibility()
    private val tasks = FakeTasks()

    // Task 3 sits at (30, 20) on the display.
    private val title = FakeNode("title", NodeInfo(className = "android.widget.TextView", text = "Network & internet", bounds = TaskBounds(46, 30, 400, 60)))
    private val row = FakeNode(
        "row",
        NodeInfo(className = "android.widget.LinearLayout", bounds = TaskBounds(30, 20, 430, 100), clickable = true, actions = listOf(NodeAction.NODE_ACTION_CLICK)),
        listOf(title),
    )
    private val hidden = FakeNode("hidden", NodeInfo(className = "android.widget.TextView", text = "off screen", visible = false))
    private val list = FakeNode(
        "list",
        NodeInfo(className = "androidx.recyclerview.widget.RecyclerView", bounds = TaskBounds(30, 20, 430, 620), scrollable = true),
        listOf(row, hidden),
    )

    private fun started(): Agent {
        locator.up = daemon
        tasks.running = listOf(appTask(3, bounds = TaskBounds(30, 20, 430, 620)))
        val agent = Agent(
            AGENT, tasks, FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            accessibility = service,
            clock = { scheduler.now },
        )
        agent.start()
        scheduler.runDue()
        service.windows[3] = listOf(list)
        return agent
    }

    private fun deliver(envelope: Envelope) {
        daemon.deliver(envelope.toByteArray())
        scheduler.runDue()
    }

    private fun watch(taskId: Int, on: Boolean) =
        deliver(Envelope.newBuilder().setAccessibilityWatch(AccessibilityWatch.newBuilder().setTaskId(taskId).setOn(on)).build())

    private fun trees(): List<AccessibilityTree> = daemon.sentMessages().filter { it.hasAccessibilityTree() }.map { it.accessibilityTree }

    @Test
    fun fr18_the_service_reads_nothing_until_the_host_watches_a_task() {
        started()
        // Turned off at boot, in case an earlier agent left it on.
        assertEquals(listOf("disable"), service.calls)
        scheduler.advance(5_000)
        assertFalse(service.on)
        assertEquals(0, service.reads)
        assertTrue(trees().isEmpty())
    }

    @Test
    fun fr18_a_watched_task_gets_its_tree_in_task_window_pixels() {
        started()
        watch(3, true)
        assertTrue(service.on)
        scheduler.advance(10)
        val tree = trees().single()
        assertTrue(tree.full)
        assertEquals(3, tree.taskId)
        assertEquals(1, tree.rootIdsCount)
        // The hidden node is left out.
        assertEquals(3, tree.nodesCount)
        val byText = tree.nodesList.associateBy { it.text }
        val titleNode = byText.getValue("Network & internet")
        assertEquals(16, titleNode.bounds.left)
        assertEquals(10, titleNode.bounds.top)
        val rowNode = tree.nodesList.single { it.className == "android.widget.LinearLayout" }
        assertEquals(listOf(titleNode.id), rowNode.childIdsList)
        assertTrue(rowNode.clickable)
        assertEquals(listOf(NodeAction.NODE_ACTION_CLICK), rowNode.actionsList)
        assertEquals(0, rowNode.bounds.left)
        assertEquals(0, rowNode.bounds.top)
    }

    @Test
    fun fr18_changes_are_batched_sent_as_deltas_and_throttled() {
        started()
        watch(3, true)
        scheduler.advance(10)
        val first = trees().single()
        val titleId = first.nodesList.single { it.text == "Network & internet" }.id
        // A burst of events: read once.
        title.info = title.info.copy(text = "Wi-Fi")
        repeat(20) { service.onChange!!.invoke() }
        scheduler.advance(1_000)
        val delta = trees().drop(1).single()
        assertFalse(delta.full)
        assertEquals(first.epoch, delta.epoch)
        assertEquals(listOf(titleId), delta.nodesList.map { it.id })
        assertEquals("Wi-Fi", delta.nodesList.single().text)
        // Nothing changed: nothing sent.
        service.onChange!!.invoke()
        scheduler.advance(1_000)
        assertEquals(2, trees().size)
        // A node that goes is named as removed.
        list.children = listOf(hidden)
        service.onChange!!.invoke()
        scheduler.advance(1_000)
        val removal = trees().last()
        assertEquals(2, removal.removedIdsCount)
        // However fast changes come, a task's trees are at least the minimum interval apart.
        list.children = listOf(row, hidden)
        val sentAt = mutableListOf<Long>()
        repeat(40) { step ->
            title.info = title.info.copy(text = "step $step")
            service.onChange!!.invoke()
            val before = trees().size
            scheduler.advance(25)
            if (trees().size > before) sentAt += scheduler.now
        }
        assertTrue(sentAt.size in 2..5)
        sentAt.zipWithNext().forEach { (a, b) -> assertTrue(b - a >= AccessibilityExport.MIN_INTERVAL_MILLIS - 25) }
    }

    @Test
    fun fr18_an_action_goes_to_its_node_and_is_answered() {
        started()
        watch(3, true)
        scheduler.advance(10)
        val tree = trees().single()
        val rowId = tree.nodesList.single { it.className == "android.widget.LinearLayout" }.id
        fun act(epoch: Int, node: Int, request: Int) = deliver(
            Envelope.newBuilder().setAccessibilityAction(
                AccessibilityAction.newBuilder().setTaskId(3).setEpoch(epoch).setNodeId(node)
                    .setAction(NodeAction.NODE_ACTION_CLICK).setRequestId(request),
            ).build(),
        )
        act(tree.epoch, rowId, 1)
        assertEquals(listOf(NodeAction.NODE_ACTION_CLICK to ""), row.performed)
        // An earlier epoch's ID names nothing now.
        act(tree.epoch + 7, rowId, 2)
        act(tree.epoch, 999, 3)
        val results = daemon.sentMessages().filter { it.hasAccessibilityActionResult() }.map { it.accessibilityActionResult }
        assertEquals(listOf(1, 2, 3), results.map { it.requestId })
        assertEquals(listOf(true, false, false), results.map { it.done })
        assertTrue(results[1].error.isNotEmpty())
        assertEquals(1, row.performed.size)
    }

    @Test
    fun fr18_the_service_turns_off_when_the_last_watch_ends_and_on_a_new_connection() {
        started()
        watch(3, true)
        watch(4, true)
        watch(3, false)
        assertTrue(service.on)
        watch(4, false)
        assertFalse(service.on)
        watch(3, true)
        assertTrue(service.on)
        // The daemon comes back: the host says again what it watches, so nothing is left on.
        daemon.die()
        locator.up = FakeEndpoint()
        scheduler.advance(2_000)
        assertFalse(service.on)
        assertEquals(listOf("disable", "enable", "disable", "enable", "disable"), service.calls)
    }

    @Test
    fun fr18_a_watched_task_gets_its_tree_when_the_service_comes_up_late_without_an_event() {
        // On the booted image the host watched the task while the user was still being
        // unlocked: the service was turned on but not bound within the first two seconds, and
        // no event of the service came afterwards. The tree never went out, and a screen
        // reader would have found the window empty for as long as it watched.
        started()
        service.bound = false
        watch(3, true)
        scheduler.advance(5_000)
        assertTrue(trees().isEmpty())
        assertTrue(log.lines.any { it.startsWith("warn: task 3 is watched, but its accessibility tree could not be read") })
        // The agent asks Android to bind the service again, once it has had time to do so.
        assertFalse("reconnect" in service.calls)
        scheduler.advance(AccessibilityExport.RECONNECT_MILLIS)
        assertEquals(1, service.calls.count { it == "reconnect" })
        // Android binds it, with no event: the tree goes out at the next try all the same.
        service.bound = true
        scheduler.advance(AccessibilityExport.SLOW_RETRY_MILLIS)
        val tree = trees().single()
        assertTrue(tree.full)
        assertEquals(3, tree.taskId)
        assertTrue(log.lines.any { it.startsWith("info: sent task 3's accessibility tree to the host") })
        // Once connected, it is not asked to bind again.
        scheduler.advance(3 * AccessibilityExport.RECONNECT_MILLIS)
        assertEquals(1, service.calls.count { it == "reconnect" })
    }

    @Test
    fun fr18_a_tree_is_capped_at_the_most_nodes_one_frame_holds() {
        started()
        val many = (0 until AccessibilityExport.MAX_NODES + 50).map { FakeNode("n$it", NodeInfo(className = "android.widget.TextView", text = "item $it")) }
        service.windows[3] = listOf(FakeNode("root", NodeInfo(className = "android.widget.FrameLayout"), many))
        watch(3, true)
        scheduler.advance(10)
        val tree = trees().single()
        assertTrue(tree.nodesCount <= AccessibilityExport.MAX_NODES)
        val ids = tree.nodesList.map { it.id }.toSet()
        tree.nodesList.forEach { node -> assertTrue(node.childIdsList.all { it in ids }) }
        assertTrue(tree.serializedSize < 1_000_000)
    }

    @Test
    fun fr4_a_resized_tasks_tree_is_read_only_once_its_app_has_had_time_to_draw() {
        started()
        watch(3, true)
        scheduler.advance(10)
        assertEquals(1, service.reads)
        assertEquals(1, trees().size)

        fun resize(width: Int) = deliver(
            Envelope.newBuilder()
                .setResizeTask(io.github.thisisthepy.dsa.control.v1.ResizeTask.newBuilder().setTaskId(3).setWidth(width).setHeight(700).setLive(true))
                .build(),
        )
        // The app lays itself out again at each size of a drag; its tree changes meanwhile.
        resize(500)
        title.info = title.info.copy(text = "Wi-Fi")
        service.onChange!!.invoke()
        scheduler.advance(400)
        assertEquals("no read while the app draws its new size", 1, service.reads)
        resize(520)
        service.onChange!!.invoke()
        scheduler.advance(AccessibilityExport.RESIZE_QUIET_MILLIS - 10)
        assertEquals("each new size waits again", 1, service.reads)
        // Once the drag has been still for long enough, the new layout is read once.
        scheduler.advance(20)
        assertEquals(2, service.reads)
        assertEquals(2, trees().size)
        assertEquals("Wi-Fi", trees().last().nodesList.single().text)
    }

    @Test
    fun fr4_a_slow_tree_read_holds_up_no_resize_and_a_change_during_it_is_read_after() {
        // The reader's thread, which this test runs only when it says so: reading an app's tree
        // waits on the app, which is busy laying itself out right after a resize.
        val reader = FakeScheduler()
        val actions = FakeActions()
        locator.up = daemon
        tasks.running = listOf(appTask(3, bounds = TaskBounds(30, 20, 430, 620)))
        val agent = Agent(
            AGENT, tasks, actions, locator, FakeWindowing(), FakeInjector(), scheduler, log,
            accessibility = service,
            accessibilityReader = reader,
            clock = { scheduler.now },
        )
        agent.start()
        scheduler.runDue()
        service.windows[3] = listOf(list)
        watch(3, true)
        scheduler.advance(10)
        // The read is under way on the reader's thread and has not come back.
        assertTrue(trees().isEmpty())

        daemon.deliver(
            Envelope.newBuilder()
                .setResizeTask(io.github.thisisthepy.dsa.control.v1.ResizeTask.newBuilder().setTaskId(3).setWidth(500).setHeight(700))
                .build().toByteArray(),
        )
        scheduler.runDue()
        assertEquals(TaskBounds(30, 20, 530, 720), actions.resized.single().second)

        // The tree changes while it is read: no second read starts beside the first, the read
        // in hand is sent, and the tree is read again once it is done.
        service.onChange!!.invoke()
        scheduler.advance(300)
        reader.runDue()
        assertEquals(1, service.reads)
        scheduler.runDue()
        assertEquals(1, trees().size)
        title.info = title.info.copy(text = "Wi-Fi")
        scheduler.advance(300)
        reader.runDue()
        scheduler.runDue()
        assertEquals(2, service.reads)
        assertEquals("Wi-Fi", trees().last().nodesList.single().text)
    }

    @Test
    fun fr18_a_busy_app_is_not_asked_again_and_again_and_gets_its_tree_once_it_has_time() {
        // Each node read waits for the app's UI thread. Here the app is busy (its main thread
        // blocked, as an app waiting on its video decoder is): a node takes 300 ms to answer.
        var extra = 0L
        var answerMillis = 300L
        var asked = 0
        val busyRoot = object : NodeHandle {
            override val key: Any = "busy-root"

            override fun info(): NodeInfo {
                asked++
                extra += answerMillis
                return list.info
            }

            override fun children(): List<NodeHandle> = listOf(row)

            override fun perform(action: NodeAction, text: String) = false
        }
        val busyService = object : AccessibilitySource {
            var on = false

            override fun enable(onChange: () -> Unit) {
                on = true
            }

            override fun disable() {
                on = false
            }

            override fun roots(taskId: Int): List<NodeHandle>? = if (on && taskId == 3) listOf(busyRoot) else null
        }
        locator.up = daemon
        tasks.running = listOf(appTask(3, bounds = TaskBounds(30, 20, 430, 620)))
        val agent = Agent(
            AGENT, tasks, FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            accessibility = busyService,
            clock = { scheduler.now + extra },
        )
        agent.start()
        scheduler.runDue()
        watch(3, true)
        scheduler.advance(10)
        // The first slow answer ends the read: nothing more is asked, and nothing half read is sent.
        assertEquals(1, asked)
        assertTrue(trees().isEmpty())
        // The app is left alone for a while, however its tree changes meanwhile.
        scheduler.advance(AccessibilityExport.BUSY_RETRY_MILLIS - 100)
        assertEquals(1, asked)
        // Once it answers at once again, its tree is read whole and sent.
        answerMillis = 1
        scheduler.advance(200)
        assertEquals(2, asked)
        val tree = trees().single()
        assertTrue(tree.full)
        assertEquals(3, tree.nodesCount)
    }

    @Test
    fun fr18_password_text_never_leaves_the_guest() {
        val node = AccessibilityExport.toNode(1, emptyList(), NodeInfo(text = "hunter2", password = true, editable = true), TaskBounds(0, 0, 0, 0))
        assertEquals(7, node.text.length)
        assertFalse(node.text.contains("hunter"))
    }

    @Test
    fun fr18_enabling_keeps_the_users_other_services() {
        val own = "io.github.thisisthepy.dsa.agent/.platform.HostAccessibilityService"
        assertEquals(own, AccessibilityExport.withService(null, own))
        assertEquals("a/.B:$own", AccessibilityExport.withService("a/.B", own))
        assertEquals("a/.B:$own", AccessibilityExport.withService("a/.B:$own", own))
        assertEquals("a/.B", AccessibilityExport.withoutService("a/.B:$own", own))
        assertEquals("", AccessibilityExport.withoutService(own, own))
        assertEquals("", AccessibilityExport.withoutService(null, own))
    }
}

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
import io.github.thisisthepy.dsa.control.v1.AccessibilityListening
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

    val arguments = mutableListOf<ActionArguments>()
    val custom = mutableListOf<Int>()
    val followed = mutableListOf<Int>()

    override fun perform(action: NodeAction, arguments: ActionArguments): Boolean {
        this.arguments += arguments
        return perform(action, arguments.text)
    }

    override fun performCustom(id: Int): Boolean {
        custom += id
        return true
    }

    override fun followLink(index: Int): Boolean {
        followed += index
        return true
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

    var onEvent: ((AccessibilityChange) -> Unit)? = null
    val listening = mutableListOf<Boolean>()

    override fun enable(onChange: () -> Unit) {
        on = true
        calls += "enable"
        this.onChange = onChange
    }

    override fun enableEvents(onEvent: (AccessibilityChange) -> Unit) {
        enable { onEvent(AccessibilityChange.ANY) }
        this.onEvent = onEvent
    }

    override fun setListening(listening: Boolean) {
        this.listening += listening
    }

    override fun disable() {
        on = false
        calls += "disable"
        onChange = null
        onEvent = null
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

    private fun listen(taskId: Int, on: Boolean) =
        deliver(Envelope.newBuilder().setAccessibilityListening(AccessibilityListening.newBuilder().setTaskId(taskId).setListening(on)).build())

    /** An event of the service, as it reaches the agent's scheduler. */
    private fun event(change: AccessibilityChange) {
        service.onEvent!!.invoke(change)
        scheduler.runDue()
    }

    private fun focus(node: FakeNode) = event(AccessibilityChange(AccessibilityChange.Kind.FOCUS, 3, node))

    @Test
    fun fr18_without_a_screen_reader_the_waits_stay_as_they_were() {
        assertEquals(100L, AccessibilityExport.BATCH_MILLIS)
        assertEquals(250L, AccessibilityExport.MIN_INTERVAL_MILLIS)
        started()
        watch(3, true)
        scheduler.advance(10)
        // The service keeps its usual events and batching: it was never told otherwise.
        assertTrue(service.listening.isEmpty())
        // A focus event without a screen reader waits for the batch like any change.
        row.info = row.info.copy(focused = true)
        focus(row)
        scheduler.advance(AccessibilityExport.BATCH_MILLIS - 10)
        assertEquals(1, trees().size)
        scheduler.advance(AccessibilityExport.MIN_INTERVAL_MILLIS)
        assertEquals(2, trees().size)
    }

    @Test
    fun fr18_with_a_screen_reader_focus_reaches_the_host_at_once() {
        started()
        watch(3, true)
        scheduler.advance(10)
        listen(3, true)
        assertEquals(listOf(true), service.listening)
        val first = trees().single()
        val rowId = first.nodesList.single { it.className == "android.widget.LinearLayout" }.id
        val reads = list.reads
        // The focus moves to the row: its change goes out with no wait at all, read from the
        // event alone.
        row.info = row.info.copy(focused = true)
        focus(row)
        val delta = trees().last()
        assertEquals(2, trees().size)
        assertFalse(delta.full)
        assertEquals(rowId, delta.focusedId)
        assertTrue(delta.nodesList.single { it.id == rowId }.focused)
        assertEquals("nothing else is read", reads, list.reads)
        // The screen reader goes: the service goes back to its usual events.
        listen(3, false)
        assertEquals(listOf(true, false), service.listening)
    }

    @Test
    fun fr18_a_held_arrow_moves_the_focus_at_least_twenty_times_a_second() {
        started()
        val rows = (0 until 30).map { FakeNode("row$it", NodeInfo(className = "android.widget.TextView", text = "row $it", focusable = true)) }
        service.windows[3] = listOf(FakeNode("root", NodeInfo(className = "android.widget.FrameLayout"), rows))
        watch(3, true)
        scheduler.advance(10)
        listen(3, true)
        val before = trees().size
        // A key held down repeats every 33 ms: each step focuses the next row.
        for (step in 0 until 30) {
            rows.getOrNull(step - 1)?.let { it.info = it.info.copy(focused = false) }
            rows[step].info = rows[step].info.copy(focused = true)
            focus(rows[step])
            scheduler.advance(33)
        }
        val moves = trees().drop(before).map { it.focusedId }.distinct()
        // 30 moves in a second: every one reached the host.
        assertEquals(30, moves.size)
    }

    @Test
    fun fr18_with_a_screen_reader_a_change_reads_only_the_node_it_names() {
        started()
        watch(3, true)
        scheduler.advance(10)
        listen(3, true)
        val titleId = trees().single().nodesList.single { it.text == "Network & internet" }.id
        val listReads = list.reads
        val rowReads = row.reads
        title.info = title.info.copy(text = "Wi-Fi")
        event(AccessibilityChange(AccessibilityChange.Kind.CONTENT, 3, title))
        scheduler.advance(AccessibilityExport.FAST_MIN_INTERVAL_MILLIS + AccessibilityExport.FAST_BATCH_MILLIS)
        val delta = trees().last()
        assertEquals(2, trees().size)
        assertEquals(listOf(titleId), delta.nodesList.map { it.id })
        assertEquals(0, delta.removedIdsCount)
        assertEquals(listReads, list.reads)
        assertEquals(rowReads, row.reads)
        // An event from a task nobody watches is not read at all.
        event(AccessibilityChange(AccessibilityChange.Kind.CONTENT, 9, title))
        scheduler.advance(1_000)
        assertEquals(2, trees().size)
    }

    @Test
    fun fr18_a_busy_apps_first_tree_is_sent_as_far_as_it_was_read() {
        var extra = 0L
        val slowRow = object : NodeHandle {
            override val key: Any = "slow-row"

            override fun info(): NodeInfo {
                extra += 300
                return row.info
            }

            override fun children(): List<NodeHandle> = emptyList()

            override fun perform(action: NodeAction, text: String) = false
        }
        val root = FakeNode("root", NodeInfo(className = "android.widget.FrameLayout"))
        val busy = object : AccessibilitySource {
            override fun enable(onChange: () -> Unit) = Unit

            override fun disable() = Unit

            override fun roots(taskId: Int): List<NodeHandle>? = listOf(
                object : NodeHandle by root {
                    override fun children(): List<NodeHandle> = listOf(slowRow)
                },
            )
        }
        locator.up = daemon
        tasks.running = listOf(appTask(3, bounds = TaskBounds(30, 20, 430, 620)))
        val agent = Agent(
            AGENT, tasks, FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            accessibility = busy,
            clock = { scheduler.now + extra },
        )
        agent.start()
        scheduler.runDue()
        watch(3, true)
        scheduler.advance(10)
        // The root was read before the app showed it was busy: the screen reader has it.
        val tree = trees().single()
        assertTrue(tree.full)
        assertEquals(listOf("android.widget.FrameLayout"), tree.nodesList.map { it.className })
    }

    @Test
    fun fr18_an_action_on_a_tree_read_once_finds_its_node_when_the_task_is_watched_again() {
        started()
        watch(3, true)
        scheduler.advance(10)
        val first = trees().single()
        val rowId = first.nodesList.single { it.className == "android.widget.LinearLayout" }.id
        // The host read the tree once for a client and stopped; the client then acts on it.
        watch(3, false)
        watch(3, true)
        scheduler.advance(10)
        val again = trees().last()
        assertTrue(again.full)
        assertEquals(first.epoch, again.epoch)
        assertEquals(rowId, again.nodesList.single { it.className == "android.widget.LinearLayout" }.id)
        deliver(
            Envelope.newBuilder().setAccessibilityAction(
                AccessibilityAction.newBuilder().setTaskId(3).setEpoch(first.epoch).setNodeId(rowId)
                    .setAction(NodeAction.NODE_ACTION_CLICK).setRequestId(5),
            ).build(),
        )
        assertEquals(listOf(NodeAction.NODE_ACTION_CLICK to ""), row.performed)
    }

    @Test
    fun fr18_links_and_an_apps_own_actions_are_nodes_a_client_can_press() {
        started()
        title.info = title.info.copy(
            links = listOf(TextLink("Learn more", "https://example.com")),
            customActions = listOf(CustomAction(0x7f0a0001, "Archive")),
        )
        watch(3, true)
        scheduler.advance(10)
        val tree = trees().single()
        val link = tree.nodesList.single { it.text == "Learn more" }
        assertTrue(link.link)
        assertEquals("https://example.com", link.url)
        val archive = tree.nodesList.single { it.text == "Archive" }
        val titleNode = tree.nodesList.single { it.text == "Network & internet" }
        assertEquals(listOf(link.id, archive.id), titleNode.childIdsList)
        fun press(id: Int, request: Int) = deliver(
            Envelope.newBuilder().setAccessibilityAction(
                AccessibilityAction.newBuilder().setTaskId(3).setEpoch(tree.epoch).setNodeId(id)
                    .setAction(NodeAction.NODE_ACTION_CLICK).setRequestId(request),
            ).build(),
        )
        press(link.id, 1)
        press(archive.id, 2)
        assertEquals(listOf(0), title.followed)
        assertEquals(listOf(0x7f0a0001), title.custom)
    }

    @Test
    fun fr18_selection_actions_carry_their_arguments() {
        started()
        val field = FakeNode(
            "field",
            NodeInfo(className = "android.widget.EditText", text = "hello", editable = true, selectionStart = 2, selectionEnd = 4, actions = listOf(NodeAction.NODE_ACTION_SET_SELECTION)),
        )
        service.windows[3] = listOf(field)
        watch(3, true)
        scheduler.advance(10)
        val node = trees().single().nodesList.single()
        assertTrue(node.hasTextSelection)
        assertEquals(2, node.textSelectionStart)
        assertEquals(4, node.textSelectionEnd)
        deliver(
            Envelope.newBuilder().setAccessibilityAction(
                AccessibilityAction.newBuilder().setTaskId(3).setEpoch(trees().single().epoch).setNodeId(node.id)
                    .setAction(NodeAction.NODE_ACTION_SET_SELECTION).setSelectionStart(1).setSelectionEnd(3).setRequestId(1),
            ).build(),
        )
        assertEquals(ActionArguments(selectionStart = 1, selectionEnd = 3), field.arguments.single())
    }

    @Test
    fun fr18_announcements_and_toasts_reach_the_host_only_while_a_screen_reader_listens() {
        started()
        watch(3, true)
        scheduler.advance(10)
        fun announcements() = daemon.sentMessages().filter { it.hasAccessibilityAnnouncement() }.map { it.accessibilityAnnouncement }
        event(AccessibilityChange(AccessibilityChange.Kind.TOAST, text = "Saved"))
        assertTrue(announcements().isEmpty())
        listen(3, true)
        event(AccessibilityChange(AccessibilityChange.Kind.TOAST, text = "Saved"))
        event(AccessibilityChange(AccessibilityChange.Kind.ANNOUNCEMENT, 3, text = "Page 2"))
        val said = announcements()
        assertEquals(listOf("Saved", "Page 2"), said.map { it.text })
        assertEquals(listOf(true, false), said.map { it.toast })
        assertEquals(listOf(3, 3), said.map { it.taskId })
    }

    @Test
    fun fr18_accessibility_focus_and_collections_are_exported() {
        val node = AccessibilityExport.toNode(
            1, emptyList(),
            NodeInfo(
                accessibilityFocused = true,
                collectionItem = CollectionItemInfo(row = 4, rowSpan = 1, column = 0, columnSpan = 1),
                liveRegion = 1,
                paneTitle = "Wi-Fi",
                heading = true,
                headingLevel = 2,
                window = WindowInfo("Delete?", modal = true, popup = false),
            ),
            TaskBounds(0, 0, 0, 0),
        )
        assertTrue(node.accessibilityFocused)
        assertEquals(4, node.collectionItem.row)
        assertEquals(1, node.liveRegion)
        assertEquals("Wi-Fi", node.paneTitle)
        assertEquals(2, node.headingLevel)
        assertTrue(node.window.modal)
        assertEquals("Delete?", node.window.title)
        // A node that is not editable says no selection.
        assertFalse(AccessibilityExport.toNode(1, emptyList(), NodeInfo(selectionStart = 1, selectionEnd = 1), TaskBounds(0, 0, 0, 0)).hasTextSelection)
    }

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
    fun nfr5_an_event_whose_app_does_not_answer_never_holds_up_the_thread_that_delivers_it() {
        // Android calls the service on the agent's UI thread, which also takes HomeActivity's
        // input. Asking Android about an event's node waits on the app that owns the window, up
        // to five seconds when that app is closing: input to HomeActivity then timed out.
        val reader = FakeScheduler()
        locator.up = daemon
        tasks.running = listOf(appTask(3, bounds = TaskBounds(30, 20, 430, 620)))
        Agent(
            AGENT, tasks, FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            accessibility = service,
            accessibilityReader = reader,
            clock = { scheduler.now },
        ).start()
        scheduler.runDue()
        service.windows[3] = listOf(list)
        watch(3, true)
        scheduler.advance(10)
        reader.runDue()
        scheduler.runDue()
        listen(3, true)
        val sent = trees().size

        var asked = 0
        row.info = row.info.copy(focused = true)
        // The thread that delivers the event asks nothing: the question waits on the reader.
        service.onEvent!!.invoke(
            AccessibilityChange(
                AccessibilityChange.Kind.FOCUS,
                resolve = {
                    asked++
                    AccessibilityChange(AccessibilityChange.Kind.FOCUS, 3, row)
                },
            ),
        )
        assertEquals(0, asked)
        scheduler.runDue()
        assertEquals("the scheduler asks nothing either", 0, asked)
        reader.runDue()
        assertEquals(1, asked)
        scheduler.advance(AccessibilityExport.MIN_INTERVAL_MILLIS)
        reader.runDue()
        scheduler.runDue()
        assertTrue(trees().size > sent)
        assertTrue(trees().last().nodesList.any { it.focused })

        // An event about nothing the export follows (an answer of null) changes nothing, and
        // one whose question fails is read as a change somewhere, not a crash.
        service.onEvent!!.invoke(AccessibilityChange(AccessibilityChange.Kind.CONTENT, resolve = { null }))
        service.onEvent!!.invoke(AccessibilityChange(AccessibilityChange.Kind.CONTENT, resolve = { throw IllegalStateException("window gone") }))
        reader.runDue()
        scheduler.runDue()
        assertTrue(log.lines.any { it.contains("could not read what an accessibility event was about") })
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

    /** A clock widget's views: its time, and a button that opens its app. */
    private val time = FakeNode("time", NodeInfo(className = "android.widget.TextClock", text = "10:42", bounds = TaskBounds(10, 10, 150, 60)))
    private val openApp = FakeNode(
        "open",
        NodeInfo(
            className = "android.widget.ImageButton",
            description = "Open Clock",
            bounds = TaskBounds(120, 70, 170, 100),
            clickable = true,
            actions = listOf(NodeAction.NODE_ACTION_CLICK),
        ),
    )
    private val widgetRoot = FakeNode("widget", NodeInfo(className = "android.widget.FrameLayout", bounds = TaskBounds(0, 0, 180, 110)), listOf(time, openApp))

    /** The agent's widgets in a test: the views of widget 12, and who listens to them. */
    private val widgetViews = object : WidgetNodes {
        var reads = 0
        var listener: ((Int) -> Unit)? = null

        override fun roots(widgetId: Int): List<NodeHandle>? {
            reads++
            return if (widgetId == 12) listOf(widgetRoot) else null
        }

        override fun bounds(widgetId: Int): TaskBounds? = if (widgetId == 12) TaskBounds(0, 0, 180, 110) else null

        override fun listen(changed: ((Int) -> Unit)?) {
            listener = changed
        }
    }

    private fun startedWithWidgets(): Agent {
        locator.up = daemon
        tasks.running = listOf(appTask(3, bounds = TaskBounds(30, 20, 430, 620)))
        val agent = Agent(
            AGENT, tasks, FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            accessibility = service,
            clock = { scheduler.now },
            widgetNodes = widgetViews,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun watchWidget(widgetId: Int, on: Boolean) =
        deliver(Envelope.newBuilder().setAccessibilityWatch(AccessibilityWatch.newBuilder().setWidgetId(widgetId).setOn(on)).build())

    @Test
    fun fr18_widget_tree_watch_names_the_widget() {
        startedWithWidgets()
        // Nothing is read until the host watches the widget.
        scheduler.advance(5_000)
        assertEquals(0, widgetViews.reads)
        assertEquals(null, widgetViews.listener)
        watchWidget(12, true)
        scheduler.advance(10)
        // A widget's views are the agent's own: the accessibility service stays off.
        assertFalse(service.on)
        assertEquals(listOf("disable"), service.calls)
        val tree = trees().single()
        assertTrue(tree.full)
        assertEquals(12, tree.widgetId)
        assertEquals(0, tree.taskId)
        assertEquals(3, tree.nodesCount)
        val button = tree.nodesList.single { it.contentDescription == "Open Clock" }
        // In pixels of the widget's image, from its top left.
        assertEquals(120, button.bounds.left)
        assertEquals(70, button.bounds.top)
        assertEquals(listOf(NodeAction.NODE_ACTION_CLICK), button.actionsList)
        assertTrue(log.lines.any { it.startsWith("info: sent widget 12's accessibility tree to the host") })
        // A change of its views is read again and sent as a change.
        time.info = time.info.copy(text = "10:43")
        widgetViews.listener!!.invoke(12)
        scheduler.advance(1_000)
        val delta = trees().last()
        assertFalse(delta.full)
        assertEquals(12, delta.widgetId)
        assertEquals("10:43", delta.nodesList.single().text)
        // Once the host stops watching, nothing listens and nothing is read.
        watchWidget(12, false)
        assertEquals(null, widgetViews.listener)
        val reads = widgetViews.reads
        scheduler.advance(5_000)
        assertEquals(reads, widgetViews.reads)
    }

    @Test
    fun fr18_invoke_on_a_widget_button_presses_it() {
        startedWithWidgets()
        watchWidget(12, true)
        scheduler.advance(10)
        val tree = trees().single()
        val buttonId = tree.nodesList.single { it.contentDescription == "Open Clock" }.id
        deliver(
            Envelope.newBuilder().setAccessibilityAction(
                AccessibilityAction.newBuilder().setWidgetId(12).setEpoch(tree.epoch).setNodeId(buttonId)
                    .setAction(NodeAction.NODE_ACTION_CLICK).setRequestId(4),
            ).build(),
        )
        assertEquals(listOf(NodeAction.NODE_ACTION_CLICK to ""), openApp.performed)
        val result = daemon.sentMessages().filter { it.hasAccessibilityActionResult() }.map { it.accessibilityActionResult }.single()
        assertEquals(4, result.requestId)
        assertEquals(12, result.widgetId)
        assertTrue(result.done)
        // A task of the same number is another tree: the action does not reach the widget.
        deliver(
            Envelope.newBuilder().setAccessibilityAction(
                AccessibilityAction.newBuilder().setTaskId(12).setEpoch(tree.epoch).setNodeId(buttonId)
                    .setAction(NodeAction.NODE_ACTION_CLICK).setRequestId(5),
            ).build(),
        )
        assertEquals(1, openApp.performed.size)
    }

    @Test
    fun fr18_a_watched_task_and_widget_keep_the_service_for_the_task_alone() {
        startedWithWidgets()
        service.windows[3] = listOf(list)
        watchWidget(12, true)
        watch(3, true)
        assertTrue(service.on)
        // The widget is still read once the task's watch ends, and the service goes off.
        watch(3, false)
        assertFalse(service.on)
        assertTrue(widgetViews.listener != null)
        watchWidget(12, false)
        assertEquals(listOf("disable", "enable", "disable"), service.calls)
    }
}

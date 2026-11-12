package io.github.thisisthepy.dsa.agent.accessibility

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.AccessibilityAction
import io.github.thisisthepy.dsa.control.v1.AccessibilityActionResult
import io.github.thisisthepy.dsa.control.v1.AccessibilityNode
import io.github.thisisthepy.dsa.control.v1.AccessibilityTree
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.NodeAction
import io.github.thisisthepy.dsa.control.v1.NodeRange
import io.github.thisisthepy.dsa.control.v1.Rect

/** A node's range, for a slider or a progress bar. */
data class RangeInfo(val min: Float, val max: Float, val current: Float)

/** What one accessibility node says about itself, read once. Bounds are in display pixels. */
data class NodeInfo(
    val className: String = "",
    val text: String = "",
    val description: String = "",
    val hint: String = "",
    val viewId: String = "",
    val stateDescription: String = "",
    val bounds: TaskBounds = TaskBounds(0, 0, 0, 0),
    val visible: Boolean = true,
    val checkable: Boolean = false,
    val checked: Boolean = false,
    val selected: Boolean = false,
    val enabled: Boolean = true,
    val focusable: Boolean = false,
    val focused: Boolean = false,
    val scrollable: Boolean = false,
    val clickable: Boolean = false,
    val longClickable: Boolean = false,
    val editable: Boolean = false,
    val password: Boolean = false,
    val heading: Boolean = false,
    val range: RangeInfo? = null,
    val actions: List<NodeAction> = emptyList(),
)

/** One node of a window, as the guest's accessibility framework hands it out. */
interface NodeHandle {
    /** Equal for the same node read twice; the node's ID within an epoch follows it. */
    val key: Any

    fun info(): NodeInfo

    fun children(): List<NodeHandle>

    /** Does [action] on the node; [text] is the new text of a set-text action. */
    fun perform(action: NodeAction, text: String): Boolean
}

/** The guest's accessibility service, which the export turns on only while it is needed. */
interface AccessibilitySource {
    /**
     * Turns the service on. [onChange] is called, from any thread, whenever the content of a
     * window may have changed.
     */
    fun enable(onChange: () -> Unit)

    /** Turns the service off: it then reads nothing and receives no events. */
    fun disable()

    /** The root node of each window of the task, in order; null while the service is not yet on. */
    fun roots(taskId: Int): List<NodeHandle>?

    /** Whether Android has bound the service, so it can read windows. */
    val connected: Boolean
        get() = true

    /** How the service stands, as Android sees it, for a warning when it does not come up. */
    fun describe(): String = if (connected) "the service is connected" else "the service is not connected"

    /**
     * Asks Android again to bind the service, which it did not do when the service was turned
     * on. Only called while the service is on and not connected.
     */
    fun reconnect() = Unit

    companion object {
        val NONE = object : AccessibilitySource {
            override fun enable(onChange: () -> Unit) = Unit

            override fun disable() = Unit

            override fun roots(taskId: Int): List<NodeHandle>? = null

            override val connected: Boolean
                get() = false
        }
    }
}

/**
 * Exports the accessibility tree of the tasks the host watches.
 *
 * While no task is watched, the guest's accessibility service is off: Android then tells no
 * app that accessibility is on, no app builds accessibility events, and the agent reads
 * nothing, so a user who uses no host accessibility client pays nothing. The first
 * AccessibilityWatch turns it on; the last one turned off, or a new connection, turns it off.
 *
 * For a watched task the agent sends a full tree, then only what changed. A change in any
 * window starts a short wait of [BATCH_MILLIS], so the burst of events one screen change makes
 * is read once, and two trees of a task are never sent less than [MIN_INTERVAL_MILLIS] apart:
 * at most four a second, while a screen reader still hears of a change well within a second.
 * A tree holds at most [MAX_NODES] nodes and [MAX_TREE_BYTES] bytes, the nearest nodes first,
 * so one frame stays far below what the binder call to the daemon may carry.
 *
 * Reading a tree is a binder call into the app for each window and for many of its nodes, and
 * an app that is busy laying itself out (as every app is right after its window is resized)
 * answers each one only when its UI thread is free. A large tree can take a second or more to
 * read that way, so the nodes are read on [reader], a thread of their own, and only what was
 * read is turned into a tree and sent on the scheduler's thread. The scheduler's thread is
 * never held up by a slow app: the host's requests (a window's new size above all) and the
 * task reports go on meanwhile. A task's tree is read by one read at a time; a change that
 * comes during a read is read once that read is done.
 *
 * Each of those calls also takes the app's UI thread away from laying itself out and drawing.
 * Right after its window is resized that is the frame the user waits for, and on a guest
 * with few processors a tree read then held a Settings window's first frame at its new size
 * back by half a second. So a task's tree is not read while the host resizes it ([resizing]):
 * a read waits until [RESIZE_QUIET_MILLIS] have passed since the task's last new size or
 * density, and the screen reader hears of the new layout once, after it has been drawn.
 *
 * A watched task gets its tree however long Android takes to bind the service. The export
 * tries every [RETRY_MILLIS] for [MAX_RETRIES] tries, then every [SLOW_RETRY_MILLIS] for as long
 * as the task is watched, rather than waiting for an event from a service that may never have
 * come up: a screen reader that got no tree would otherwise find the window empty for good.
 * When the service has not come up by then, the agent says why it can see (how Android lists
 * the service) and asks Android to bind it again, at most every [RECONNECT_MILLIS].
 *
 * Every method runs on the agent's scheduler thread.
 */
class AccessibilityExport(
    private val source: AccessibilitySource,
    private val scheduler: Scheduler,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val taskBounds: (Int) -> TaskBounds?,
    private val clock: () -> Long = { System.nanoTime() / 1_000_000 },
    /** Where the nodes are read: a thread of their own on the guest, the scheduler in tests. */
    private val reader: Scheduler = scheduler,
) {
    private class TaskState {
        var epoch = 0
        var needFull = true
        var pending = false
        var lastSentAt = Long.MIN_VALUE / 2
        var nextId = 1
        val ids = HashMap<Any, Int>()
        var sent: Map<Int, AccessibilityNode> = emptyMap()
        var sentRoots: List<Int> = emptyList()
        var sentFocus = 0
        var handles: Map<Int, NodeHandle> = emptyMap()
        var retries = 0

        /** Whether [reader] is reading the tree now. */
        var reading = false

        /** Whether the tree changed while it was being read, so it is read again. */
        var changedWhileReading = false

        /** When the host asked for the tree, on [clock]. */
        var watchedAt = 0L

        /** Whether the first tree of this watch went out, which the guest's log says once. */
        var announced = false

        /** Whether the guest's log said that the tree could not be read yet. */
        var warned = false

        /** Until when, on [clock], the task is being resized and its tree is not read. */
        var quietAt = Long.MIN_VALUE / 2
    }

    private val watched = HashMap<Int, TaskState>()
    private var enabled = false
    private var epochs = 0

    /** When the service may next be asked to bind again, on [clock]. */
    private var reconnectAt = 0L

    /** Whether the accessibility service is on. */
    val active: Boolean
        get() = enabled

    /** The tasks the host watches. */
    val tasks: Set<Int>
        get() = watched.keys.toSet()

    /** At boot: a service left on by an agent that died is turned off. */
    fun start() {
        try {
            source.disable()
        } catch (e: Exception) {
            log.warn("could not turn the accessibility service off", e)
        }
    }

    /** A new connection: the host says again which tasks it watches. */
    fun connected() {
        watched.clear()
        turnOff()
    }

    /** Takes a watch or an action. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.ACCESSIBILITY_WATCH -> {
                val watch = envelope.accessibilityWatch
                if (watch.on) watch(watch.taskId) else unwatch(watch.taskId)
            }
            Envelope.BodyCase.ACCESSIBILITY_ACTION -> perform(envelope.accessibilityAction)
            else -> return false
        }
        return true
    }

    /**
     * The host gave task [taskId] a new size or density: its app is laying itself out again,
     * and its tree is not read until it has had [RESIZE_QUIET_MILLIS] to draw.
     */
    fun resizing(taskId: Int) {
        watched[taskId]?.quietAt = clock() + RESIZE_QUIET_MILLIS
    }

    /** The content of some window may have changed. Called on the scheduler thread. */
    fun changed() {
        for ((taskId, state) in watched) schedule(taskId, state, BATCH_MILLIS)
    }

    private fun watch(taskId: Int) {
        val state = watched.getOrPut(taskId) { TaskState() }
        state.needFull = true
        state.retries = 0
        state.watchedAt = clock()
        state.announced = false
        state.warned = false
        if (!enabled) {
            enabled = true
            reconnectAt = clock() + RECONNECT_MILLIS
            log.info("a host accessibility client watches task $taskId; the accessibility service is on")
            try {
                source.enable { scheduler.post(::changed) }
            } catch (e: Exception) {
                log.warn("could not turn the accessibility service on", e)
            }
        }
        schedule(taskId, state, 0)
    }

    /**
     * The task's tree could not be read yet: the service is still coming up, or the task is
     * not placed yet. Tries again soon a few times, then slowly for as long as it is watched.
     */
    private fun retry(taskId: Int, state: TaskState, why: String) {
        if (state.retries++ < MAX_RETRIES) {
            schedule(taskId, state, RETRY_MILLIS)
            return
        }
        if (!state.warned) {
            state.warned = true
            log.warn(
                "task $taskId is watched, but its accessibility tree could not be read " +
                    "${clock() - state.watchedAt} ms after the host asked: $why (${describeSource()}); " +
                    "trying again every ${SLOW_RETRY_MILLIS / 1000} s",
            )
        }
        val connected = try {
            source.connected
        } catch (e: Exception) {
            false
        }
        if (enabled && !connected && clock() >= reconnectAt) {
            reconnectAt = clock() + RECONNECT_MILLIS
            log.info("asking Android again to bind the agent's accessibility service (${describeSource()})")
            try {
                source.reconnect()
            } catch (e: Exception) {
                log.warn("could not ask Android again to bind the accessibility service", e)
            }
        }
        schedule(taskId, state, SLOW_RETRY_MILLIS)
    }

    private fun describeSource(): String = try {
        source.describe()
    } catch (e: Exception) {
        "the service's state could not be read: ${e.message}"
    }

    private fun unwatch(taskId: Int) {
        if (watched.remove(taskId) == null) return
        if (watched.isEmpty()) turnOff()
    }

    private fun turnOff() {
        if (!enabled) return
        enabled = false
        log.info("no host accessibility client watches a task; the accessibility service is off")
        try {
            source.disable()
        } catch (e: Exception) {
            log.warn("could not turn the accessibility service off", e)
        }
    }

    private fun schedule(taskId: Int, state: TaskState, wait: Long) {
        if (state.pending) return
        state.pending = true
        val now = clock()
        val delay = maxOf(wait, state.lastSentAt + MIN_INTERVAL_MILLIS - now, state.quietAt - now, 0L)
        scheduler.postDelayed(delay) {
            state.pending = false
            if (watched[taskId] === state) read(taskId, state)
        }
    }

    /** Reads the task's nodes on [reader], then sends what changed from the scheduler's thread. */
    private fun read(taskId: Int, state: TaskState) {
        if (state.reading) {
            state.changedWhileReading = true
            return
        }
        // Resized since the read was planned: it waits for the app to have drawn.
        if (clock() < state.quietAt) {
            schedule(taskId, state, 0)
            return
        }
        val bounds = taskBounds(taskId)
        if (bounds == null) {
            retry(taskId, state, "the task is not placed yet")
            return
        }
        state.reading = true
        state.changedWhileReading = false
        reader.post {
            val read = try {
                source.roots(taskId)?.let { roots -> readNodes(roots, bounds) }
            } catch (e: Exception) {
                log.warn("reading the accessibility nodes of task $taskId failed", e)
                null
            }
            scheduler.post {
                state.reading = false
                if (watched[taskId] !== state) return@post
                sendRead(taskId, state, read)
                if (state.changedWhileReading) {
                    state.changedWhileReading = false
                    schedule(taskId, state, BATCH_MILLIS)
                }
            }
        }
    }

    private fun sendRead(taskId: Int, state: TaskState, read: ReadNodes?) {
        val bounds = taskBounds(taskId)
        if (read == null) {
            retry(taskId, state, "the accessibility service gave no windows")
            return
        }
        if (bounds == null) {
            retry(taskId, state, "the task is not placed yet")
            return
        }
        state.retries = 0
        val full = state.needFull || state.ids.size > MAX_IDS
        if (full) {
            state.ids.clear()
            state.nextId = 1
            state.epoch = ++epochs
            state.sent = emptyMap()
            state.sentRoots = emptyList()
            state.sentFocus = 0
        }
        val snapshot = snapshot(state, read, bounds)
        val changed = snapshot.nodes.values.filter { state.sent[it.id] != it }
        val removed = state.sent.keys.filter { it !in snapshot.nodes }
        if (!full && changed.isEmpty() && removed.isEmpty() && snapshot.roots == state.sentRoots && snapshot.focus == state.sentFocus) {
            state.handles = snapshot.handles
            return
        }
        val tree = AccessibilityTree.newBuilder()
            .setTaskId(taskId)
            .setEpoch(state.epoch)
            .setFull(full)
            .addAllRootIds(snapshot.roots)
            .addAllNodes(if (full) snapshot.nodes.values else changed)
            .addAllRemovedIds(if (full) emptyList() else removed)
            .setFocusedId(snapshot.focus)
            .build()
        if (!send(Envelope.newBuilder().setAccessibilityTree(tree).build().toByteArray())) {
            log.warn("the host did not take the accessibility tree of task $taskId; sending it whole next time")
            state.needFull = true
            return
        }
        state.needFull = false
        if (!state.announced) {
            state.announced = true
            log.info("sent task $taskId's accessibility tree to the host: ${snapshot.nodes.size} nodes, ${clock() - state.watchedAt} ms after it asked")
        }
        state.lastSentAt = clock()
        state.sent = snapshot.nodes
        state.sentRoots = snapshot.roots
        state.sentFocus = snapshot.focus
        state.handles = snapshot.handles
        if (snapshot.truncated) log.warn("task $taskId has more accessibility nodes than one tree holds; the farthest were left out")
    }

    private class Snapshot(
        val roots: List<Int>,
        val nodes: Map<Int, AccessibilityNode>,
        val handles: Map<Int, NodeHandle>,
        val focus: Int,
        val truncated: Boolean,
    )

    /** One node as [readNodes] read it: the node, what it said, and its children's places. */
    private class ReadNode(val handle: NodeHandle, val info: NodeInfo, val children: MutableList<Int> = mutableListOf())

    /** The nodes of a task's windows, breadth first, and the places of the roots among them. */
    private class ReadNodes(val nodes: List<ReadNode>, val roots: List<Int>, val truncated: Boolean)

    /**
     * Reads the task's nodes breadth first, so a cut tree keeps the nodes nearest the top. Runs
     * on [reader]: it only asks the nodes, and touches none of the export's state.
     */
    private fun readNodes(roots: List<NodeHandle>, task: TaskBounds): ReadNodes {
        val nodes = ArrayList<ReadNode>()
        val seen = HashSet<Any>()
        val rootPlaces = mutableListOf<Int>()
        var bytes = 0
        var truncated = false
        for (root in roots) {
            val info = root.info()
            if (!info.visible) continue
            if (!seen.add(root.key)) continue
            if (seen.size > MAX_NODES) {
                truncated = true
                break
            }
            rootPlaces += nodes.size
            nodes += ReadNode(root, info)
        }
        var next = 0
        while (next < nodes.size) {
            val place = next++
            val node = nodes[place]
            val children = try {
                node.handle.children()
            } catch (e: Exception) {
                emptyList()
            }
            for (child in children) {
                if (seen.size >= MAX_NODES || bytes >= MAX_TREE_BYTES) {
                    truncated = true
                    break
                }
                val childInfo = try {
                    child.info()
                } catch (e: Exception) {
                    continue
                }
                if (!childInfo.visible) continue
                if (!seen.add(child.key)) continue
                node.children += nodes.size
                nodes += ReadNode(child, childInfo)
            }
            // The size the node takes in a tree; its place stands in for its ID, which is about
            // as long.
            bytes += toNode(place + 1, node.children.map { it + 1 }, node.info, task).serializedSize
        }
        return ReadNodes(nodes, rootPlaces, truncated)
    }

    /** What was read, with the task's IDs for its nodes: a node read before keeps its ID. */
    private fun snapshot(state: TaskState, read: ReadNodes, task: TaskBounds): Snapshot {
        val idOf = { handle: NodeHandle -> state.ids.getOrPut(handle.key) { state.nextId++ } }
        val ids = read.nodes.map { idOf(it.handle) }
        val nodes = LinkedHashMap<Int, AccessibilityNode>()
        val handles = HashMap<Int, NodeHandle>()
        var focus = 0
        for ((place, node) in read.nodes.withIndex()) {
            val id = ids[place]
            nodes[id] = toNode(id, node.children.map { ids[it] }, node.info, task)
            handles[id] = node.handle
            if (node.info.focused && focus == 0) focus = id
        }
        return Snapshot(read.roots.map { ids[it] }, nodes, handles, focus, read.truncated)
    }

    private fun perform(action: AccessibilityAction) {
        val state = watched[action.taskId]
        val error = when {
            state == null -> "nobody watches task ${action.taskId}"
            action.epoch != state.epoch -> "the screen changed; the node is no longer there"
            action.action == NodeAction.NODE_ACTION_UNSPECIFIED || action.action == NodeAction.UNRECOGNIZED -> "the action is not one the agent knows"
            else -> {
                val handle = state.handles[action.nodeId]
                when {
                    handle == null -> "the node is no longer there"
                    else -> try {
                        if (handle.perform(action.action, action.text)) null else "the app did not do it"
                    } catch (e: Exception) {
                        log.warn("an accessibility action failed", e)
                        "the app did not do it: ${e.message}"
                    }
                }
            }
        }
        val result = AccessibilityActionResult.newBuilder()
            .setTaskId(action.taskId)
            .setRequestId(action.requestId)
            .setDone(error == null)
            .setError(error ?: "")
            .build()
        send(Envelope.newBuilder().setAccessibilityActionResult(result).build().toByteArray())
        if (state != null) schedule(action.taskId, state, BATCH_MILLIS)
    }

    companion object {
        /** How long a change waits for the rest of its burst. */
        const val BATCH_MILLIS = 100L

        /** The least time between two trees of a task: at most four a second. */
        const val MIN_INTERVAL_MILLIS = 250L

        /**
         * How long after a task's last new size or density its tree waits to be read: about
         * as long as an app takes to draw at a new size on a slow guest, and short enough that
         * a screen reader hears of the new layout within a second of the resize.
         */
        const val RESIZE_QUIET_MILLIS = 500L

        /** How long to wait for a service that is still starting, or a task not yet placed. */
        const val RETRY_MILLIS = 200L

        /** How many times to wait for it, two seconds in all. */
        const val MAX_RETRIES = 10

        /** How often a watched task's tree is tried after that, while it is watched. */
        const val SLOW_RETRY_MILLIS = 2_000L

        /** The least time between two requests to Android to bind the service again. */
        const val RECONNECT_MILLIS = 10_000L

        /** The most nodes one tree holds; the host takes up to 4000. */
        const val MAX_NODES = 2000

        /** About how many bytes one tree holds: a binder call carries at most 1 MiB. */
        const val MAX_TREE_BYTES = 400_000

        /** The most characters one text of a node keeps; the host keeps 2000. */
        const val MAX_TEXT = 2000

        /** Past this many IDs given out in one epoch, the next tree starts a new one. */
        const val MAX_IDS = 4 * MAX_NODES

        /** The node as the host takes it: bounds relative to the task's window, text capped. */
        fun toNode(id: Int, children: List<Int>, info: NodeInfo, task: TaskBounds): AccessibilityNode {
            val builder = AccessibilityNode.newBuilder()
                .setId(id)
                .addAllChildIds(children)
                .setClassName(info.className.take(MAX_TEXT))
                .setText(info.text.take(MAX_TEXT))
                .setContentDescription(info.description.take(MAX_TEXT))
                .setHint(info.hint.take(MAX_TEXT))
                .setViewId(info.viewId.take(MAX_TEXT))
                .setStateDescription(info.stateDescription.take(MAX_TEXT))
                .setBounds(
                    Rect.newBuilder()
                        .setLeft(info.bounds.left - task.left)
                        .setTop(info.bounds.top - task.top)
                        .setRight(info.bounds.right - task.left)
                        .setBottom(info.bounds.bottom - task.top),
                )
                .setCheckable(info.checkable)
                .setChecked(info.checked)
                .setSelected(info.selected)
                .setEnabled(info.enabled)
                .setFocusable(info.focusable)
                .setFocused(info.focused)
                .setScrollable(info.scrollable)
                .setClickable(info.clickable)
                .setLongClickable(info.longClickable)
                .setEditable(info.editable)
                .setPassword(info.password)
                .setHeading(info.heading)
                .addAllActions(info.actions.distinct().sortedBy { it.number })
            info.range?.let { builder.setRange(NodeRange.newBuilder().setMin(it.min).setMax(it.max).setCurrent(it.current)) }
            // A password's characters never leave the guest; only how many there are.
            if (info.password) builder.setText("•".repeat(info.text.length.coerceAtMost(MAX_TEXT)))
            return builder.build()
        }

        /** The enabled accessibility services, as Settings keeps them, with [component] added. */
        fun withService(enabled: String?, component: String): String {
            val services = split(enabled)
            return if (services.any { it.equals(component, ignoreCase = true) }) services.joinToString(":") else (services + component).joinToString(":")
        }

        /** The enabled accessibility services without [component]; others stay as they were. */
        fun withoutService(enabled: String?, component: String): String =
            split(enabled).filterNot { it.equals(component, ignoreCase = true) }.joinToString(":")

        private fun split(enabled: String?): List<String> = enabled.orEmpty().split(':').map { it.trim() }.filter { it.isNotEmpty() }
    }
}

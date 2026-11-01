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

    companion object {
        val NONE = object : AccessibilitySource {
            override fun enable(onChange: () -> Unit) = Unit

            override fun disable() = Unit

            override fun roots(taskId: Int): List<NodeHandle>? = null
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
 * Every method runs on the agent's scheduler thread.
 */
class AccessibilityExport(
    private val source: AccessibilitySource,
    private val scheduler: Scheduler,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val taskBounds: (Int) -> TaskBounds?,
    private val clock: () -> Long = { System.nanoTime() / 1_000_000 },
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
    }

    private val watched = HashMap<Int, TaskState>()
    private var enabled = false
    private var epochs = 0

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

    /** The content of some window may have changed. Called on the scheduler thread. */
    fun changed() {
        for ((taskId, state) in watched) schedule(taskId, state, BATCH_MILLIS)
    }

    private fun watch(taskId: Int) {
        val state = watched.getOrPut(taskId) { TaskState() }
        state.needFull = true
        state.retries = 0
        if (!enabled) {
            enabled = true
            log.info("a host accessibility client watches task $taskId; the accessibility service is on")
            try {
                source.enable { scheduler.post(::changed) }
            } catch (e: Exception) {
                log.warn("could not turn the accessibility service on", e)
            }
        }
        schedule(taskId, state, 0)
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
        val delay = maxOf(wait, state.lastSentAt + MIN_INTERVAL_MILLIS - clock(), 0L)
        scheduler.postDelayed(delay) {
            state.pending = false
            if (watched[taskId] === state) read(taskId, state)
        }
    }

    private fun read(taskId: Int, state: TaskState) {
        val bounds = taskBounds(taskId)
        val roots = try {
            source.roots(taskId)
        } catch (e: Exception) {
            log.warn("reading the accessibility nodes of task $taskId failed", e)
            null
        }
        if (roots == null || bounds == null) {
            // The service is still starting, or the task is not placed yet.
            if (state.retries++ < MAX_RETRIES) schedule(taskId, state, RETRY_MILLIS)
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
        val snapshot = collect(state, roots, bounds)
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

    /** Reads the task's nodes breadth first, so a cut tree keeps the nodes nearest the top. */
    private fun collect(state: TaskState, roots: List<NodeHandle>, task: TaskBounds): Snapshot {
        val nodes = LinkedHashMap<Int, AccessibilityNode>()
        val handles = HashMap<Int, NodeHandle>()
        var bytes = 0
        var truncated = false
        var focus = 0
        val idOf = { handle: NodeHandle -> state.ids.getOrPut(handle.key) { state.nextId++ } }
        val queue = ArrayDeque<Pair<Int, Pair<NodeHandle, NodeInfo>>>()
        val rootIds = mutableListOf<Int>()
        val reserved = HashSet<Int>()
        for (root in roots) {
            val info = root.info()
            if (!info.visible) continue
            val id = idOf(root)
            if (!reserved.add(id)) continue
            if (reserved.size > MAX_NODES) {
                truncated = true
                break
            }
            rootIds += id
            queue.addLast(id to (root to info))
        }
        while (queue.isNotEmpty()) {
            val (id, pair) = queue.removeFirst()
            val (handle, info) = pair
            val childIds = mutableListOf<Int>()
            val children = try {
                handle.children()
            } catch (e: Exception) {
                emptyList()
            }
            for (child in children) {
                if (reserved.size >= MAX_NODES || bytes >= MAX_TREE_BYTES) {
                    truncated = true
                    break
                }
                val childInfo = try {
                    child.info()
                } catch (e: Exception) {
                    continue
                }
                if (!childInfo.visible) continue
                val childId = idOf(child)
                if (!reserved.add(childId)) continue
                childIds += childId
                queue.addLast(childId to (child to childInfo))
            }
            val node = toNode(id, childIds, info, task)
            bytes += node.serializedSize
            nodes[id] = node
            handles[id] = handle
            if (info.focused && focus == 0) focus = id
        }
        return Snapshot(rootIds, nodes, handles, focus, truncated)
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

        /** How long to wait for a service that is still starting, or a task not yet placed. */
        const val RETRY_MILLIS = 200L

        /** How many times to wait for it, two seconds in all. */
        const val MAX_RETRIES = 10

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

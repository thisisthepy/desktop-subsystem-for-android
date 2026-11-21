package io.github.thisisthepy.dsa.agent.accessibility

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.AccessibilityAction
import io.github.thisisthepy.dsa.control.v1.AccessibilityActionResult
import io.github.thisisthepy.dsa.control.v1.AccessibilityAnnouncement
import io.github.thisisthepy.dsa.control.v1.AccessibilityNode
import io.github.thisisthepy.dsa.control.v1.AccessibilityTree
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.NodeAction
import io.github.thisisthepy.dsa.control.v1.NodeCollection
import io.github.thisisthepy.dsa.control.v1.NodeCollectionItem
import io.github.thisisthepy.dsa.control.v1.NodeRange
import io.github.thisisthepy.dsa.control.v1.NodeWindow
import io.github.thisisthepy.dsa.control.v1.Rect

/** A node's range, for a slider or a progress bar. */
data class RangeInfo(val min: Float, val max: Float, val current: Float)

/** A list's or a grid's size, from Android's CollectionInfo. */
data class CollectionInfo(val rows: Int, val columns: Int, val hierarchical: Boolean = false)

/** Where an item is in its list or grid, from Android's CollectionItemInfo. */
data class CollectionItemInfo(val row: Int, val rowSpan: Int, val column: Int, val columnSpan: Int, val heading: Boolean = false)

/** An action the app defines for a node, beyond the standard ones (a mail's "Archive"). */
data class CustomAction(val id: Int, val label: String)

/** A link inside a node's text: a URLSpan or a ClickableSpan, by its place among the spans. */
data class TextLink(val text: String, val url: String)

/** The Android window a root node belongs to. */
data class WindowInfo(val title: String, val modal: Boolean, val popup: Boolean)

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
    val accessibilityFocused: Boolean = false,
    /** The selection of an editable node's text, in UTF-16 code units; -1 for none. */
    val selectionStart: Int = -1,
    val selectionEnd: Int = -1,
    /** A heading's level, 1 to 9, or 0 when the app does not say. */
    val headingLevel: Int = 0,
    val link: Boolean = false,
    val url: String = "",
    val collection: CollectionInfo? = null,
    val collectionItem: CollectionItemInfo? = null,
    /** 0 not announced, 1 politely, 2 at once, as Android's live region modes. */
    val liveRegion: Int = 0,
    val paneTitle: String = "",
    val customActions: List<CustomAction> = emptyList(),
    val links: List<TextLink> = emptyList(),
    /** Set for a window's root: the window it is the root of. */
    val window: WindowInfo? = null,
)

/** What an action needs besides its kind. */
data class ActionArguments(
    val text: String = "",
    val selectionStart: Int = 0,
    val selectionEnd: Int = 0,
    val progress: Float = 0f,
    val row: Int = 0,
    val column: Int = 0,
)

/** One node of a window, as the guest's accessibility framework hands it out. */
interface NodeHandle {
    /** Equal for the same node read twice; the node's ID within an epoch follows it. */
    val key: Any

    fun info(): NodeInfo

    fun children(): List<NodeHandle>

    /** Does [action] on the node; [text] is the new text of a set-text action. */
    fun perform(action: NodeAction, text: String): Boolean

    /** Does [action] with all it needs. */
    fun perform(action: NodeAction, arguments: ActionArguments): Boolean = perform(action, arguments.text)

    /** Does the app's own action [id] on the node. */
    fun performCustom(id: Int): Boolean = false

    /** Follows the link at [index] among the node's links. */
    fun followLink(index: Int): Boolean = false
}

/** What one accessibility event says, as the agent needs it. */
data class AccessibilityChange(
    val kind: Kind,
    /** The task the event's window belongs to; null when it says no window. */
    val taskId: Int? = null,
    /** The node the event is about, when it names one. */
    val source: NodeHandle? = null,
    /** What an announcement or a toast says. */
    val text: String = "",
    /** The app the event came from. */
    val packageName: String = "",
    /**
     * Finds the task and the node the event is about, when finding them asks Android and so
     * waits on the app that owns the window. The export calls it on its reader thread, never on
     * the thread that delivered the event: that is the agent's UI thread, and an app that does
     * not answer (one that is closing, say) holds the asker for seconds. Null when the event
     * says it all already; the change it gives is null when it turned out to be about nothing
     * the export follows.
     */
    val resolve: (() -> AccessibilityChange?)? = null,
) {
    enum class Kind {
        /** The input focus, the accessibility focus or the selection moved. */
        FOCUS,

        /** A window's content changed. */
        CONTENT,

        /** An app asked a screen reader to say something. */
        ANNOUNCEMENT,

        /** An app showed a toast. */
        TOAST,
    }

    companion object {
        /** A change in some window, which says nothing more. */
        val ANY = AccessibilityChange(Kind.CONTENT)
    }
}

/** The guest's accessibility service, which the export turns on only while it is needed. */
interface AccessibilitySource {
    /**
     * Turns the service on. [onChange] is called, from any thread, whenever the content of a
     * window may have changed.
     */
    fun enable(onChange: () -> Unit)

    /**
     * Turns the service on, telling [onEvent] what each event says. A source that cannot
     * tell says that something changed.
     */
    fun enableEvents(onEvent: (AccessibilityChange) -> Unit) = enable { onEvent(AccessibilityChange.ANY) }

    /** Turns the service off: it then reads nothing and receives no events. */
    fun disable()

    /** The root node of each window of the task, in order; null while the service is not yet on. */
    fun roots(taskId: Int): List<NodeHandle>?

    /**
     * Whether a screen reader listens for any watched task. While one does, the service takes
     * every event as it happens and the kinds only a screen reader needs (selection, the
     * accessibility focus, announcements); otherwise it keeps its usual set and batching.
     */
    fun setListening(listening: Boolean) = Unit

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
 * The views of the desktop widgets the agent hosts, read as accessibility nodes. A widget's
 * views live in the agent's own process, so they are read directly, with no accessibility
 * service, and only while the host watches the widget.
 */
interface WidgetNodes {
    /**
     * The root node of the widget's views, with bounds in pixels of the widget's image ((0, 0)
     * its top left); null while the widget is not drawn. Called on the export's reader thread.
     */
    fun roots(widgetId: Int): List<NodeHandle>?

    /** The widget's image as bounds from (0, 0); null while the widget is not drawn. */
    fun bounds(widgetId: Int): TaskBounds?

    /**
     * Calls [changed], from any thread, with a widget whose views may have changed, until it is
     * set to null. Null while no widget is watched, so a widget that redraws costs nothing.
     */
    fun listen(changed: ((Int) -> Unit)?)

    companion object {
        val NONE = object : WidgetNodes {
            override fun roots(widgetId: Int): List<NodeHandle>? = null

            override fun bounds(widgetId: Int): TaskBounds? = null

            override fun listen(changed: ((Int) -> Unit)?) = Unit
        }
    }
}

/**
 * A node the agent adds below a real one: a link inside its text, or one of the app's own
 * actions. It is read with its parent, so it costs the app nothing more, and pressing it
 * follows the link or does the action on the parent.
 */
private class AddedNode(private val parent: NodeHandle, private val kind: String, private val index: Int, private val info: NodeInfo) : NodeHandle {
    override val key: Any = Triple(parent.key, kind, index)

    override fun info(): NodeInfo = info

    override fun children(): List<NodeHandle> = emptyList()

    override fun perform(action: NodeAction, text: String): Boolean {
        if (action != NodeAction.NODE_ACTION_CLICK) return false
        return if (kind == LINK) parent.followLink(index) else parent.performCustom(index)
    }

    companion object {
        const val LINK = "link"
        const val ACTION = "action"

        /** The links and actions of a node read as [info], as nodes of their own. */
        fun of(parent: NodeHandle, info: NodeInfo): List<NodeHandle> {
            val links = info.links.mapIndexed { index, link ->
                AddedNode(
                    parent, LINK, index,
                    NodeInfo(
                        className = "android.text.style.URLSpan", text = link.text, bounds = info.bounds,
                        clickable = true, link = true, url = link.url, actions = listOf(NodeAction.NODE_ACTION_CLICK),
                    ),
                )
            }
            val actions = info.customActions.map { action ->
                AddedNode(
                    parent, ACTION, action.id,
                    NodeInfo(
                        className = "android.widget.Button", text = action.label, bounds = info.bounds,
                        clickable = true, actions = listOf(NodeAction.NODE_ACTION_CLICK),
                    ),
                )
            }
            return links + actions
        }
    }
}

/**
 * Whose tree the host watches: a task's window ([widgetId] 0), or a desktop widget's window
 * ([taskId] 0). Its name is how the guest's log names it.
 */
data class TreeTarget(val taskId: Int, val widgetId: Int) {
    val isWidget: Boolean
        get() = widgetId != 0

    override fun toString(): String = if (isWidget) "widget $widgetId" else "task $taskId"

    companion object {
        fun task(taskId: Int) = TreeTarget(taskId, 0)

        fun widget(widgetId: Int) = TreeTarget(0, widgetId)

        /** The target a message names: its widget when it names one, else its task. */
        fun of(taskId: Int, widgetId: Int) = if (widgetId != 0) widget(widgetId) else task(taskId)
    }
}

/**
 * Exports the accessibility tree of the tasks the host watches, and of the desktop widgets it
 * watches ([WidgetNodes]): a widget's tree is sent and acted on as a task's is, named by its
 * widget ID, and reading it never turns the accessibility service on.
 *
 * While no task is watched, the guest's accessibility service is off: Android then tells no
 * app that accessibility is on, no app builds accessibility events, and the agent reads
 * nothing, so a user who uses no host accessibility client pays nothing. The first
 * AccessibilityWatch turns it on; the last one turned off, or a new connection, turns it off.
 *
 * For a watched task the agent sends a full tree, then only what changed. An event names the
 * window it came from, and only the task of that window is read again. A change starts a
 * short wait of [BATCH_MILLIS], so the burst of events one screen change makes is read once,
 * and two trees of a task are never sent less than [MIN_INTERVAL_MILLIS] apart: at most four
 * a second, while a screen reader still hears of a change well within a second.
 *
 * While the host says a screen reader listens for the task (AccessibilityListening), focus
 * comes first: a focus, selection or accessibility focus event sends the node it names at
 * once, as a change of that node alone, whatever waits and however busy the app was. Other
 * changes wait only [FAST_BATCH_MILLIS] and [FAST_MIN_INTERVAL_MILLIS], and the agent reads
 * only the part of the task an event names (its source and what is below it) instead of the
 * whole task, falling back to the whole task when the source is new to it. Without a screen
 * reader nothing of this runs and the waits stay as they were.
 *
 * A tree holds at most [MAX_NODES] nodes and [MAX_TREE_BYTES] bytes, the nearest nodes first,
 * so one frame stays far below what the binder call to the daemon may carry. A node's links
 * and the app's own actions on it are added below it as nodes a client can press.
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
 * Right after its window is resized that is the frame the user waits for, so a task's tree is
 * not read while the host resizes it ([resizing]): a read waits until [RESIZE_QUIET_MILLIS]
 * have passed since the task's last new size or density.
 *
 * Each of those calls waits for the app's UI thread, up to the framework's own timeout of
 * five seconds, and a busy UI thread is one an app may not have to spare. So a read stops as
 * soon as one call takes [SLOW_CALL_MILLIS] or the read has taken [READ_BUDGET_MILLIS]: the
 * app is busy, and the task's tree is read again [BUSY_RETRY_MILLIS] later. What was read by
 * then is sent all the same: the first tree of a task as far as it got, a later one as the
 * changes of the nodes it reached, leaving the nodes it did not reach as they were.
 *
 * A watch that ends keeps the task's node IDs for [RETIRED_MILLIS]: a host that reads a tree
 * once for a client and then stops watching, then sends an action on that tree, watches the
 * task again, and the full tree that comes keeps the epoch and the IDs the action names.
 *
 * A watched task gets its tree however long Android takes to bind the service. The export
 * tries every [RETRY_MILLIS] for [MAX_RETRIES] tries, then every [SLOW_RETRY_MILLIS] for as long
 * as the task is watched. When the service has not come up by then, the agent says why it can
 * see and asks Android to bind it again, at most every [RECONNECT_MILLIS].
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
    /** The desktop widgets' views, whose trees the host may watch too. */
    private val widgets: WidgetNodes = WidgetNodes.NONE,
    /** The package of a task's app, to tell which task a toast belongs to. */
    private val taskPackage: (Int) -> String? = { null },
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
        var sentAccessibilityFocus = 0
        var handles: Map<Int, NodeHandle> = emptyMap()
        var retries = 0

        /** Whether a screen reader listens for the task's events. */
        var listening = false

        /** The nodes events named since the last read, while a screen reader listens. */
        val dirty = LinkedHashMap<Any, NodeHandle>()

        /** Whether the whole task is to be read, not just [dirty]. */
        var readAll = true

        /** Whether the next full tree keeps the epoch and IDs a retired watch had. */
        var keepIds = false

        /** When the watch ended, for a retired state. */
        var retiredAt = 0L

        var reading = false
        var changedWhileReading = false
        var watchedAt = 0L
        var announced = false
        var warned = false
        var quietAt = Long.MIN_VALUE / 2
        var busyWarned = false

        val batch: Long
            get() = if (listening) FAST_BATCH_MILLIS else BATCH_MILLIS

        val interval: Long
            get() = if (listening) FAST_MIN_INTERVAL_MILLIS else MIN_INTERVAL_MILLIS
    }

    private val watched = HashMap<TreeTarget, TaskState>()
    private val retired = LinkedHashMap<TreeTarget, TaskState>()
    private var enabled = false
    private var epochs = 0
    private var reconnectAt = 0L
    private var listeningSaid = false

    /** Whether the accessibility service is on. */
    val active: Boolean
        get() = enabled

    /** The tasks the host watches. */
    val tasks: Set<Int>
        get() = watched.keys.filter { !it.isWidget }.map { it.taskId }.toSet()

    /** The desktop widgets the host watches. */
    val watchedWidgets: Set<Int>
        get() = watched.keys.filter { it.isWidget }.map { it.widgetId }.toSet()

    /** At boot: a service left on by an agent that died is turned off. */
    fun start() {
        try {
            source.disable()
        } catch (e: Exception) {
            log.warn("could not turn the accessibility service off", e)
        }
    }

    /** A new connection: the host says again which tasks and widgets it watches. */
    fun connected() {
        watched.clear()
        retired.clear()
        followWidgets()
        turnOff()
    }

    /** Takes a watch, a listening or an action. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.ACCESSIBILITY_WATCH -> {
                val watch = envelope.accessibilityWatch
                val target = TreeTarget.of(watch.taskId, watch.widgetId)
                if (watch.on) watch(target) else unwatch(target)
            }
            Envelope.BodyCase.ACCESSIBILITY_LISTENING -> listen(envelope.accessibilityListening.taskId, envelope.accessibilityListening.listening)
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
        watched[TreeTarget.task(taskId)]?.quietAt = clock() + RESIZE_QUIET_MILLIS
    }

    /** The content of some window may have changed. Called on the scheduler thread. */
    fun changed() = changed(AccessibilityChange.ANY)

    /**
     * What an event said. Called on the scheduler thread. Events come from the accessibility
     * service, so they are about tasks only; a widget's changes come through [widgetChanged].
     */
    fun changed(change: AccessibilityChange) {
        val taskId = change.taskId
        if (taskId != null && TreeTarget.task(taskId) !in watched) return
        when (change.kind) {
            AccessibilityChange.Kind.ANNOUNCEMENT, AccessibilityChange.Kind.TOAST -> {
                announce(change)
                return
            }
            else -> Unit
        }
        val targets = if (taskId != null) listOf(TreeTarget.task(taskId)) else watched.keys.filter { !it.isWidget }
        for (target in targets) {
            val state = watched[target] ?: continue
            val node = change.source
            if (!state.listening || node == null || taskId == null) {
                state.readAll = true
                schedule(target, state, state.batch)
                continue
            }
            if (change.kind == AccessibilityChange.Kind.FOCUS && focusUpdate(target, state, node)) continue
            state.dirty[node.key] = node
            schedule(target, state, if (change.kind == AccessibilityChange.Kind.FOCUS) 0 else state.batch)
        }
    }

    /**
     * An event as the service delivers it, on whatever thread Android calls the service on (the
     * agent's UI thread). Nothing here waits: what needs asking Android is asked on [reader],
     * and the rest goes to the scheduler thread.
     */
    private fun delivered(change: AccessibilityChange) {
        val resolve = change.resolve ?: return scheduler.post { changed(change) }
        reader.post {
            val resolved = try {
                resolve()
            } catch (e: Exception) {
                log.warn("could not read what an accessibility event was about", e)
                AccessibilityChange.ANY
            }
            if (resolved != null) scheduler.post { changed(resolved) }
        }
    }

    /** The views of widget [widgetId] may have changed. Called on the scheduler thread. */
    fun widgetChanged(widgetId: Int) {
        val target = TreeTarget.widget(widgetId)
        watched[target]?.let {
            it.readAll = true
            schedule(target, it, it.batch)
        }
    }

    /** Listens to the widgets' changes while a widget is watched, and not otherwise. */
    private fun followWidgets() {
        val any = watched.keys.any { it.isWidget }
        val changed: (Int) -> Unit = { widgetId -> scheduler.post { widgetChanged(widgetId) } }
        try {
            widgets.listen(if (any) changed else null)
        } catch (e: Exception) {
            log.warn("could not follow the widgets' changes", e)
        }
    }

    private fun watch(target: TreeTarget) {
        pruneRetired()
        val state = watched.getOrPut(target) {
            retired.remove(target)?.also {
                it.keepIds = true
                it.pending = false
                // A watch that starts again is a new ask: its first tree is not held back by
                // the least interval since the tree the retired watch sent last.
                it.lastSentAt = Long.MIN_VALUE / 2
            } ?: TaskState()
        }
        state.needFull = true
        state.readAll = true
        state.retries = 0
        state.watchedAt = clock()
        state.announced = false
        state.warned = false
        state.listening = false
        if (target.isWidget) {
            // A widget's views are the agent's own: no service is needed to read them.
            log.info("a host accessibility client watches $target; reading its views")
            followWidgets()
        } else if (!enabled) {
            enabled = true
            reconnectAt = clock() + RECONNECT_MILLIS
            log.info("a host accessibility client watches $target; the accessibility service is on")
            try {
                source.enableEvents(::delivered)
            } catch (e: Exception) {
                log.warn("could not turn the accessibility service on", e)
            }
        }
        sayListening()
        schedule(target, state, 0)
    }

    private fun listen(taskId: Int, listening: Boolean) {
        val state = watched[TreeTarget.task(taskId)] ?: return
        if (state.listening == listening) return
        state.listening = listening
        log.info(
            if (listening) "a screen reader listens for task $taskId; its focus goes at once"
            else "no screen reader listens for task $taskId; its tree keeps the usual waits",
        )
        sayListening()
    }

    /** Tells the service whether any watched task has a screen reader listening. */
    private fun sayListening() {
        val listening = enabled && watched.values.any { it.listening }
        if (listening == listeningSaid) return
        listeningSaid = listening
        try {
            source.setListening(listening)
        } catch (e: Exception) {
            log.warn("could not change which accessibility events the service takes", e)
        }
    }

    /**
     * The tree could not be read yet: the service is still coming up, or the task is not
     * placed yet (a widget not drawn yet). Tries again soon a few times, then slowly for as
     * long as it is watched.
     */
    private fun retry(target: TreeTarget, state: TaskState, why: String) {
        if (state.retries++ < MAX_RETRIES) {
            schedule(target, state, RETRY_MILLIS)
            return
        }
        if (!state.warned) {
            state.warned = true
            val about = if (target.isWidget) "" else " (${describeSource()})"
            log.warn(
                "$target is watched, but its accessibility tree could not be read " +
                    "${clock() - state.watchedAt} ms after the host asked: $why$about; " +
                    "trying again every ${SLOW_RETRY_MILLIS / 1000} s",
            )
        }
        if (target.isWidget) {
            schedule(target, state, SLOW_RETRY_MILLIS)
            return
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
        schedule(target, state, SLOW_RETRY_MILLIS)
    }

    private fun describeSource(): String = try {
        source.describe()
    } catch (e: Exception) {
        "the service's state could not be read: ${e.message}"
    }

    private fun unwatch(target: TreeTarget) {
        val state = watched.remove(target) ?: return
        // Kept a while, without its node handles, so an action on its last tree still finds
        // its node when the host watches the task (or widget) again for it.
        if (state.epoch != 0) {
            state.handles = emptyMap()
            state.dirty.clear()
            state.retiredAt = clock()
            retired.remove(target)
            retired[target] = state
            while (retired.size > MAX_RETIRED) retired.remove(retired.keys.first())
        }
        if (target.isWidget) {
            log.info("no host accessibility client watches $target; its views are not read")
            followWidgets()
        }
        // The service stays on while a task is watched, and only then.
        if (watched.keys.none { !it.isWidget }) turnOff() else sayListening()
    }

    private fun pruneRetired() {
        val now = clock()
        retired.entries.removeAll { now - it.value.retiredAt > RETIRED_MILLIS }
    }

    private fun turnOff() {
        if (!enabled) return
        enabled = false
        listeningSaid = false
        log.info("no host accessibility client watches a task; the accessibility service is off")
        try {
            source.disable()
        } catch (e: Exception) {
            log.warn("could not turn the accessibility service off", e)
        }
    }

    private fun schedule(target: TreeTarget, state: TaskState, wait: Long) {
        if (state.pending) return
        state.pending = true
        val now = clock()
        // A focus change a screen reader waits for skips the least interval between trees.
        val interval = if (wait == 0L && state.listening) 0L else state.lastSentAt + state.interval - now
        val delay = maxOf(wait, interval, state.quietAt - now, 0L)
        scheduler.postDelayed(delay) {
            state.pending = false
            if (watched[target] === state) read(target, state)
        }
    }

    /** Where the target's nodes are measured from: its task's place, or its widget's image. */
    private fun boundsOf(target: TreeTarget): TaskBounds? = try {
        if (target.isWidget) widgets.bounds(target.widgetId) else taskBounds(target.taskId)
    } catch (e: Exception) {
        log.warn("reading where $target is failed", e)
        null
    }

    /** Why the target cannot be read when it has no bounds. */
    private fun unplaced(target: TreeTarget) =
        if (target.isWidget) "the widget is not drawn yet" else "the task is not placed yet"

    /**
     * Sends a focus event's node at once, as a change of it and of the node that had the focus
     * before. Its fields come with the event, so this asks the app nothing. False when the
     * node is new to the tree, which is then read.
     */
    private fun focusUpdate(target: TreeTarget, state: TaskState, handle: NodeHandle): Boolean {
        if (state.needFull || state.sent.isEmpty()) return false
        val id = state.ids[handle.key] ?: return false
        val previous = state.sent[id] ?: return false
        val bounds = boundsOf(target) ?: return false
        val info = try {
            handle.info()
        } catch (e: Exception) {
            return false
        }
        val node = toNode(id, previous.childIdsList, info, bounds)
        val changed = LinkedHashMap<Int, AccessibilityNode>()
        if (node != previous) changed[id] = node
        var focus = state.sentFocus
        var accessibilityFocus = state.sentAccessibilityFocus
        if (info.focused && focus != id) {
            state.sent[focus]?.let { changed[focus] = it.toBuilder().setFocused(false).build() }
            focus = id
        } else if (!info.focused && focus == id) {
            focus = 0
        }
        if (info.accessibilityFocused && accessibilityFocus != id) {
            state.sent[accessibilityFocus]?.let {
                changed[accessibilityFocus] = (changed[accessibilityFocus] ?: it).toBuilder().setAccessibilityFocused(false).build()
            }
            accessibilityFocus = id
        } else if (!info.accessibilityFocused && accessibilityFocus == id) {
            accessibilityFocus = 0
        }
        if (changed.isEmpty() && focus == state.sentFocus && accessibilityFocus == state.sentAccessibilityFocus) return true
        val tree = AccessibilityTree.newBuilder()
            .setTaskId(target.taskId)
            .setWidgetId(target.widgetId)
            .setEpoch(state.epoch)
            .setFull(false)
            .addAllRootIds(state.sentRoots)
            .addAllNodes(changed.values)
            .setFocusedId(focus)
            .setAccessibilityFocusedId(accessibilityFocus)
            .build()
        if (!send(Envelope.newBuilder().setAccessibilityTree(tree).build().toByteArray())) {
            state.needFull = true
            return true
        }
        state.sent = state.sent + changed
        state.handles = state.handles + (id to handle)
        state.sentFocus = focus
        state.sentAccessibilityFocus = accessibilityFocus
        return true
    }

    private fun announce(change: AccessibilityChange) {
        if (change.text.isBlank()) return
        val listening = watched.filter { (target, state) -> state.listening && !target.isWidget }.keys.map { it.taskId }
        if (listening.isEmpty()) return
        // A toast is shown by the system, not in the app's window: it goes to the app's task.
        val targets = when {
            change.taskId != null -> listOfNotNull(change.taskId.takeIf { it in listening })
            change.packageName.isNotEmpty() -> listening.filter { taskPackage(it) == change.packageName }.ifEmpty { listening.take(1) }
            else -> listening.take(1)
        }
        for (taskId in targets) {
            val announcement = AccessibilityAnnouncement.newBuilder()
                .setTaskId(taskId)
                .setText(change.text.take(MAX_TEXT))
                .setAssertive(false)
                .setToast(change.kind == AccessibilityChange.Kind.TOAST)
                .build()
            send(Envelope.newBuilder().setAccessibilityAnnouncement(announcement).build().toByteArray())
        }
    }

    /** Reads the target's nodes on [reader], then sends what changed from the scheduler's thread. */
    private fun read(target: TreeTarget, state: TaskState) {
        if (state.reading) {
            state.changedWhileReading = true
            return
        }
        if (clock() < state.quietAt) {
            schedule(target, state, 0)
            return
        }
        val bounds = boundsOf(target)
        if (bounds == null) {
            retry(target, state, unplaced(target))
            return
        }
        // Only the nodes events named, when all of them are in the tree already.
        val starts = if (state.readAll || state.needFull || state.ids.size > MAX_IDS) {
            null
        } else {
            state.dirty.values.filter { it.key in state.ids }.takeIf { it.size == state.dirty.size && it.isNotEmpty() }
        }
        state.dirty.clear()
        state.readAll = false
        state.reading = true
        state.changedWhileReading = false
        reader.post {
            val read = try {
                if (starts != null) {
                    readNodes(starts, bounds, subtree = true)
                } else {
                    val roots = if (target.isWidget) widgets.roots(target.widgetId) else source.roots(target.taskId)
                    roots?.let { readNodes(it, bounds, subtree = false) }
                }
            } catch (e: Exception) {
                log.warn("reading the accessibility nodes of $target failed", e)
                null
            }
            scheduler.post {
                state.reading = false
                if (watched[target] !== state) return@post
                if (read?.busy != null) {
                    if (!state.busyWarned) {
                        state.busyWarned = true
                        log.info("$target's app is busy (${read.busy}); what was read is sent, and its accessibility tree is read again in $BUSY_RETRY_MILLIS ms")
                    }
                    if (read.nodes.isNotEmpty()) sendRead(target, state, read)
                    state.changedWhileReading = false
                    state.readAll = true
                    schedule(target, state, BUSY_RETRY_MILLIS)
                    return@post
                }
                state.busyWarned = false
                sendRead(target, state, read)
                if (state.changedWhileReading) {
                    state.changedWhileReading = false
                    state.readAll = state.readAll || !state.listening
                    schedule(target, state, state.batch)
                }
            }
        }
    }

    private fun sendRead(target: TreeTarget, state: TaskState, read: ReadNodes?) {
        val bounds = boundsOf(target)
        if (read == null) {
            val why = if (target.isWidget) "the widget's views could not be read" else "the accessibility service gave no windows"
            retry(target, state, why)
            return
        }
        if (bounds == null) {
            retry(target, state, unplaced(target))
            return
        }
        state.retries = 0
        val full = state.needFull || state.ids.size > MAX_IDS
        if (full) {
            if (state.keepIds && state.ids.size <= MAX_IDS) {
                state.keepIds = false
            } else {
                state.ids.clear()
                state.nextId = 1
                state.epoch = ++epochs
            }
            state.sent = emptyMap()
            state.sentRoots = emptyList()
            state.sentFocus = 0
            state.sentAccessibilityFocus = 0
        }
        val snapshot = snapshot(state, read, bounds)
        // A whole read replaces what was sent; a part (a subtree, or a read the app cut short)
        // changes only the nodes it reached and removes only what went from below a node it
        // read whole.
        val whole = !read.subtree && read.busy == null
        val removed = if (whole) state.sent.keys.filter { it !in snapshot.nodes } else gone(state.sent, snapshot)
        val nodes = LinkedHashMap<Int, AccessibilityNode>()
        for ((id, node) in snapshot.nodes) {
            // A node whose children were not read keeps the ones it had.
            nodes[id] = if (id in snapshot.complete) node else state.sent[id]?.let { old -> node.toBuilder().clearChildIds().addAllChildIds(old.childIdsList.filter { it !in removed }).build() } ?: node
        }
        val next = if (whole) nodes else (state.sent - removed.toSet()) + nodes
        val roots = if (read.subtree) state.sentRoots else snapshot.roots
        val focus = snapshot.focus.takeIf { it != 0 } ?: state.sentFocus.takeIf { it in next && it !in nodes } ?: 0
        val accessibilityFocus = snapshot.accessibilityFocus.takeIf { it != 0 }
            ?: state.sentAccessibilityFocus.takeIf { it in next && it !in nodes } ?: 0
        val changed = nodes.values.filter { state.sent[it.id] != it }
        if (!full && changed.isEmpty() && removed.isEmpty() && roots == state.sentRoots && focus == state.sentFocus && accessibilityFocus == state.sentAccessibilityFocus) {
            state.handles = state.handles + snapshot.handles
            return
        }
        val tree = AccessibilityTree.newBuilder()
            .setTaskId(target.taskId)
            .setWidgetId(target.widgetId)
            .setEpoch(state.epoch)
            .setFull(full)
            .addAllRootIds(roots)
            .addAllNodes(if (full) next.values else changed)
            .addAllRemovedIds(if (full) emptyList() else removed)
            .setFocusedId(focus)
            .setAccessibilityFocusedId(accessibilityFocus)
            .build()
        if (!send(Envelope.newBuilder().setAccessibilityTree(tree).build().toByteArray())) {
            log.warn("the host did not take the accessibility tree of $target; sending it whole next time")
            state.needFull = true
            return
        }
        state.needFull = false
        if (!state.announced) {
            state.announced = true
            log.info("sent $target's accessibility tree to the host: ${next.size} nodes, ${clock() - state.watchedAt} ms after it asked")
        }
        state.lastSentAt = clock()
        state.sent = next
        state.sentRoots = roots
        state.sentFocus = focus
        state.sentAccessibilityFocus = accessibilityFocus
        state.handles = (if (whole) emptyMap() else state.handles - removed.toSet()) + snapshot.handles
        if (snapshot.truncated) log.warn("$target has more accessibility nodes than one tree holds; the farthest were left out")
    }

    /**
     * The nodes that were sent and are gone: those below a node [snapshot] read whole (its
     * children all read), at any depth, that the read did not find. Nodes below one the read
     * did not reach are left as they were.
     */
    private fun gone(sent: Map<Int, AccessibilityNode>, snapshot: Snapshot): List<Int> {
        val parent = HashMap<Int, Int>()
        for (node in sent.values) for (child in node.childIdsList) parent[child] = node.id
        return sent.keys.filter { id ->
            if (id in snapshot.nodes) return@filter false
            var at = parent[id]
            var steps = 0
            while (at != null && at !in snapshot.nodes && steps++ < MAX_IDS) at = parent[at]
            // A top-level node that went is gone when the read had the top-level nodes.
            if (at == null) !snapshot.subtree else at in snapshot.complete
        }
    }

    private class Snapshot(
        val roots: List<Int>,
        val nodes: Map<Int, AccessibilityNode>,
        val handles: Map<Int, NodeHandle>,
        val focus: Int,
        val accessibilityFocus: Int,
        val truncated: Boolean,
        /** The nodes whose children were read. */
        val complete: Set<Int>,
        val subtree: Boolean,
    )

    private class ReadNode(val handle: NodeHandle, val info: NodeInfo, val children: MutableList<Int> = mutableListOf())

    /**
     * The nodes of a task's windows (or of the subtrees a read started from), breadth first,
     * the places of the roots among them, and how many of them had their children read.
     * [busy] says why the read stopped early, when the app was too busy to read.
     */
    private class ReadNodes(
        val nodes: List<ReadNode>,
        val roots: List<Int>,
        val truncated: Boolean,
        val busy: String? = null,
        val processed: Int = nodes.size,
        val subtree: Boolean = false,
    )

    private class Busy(val why: String) : Exception(why)

    private inner class Asker {
        private val started = clock()

        fun <T> ask(call: () -> T): T {
            val before = clock()
            val result = call()
            val after = clock()
            if (after - before >= SLOW_CALL_MILLIS) throw Busy("one of its nodes took ${after - before} ms to read")
            if (after - started >= READ_BUDGET_MILLIS) throw Busy("its nodes took ${after - started} ms to read")
            return result
        }
    }

    /**
     * Reads nodes breadth first from [starts] (the task's window roots, or for a [subtree] read
     * the nodes events named), so a cut read keeps the nodes nearest the top. Runs on [reader]:
     * it only asks the nodes, and touches none of the export's state. When the app is busy it
     * stops, and says how far it got.
     */
    private fun readNodes(starts: List<NodeHandle>, task: TaskBounds, subtree: Boolean): ReadNodes {
        val asker = Asker()
        val nodes = ArrayList<ReadNode>()
        val seen = HashSet<Any>()
        val rootPlaces = mutableListOf<Int>()
        var bytes = 0
        var truncated = false
        var next = 0
        try {
            for (root in starts) {
                val info = asker.ask { root.info() }
                if (!info.visible && !subtree) continue
                if (!seen.add(root.key)) continue
                if (seen.size > MAX_NODES) {
                    truncated = true
                    break
                }
                rootPlaces += nodes.size
                nodes += ReadNode(root, info)
            }
            while (next < nodes.size) {
                val place = next
                val node = nodes[place]
                val real = try {
                    asker.ask { node.handle.children() }
                } catch (e: Busy) {
                    throw e
                } catch (e: Exception) {
                    emptyList()
                }
                val children = real + AddedNode.of(node.handle, node.info)
                for (child in children) {
                    if (seen.size >= MAX_NODES || bytes >= MAX_TREE_BYTES) {
                        truncated = true
                        break
                    }
                    val childInfo = try {
                        if (child is AddedNode) child.info() else asker.ask { child.info() }
                    } catch (e: Busy) {
                        throw e
                    } catch (e: Exception) {
                        continue
                    }
                    if (!childInfo.visible) continue
                    if (!seen.add(child.key)) continue
                    node.children += nodes.size
                    nodes += ReadNode(child, childInfo)
                }
                next++
                bytes += toNode(place + 1, node.children.map { it + 1 }, node.info, task).serializedSize
            }
        } catch (e: Busy) {
            // The node being read when the app showed it was busy has only some children: its
            // children list counts as not read.
            return ReadNodes(nodes, rootPlaces, truncated = false, busy = e.why, processed = next, subtree = subtree)
        }
        return ReadNodes(nodes, rootPlaces, truncated, subtree = subtree)
    }

    /** What was read, with the task's IDs for its nodes: a node read before keeps its ID. */
    private fun snapshot(state: TaskState, read: ReadNodes, task: TaskBounds): Snapshot {
        val idOf = { handle: NodeHandle -> state.ids.getOrPut(handle.key) { state.nextId++ } }
        val ids = read.nodes.map { idOf(it.handle) }
        val nodes = LinkedHashMap<Int, AccessibilityNode>()
        val handles = HashMap<Int, NodeHandle>()
        val complete = HashSet<Int>()
        var focus = 0
        var accessibilityFocus = 0
        for ((place, node) in read.nodes.withIndex()) {
            val id = ids[place]
            nodes[id] = toNode(id, node.children.map { ids[it] }, node.info, task)
            handles[id] = node.handle
            if (place < read.processed) complete += id
            if (node.info.focused && focus == 0) focus = id
            if (node.info.accessibilityFocused && accessibilityFocus == 0) accessibilityFocus = id
        }
        return Snapshot(read.roots.map { ids[it] }, nodes, handles, focus, accessibilityFocus, read.truncated, complete, read.subtree)
    }

    private fun perform(action: AccessibilityAction) {
        val target = TreeTarget.of(action.taskId, action.widgetId)
        val state = watched[target]
        val error = when {
            state == null -> "nobody watches $target"
            action.epoch != state.epoch -> "the screen changed; the node is no longer there"
            action.action == NodeAction.NODE_ACTION_UNSPECIFIED || action.action == NodeAction.UNRECOGNIZED -> "the action is not one the agent knows"
            else -> {
                val handle = state.handles[action.nodeId]
                when {
                    handle == null -> "the node is no longer there"
                    else -> try {
                        val arguments = ActionArguments(
                            text = action.text,
                            selectionStart = action.selectionStart,
                            selectionEnd = action.selectionEnd,
                            progress = action.progress,
                            row = action.row,
                            column = action.column,
                        )
                        if (handle.perform(action.action, arguments)) null else "the app did not do it"
                    } catch (e: Exception) {
                        log.warn("an accessibility action failed", e)
                        "the app did not do it: ${e.message}"
                    }
                }
            }
        }
        val result = AccessibilityActionResult.newBuilder()
            .setTaskId(action.taskId)
            .setWidgetId(action.widgetId)
            .setRequestId(action.requestId)
            .setDone(error == null)
            .setError(error ?: "")
            .build()
        send(Envelope.newBuilder().setAccessibilityActionResult(result).build().toByteArray())
        if (state != null) {
            state.readAll = true
            schedule(target, state, state.batch)
        }
    }

    companion object {
        /** How long a change waits for the rest of its burst. */
        const val BATCH_MILLIS = 100L

        /** The least time between two trees of a task: at most four a second. */
        const val MIN_INTERVAL_MILLIS = 250L

        /** How long a change waits while a screen reader listens. */
        const val FAST_BATCH_MILLIS = 25L

        /** The least time between two trees of a task while a screen reader listens. */
        const val FAST_MIN_INTERVAL_MILLIS = 50L

        const val RESIZE_QUIET_MILLIS = 500L

        const val SLOW_CALL_MILLIS = 250L

        const val READ_BUDGET_MILLIS = 1_000L

        const val BUSY_RETRY_MILLIS = 2_000L

        const val RETRY_MILLIS = 200L

        const val MAX_RETRIES = 10

        const val SLOW_RETRY_MILLIS = 2_000L

        const val RECONNECT_MILLIS = 10_000L

        /** How long a watch that ended keeps its node IDs for the next watch of the task. */
        const val RETIRED_MILLIS = 60_000L

        /** The most tasks whose ended watch keeps its IDs. */
        const val MAX_RETIRED = 4

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
                .setAccessibilityFocused(info.accessibilityFocused)
                .setHeadingLevel(info.headingLevel.coerceIn(0, 9))
                .setLink(info.link)
                .setUrl(info.url.take(MAX_TEXT))
                .setLiveRegion(info.liveRegion.coerceIn(0, 2))
                .setPaneTitle(info.paneTitle.take(MAX_TEXT))
            info.range?.let { builder.setRange(NodeRange.newBuilder().setMin(it.min).setMax(it.max).setCurrent(it.current)) }
            if (info.editable && info.selectionStart >= 0 && info.selectionEnd >= 0) {
                builder.setHasTextSelection(true).setTextSelectionStart(info.selectionStart).setTextSelectionEnd(info.selectionEnd)
            }
            info.collection?.let { builder.setCollection(NodeCollection.newBuilder().setRows(it.rows).setColumns(it.columns).setHierarchical(it.hierarchical)) }
            info.collectionItem?.let {
                builder.setCollectionItem(
                    NodeCollectionItem.newBuilder().setRow(it.row).setRowSpan(it.rowSpan).setColumn(it.column).setColumnSpan(it.columnSpan).setHeading(it.heading),
                )
            }
            info.window?.let { builder.setWindow(NodeWindow.newBuilder().setTitle(it.title.take(MAX_TEXT)).setModal(it.modal).setPopup(it.popup)) }
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

package io.github.thisisthepy.dsa.agent

import io.github.thisisthepy.dsa.agent.channel.ChannelEndpoint
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.requests.ResizeOutcome
import io.github.thisisthepy.dsa.agent.input.InjectedKey
import io.github.thisisthepy.dsa.agent.input.InjectedMotion
import io.github.thisisthepy.dsa.agent.input.InputInjector
import io.github.thisisthepy.dsa.agent.requests.SettingsPage
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskKind
import io.github.thisisthepy.dsa.agent.tasks.TaskSnapshot
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.agent.windowing.WindowingControl
import io.github.thisisthepy.dsa.control.v1.Envelope

const val AGENT = "io.github.thisisthepy.dsa.agent"

fun appTask(
    taskId: Int,
    packageName: String = "org.example.app$taskId",
    label: String = "App $taskId",
    bounds: TaskBounds = TaskBounds(10 * taskId, 20, 10 * taskId + 400, 620),
    focused: Boolean = false,
    kind: TaskKind = TaskKind.APP,
    densityDpi: Int = 160,
    /** By default the screen size the framework derives from the bounds and the density. */
    screenDp: ScreenDp? = (bounds.right - bounds.left).let { w ->
        val h = bounds.bottom - bounds.top
        ScreenDp(w * 160 / densityDpi, h * 160 / densityDpi, minOf(w, h) * 160 / densityDpi)
    },
) = TaskSnapshot(taskId, kind, packageName, label, bounds, focused, densityDpi, screenDp)

fun decode(frame: ByteArray): Envelope = Envelope.parseFrom(frame)

/** A single thread in a test: work runs when the test says, delayed work when time passes. */
class FakeScheduler : Scheduler {
    private data class Item(val at: Long, val order: Long, val work: () -> Unit)

    private val queue = mutableListOf<Item>()
    private var order = 0L
    var now = 0L
        private set

    override fun post(work: () -> Unit) {
        queue += Item(now, order++, work)
    }

    override fun postDelayed(delayMillis: Long, work: () -> Unit) {
        queue += Item(now + delayMillis, order++, work)
    }

    /** Ahead of everything waiting, in the order such work was posted. */
    override fun postFirst(work: () -> Unit) {
        queue += Item(Long.MIN_VALUE, order++, work)
    }

    /** Runs everything that is due, including what that work posts. */
    fun runDue() {
        while (true) {
            val next = queue.filter { it.at <= now }.minWithOrNull(compareBy({ it.at }, { it.order })) ?: return
            queue -= next
            next.work()
        }
    }

    fun advance(millis: Long) {
        val end = now + millis
        while (true) {
            runDue()
            val next = queue.filter { it.at <= end }.minOfOrNull { it.at } ?: break
            now = next
        }
        now = end
        runDue()
    }
}

class RecordingLog : Log {
    val lines = mutableListOf<String>()

    override fun info(message: String) {
        lines += "info: $message"
    }

    override fun warn(message: String, error: Throwable?) {
        lines += "warn: $message"
    }
}

/** The control daemon's side of one connection. */
class FakeEndpoint : ChannelEndpoint {
    val sent = mutableListOf<ByteArray>()
    var subscriber: ((ByteArray) -> Unit)? = null
    private var gone: (() -> Unit)? = null
    var dead = false

    override fun send(frame: ByteArray) {
        if (dead) throw IllegalStateException("dead object")
        sent += frame
    }

    override fun subscribe(onFrame: (ByteArray) -> Unit) {
        if (dead) throw IllegalStateException("dead object")
        subscriber = onFrame
    }

    override fun onGone(onGone: () -> Unit) {
        gone = onGone
    }

    /** A frame from the host, or another guest client's task message. */
    fun deliver(frame: ByteArray) {
        subscriber!!.invoke(frame)
    }

    fun die() {
        dead = true
        gone?.invoke()
    }

    /** What the agent sent for the host: all but its word to the daemon that it is there. */
    fun sentMessages(): List<Envelope> = sent.map(::decode).filterNot { it.hasAgentConnected() }

    val connectionsAnnounced: Int get() = sent.map(::decode).count { it.hasAgentConnected() }
}

/** The service manager: the daemon's service is there once [up] is set. */
class FakeLocator : ChannelLocator {
    var up: FakeEndpoint? = null
    var lookups = 0

    override fun find(): ChannelEndpoint? {
        lookups++
        return up
    }
}

class FakeTasks : TaskSource {
    var running = listOf<TaskSnapshot>()
    private var onChange: (() -> Unit)? = null

    override fun snapshot() = running

    override fun listen(onChange: () -> Unit) {
        this.onChange = onChange
    }

    fun change(tasks: List<TaskSnapshot>) {
        running = tasks
        onChange!!.invoke()
    }
}

class FakeLauncherApps : io.github.thisisthepy.dsa.agent.tasks.LauncherApps {
    var installed = listOf<io.github.thisisthepy.dsa.agent.tasks.LauncherApp>()
    private var onChange: (() -> Unit)? = null

    override fun list() = installed

    override fun listen(onChange: () -> Unit) {
        this.onChange = onChange
    }

    fun change(apps: List<io.github.thisisthepy.dsa.agent.tasks.LauncherApp>) {
        installed = apps
        onChange!!.invoke()
    }
}

/** The apps' shortcuts, by package; an app missing from [byPackage] has none. */
class FakeShortcuts : io.github.thisisthepy.dsa.agent.tasks.ShortcutSource {
    var byPackage = mapOf<String, List<io.github.thisisthepy.dsa.agent.tasks.LauncherShortcut>>()
    private var onChange: ((String) -> Unit)? = null

    override fun shortcuts(packageName: String) = byPackage[packageName].orEmpty()

    override fun listen(onChange: (String) -> Unit) {
        this.onChange = onChange
    }

    fun change(packageName: String, shortcuts: List<io.github.thisisthepy.dsa.agent.tasks.LauncherShortcut>) {
        byPackage = byPackage + (packageName to shortcuts)
        onChange!!.invoke(packageName)
    }
}

/** An app's pin request; [accepted] counts the times it was accepted. */
class FakePinRequest(
    override val packageName: String,
    override val shortcut: io.github.thisisthepy.dsa.agent.tasks.LauncherShortcut,
    override val appLabel: String = packageName.substringAfterLast('.'),
    var valid: Boolean = true,
) : io.github.thisisthepy.dsa.agent.tasks.PinRequest {
    var accepted = 0

    override fun accept(): Boolean {
        accepted++
        return valid
    }
}

class FakeActions : TaskActions {
    /** Each change of windowing mode asked for: the task, and its freeform bounds (null for fullscreen). */
    val windowing = mutableListOf<Pair<Int, TaskBounds?>>()

    override fun setWindowingMode(taskId: Int, freeformBounds: TaskBounds?): Boolean {
        windowing += taskId to freeformBounds
        return true
    }

    val launched = mutableListOf<String>()
    val closed = mutableListOf<Int>()
    val moved = mutableListOf<Pair<Int, TaskBounds>>()

    /** Each resize asked for: the task, its bounds, and the density if one was asked for. */
    val resized = mutableListOf<Triple<Int, TaskBounds, Int?>>()
    var launchable = setOf<String>()
    var closable = setOf<Int>()

    /** What the window manager does with a move; by default it accepts it and does nothing yet. */
    var onMove: (Int, TaskBounds) -> Boolean = { _, _ -> true }

    /** What the window manager takes of a resize; by default all of it, applied later. */
    var onResize: (Int, TaskBounds, Int?) -> ResizeOutcome = { _, _, _ -> ResizeOutcome.APPLIED }

    override fun launchApp(packageName: String): Boolean {
        launched += packageName
        return packageName in launchable
    }

    /** The apps that support several windows, and each app a new window was started for. */
    var multiWindow = setOf<String>()
    val newWindows = mutableListOf<String>()

    override fun supportsMultipleWindows(packageName: String): Boolean = packageName in multiWindow

    override fun launchNewWindow(packageName: String): Boolean {
        newWindows += packageName
        return packageName in launchable
    }

    /** Each shortcut started, as package and ID, and the ones that can start. */
    val shortcutsLaunched = mutableListOf<Pair<String, String>>()
    var startableShortcuts = setOf<Pair<String, String>>()

    override fun launchShortcut(packageName: String, shortcutId: String): Boolean {
        shortcutsLaunched += packageName to shortcutId
        return (packageName to shortcutId) in startableShortcuts
    }

    /** Each app whose settings were opened; null for the list of every app. */
    val settingsOpened = mutableListOf<String?>()

    /** The page each of those opened at. */
    val settingsPages = mutableListOf<SettingsPage>()
    var settingsOpen = true

    override fun openAppSettings(packageName: String?, page: SettingsPage): Boolean {
        settingsOpened += packageName
        settingsPages += page
        return settingsOpen
    }

    override fun closeTask(taskId: Int): Boolean {
        closed += taskId
        return taskId in closable
    }

    override fun moveTask(taskId: Int, bounds: TaskBounds): Boolean {
        moved += taskId to bounds
        return onMove(taskId, bounds)
    }

    override fun resizeTask(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome {
        resized += Triple(taskId, bounds, densityDpi)
        return onResize(taskId, bounds, densityDpi)
    }

    /** Each step of a drag: the task, its bounds, and the screen size its configuration keeps. */
    val resizedLive = mutableListOf<Triple<Int, TaskBounds, ScreenDp>>()

    /** Each end of a drag: the task, its bounds, and the density if one was asked for. */
    val endedLive = mutableListOf<Triple<Int, TaskBounds, Int?>>()

    /** What the window manager takes of a step of a drag; by default all of it, applied later. */
    var onResizeLive: (Int, TaskBounds, ScreenDp) -> ResizeOutcome = { _, _, _ -> ResizeOutcome.APPLIED }

    override fun resizeTaskLive(taskId: Int, bounds: TaskBounds, screen: ScreenDp): ResizeOutcome {
        resizedLive += Triple(taskId, bounds, screen)
        return onResizeLive(taskId, bounds, screen)
    }

    override fun endLiveResize(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome {
        endedLive += Triple(taskId, bounds, densityDpi)
        return ResizeOutcome.APPLIED
    }

    /** Each task given the input focus, in order. */
    val focused = mutableListOf<Int>()
    var focusable = true

    override fun focusTask(taskId: Int): Boolean {
        focused += taskId
        return focusable
    }

    /** Each caption inset given: the task and its frame, null for a removal. */
    val captionInsets = mutableListOf<Pair<Int, TaskBounds?>>()

    /** The bounding rectangles each of [captionInsets] carried. */
    val captionBoundingRects = mutableListOf<List<TaskBounds>>()
    var insetsTaken = true

    /** Whether each of [captionInsets] was for an app that starts below the title bar. */
    val captionOpaque = mutableListOf<Boolean>()

    override fun setCaptionInset(
        taskId: Int,
        frame: TaskBounds?,
        boundingRects: List<TaskBounds>,
        opaque: Boolean,
    ): Boolean {
        captionInsets += taskId to frame
        captionBoundingRects += boundingRects
        captionOpaque += opaque
        return insetsTaken
    }

    /** The framework's status bar height at each density; 0 for any other. */
    var statusBarHeights = mapOf<Int, Int>()

    override fun statusBarHeight(densityDpi: Int): Int = statusBarHeights[densityDpi] ?: 0

    /** Whether the framework can mark a task for forced system scaling; how often it was asked. */
    var forcedDensity = false
    var forcedDensityAsked = 0

    /** Each mark asked for, in order: the task and whether it is marked. */
    val forcedMarks = mutableListOf<Pair<Int, Boolean>>()
    var marksTaken = true

    override fun forcedDensitySupported(): Boolean {
        forcedDensityAsked++
        return forcedDensity
    }

    override fun markForcedDensity(taskId: Int, forced: Boolean): Boolean {
        forcedMarks += taskId to forced
        return marksTaken
    }
}

class FakeWindowing(var acceptAfter: Int = 0) : WindowingControl {
    var freeformEnabled = false
    var mode = 1
    var sets = 0
    var size: Size? = Size(1920, 1080)

    override fun enableFreeform() {
        freeformEnabled = true
    }

    override fun displayWindowingMode() = mode

    override fun setDisplayWindowingMode(mode: Int) {
        sets++
        if (freeformEnabled && sets > acceptAfter) this.mode = mode
    }

    override fun displaySize() = size ?: throw IllegalStateException("the display is not up")

    private var displayListener: (() -> Unit)? = null

    override fun listenToDisplay(onChange: () -> Unit) {
        displayListener = onChange
    }

    /** The display takes [to], as when the hwcomposer grows it, and the framework says so. */
    fun resize(to: Size) {
        size = to
        displayListener?.invoke()
    }
}

/** The framework's input: records what was injected, on a clock the test moves. */
/**
 * Takes injected input as the framework does. Motion events go through the same checks as the
 * framework's input verifier, which tracks each device's hovering and touching pointers on its
 * display: a HOVER_ENTER while the device hovers, a HOVER_EXIT while it does not, an
 * ACTION_DOWN while it hovers or is down, an ACTION_CANCEL without FLAG_CANCELED, and a
 * move, pointer change or lift that does not match the pointers down are refused, and do not
 * go in [injected]. A HOVER_MOVE without a HOVER_ENTER starts a hover, as the framework allows.
 */
class FakeInjector : InputInjector {
    val injected = mutableListOf<InjectedMotion>()

    /** The motion events refused by the checks, with the reason, as the framework logs it. */
    val rejected = mutableListOf<Pair<InjectedMotion, String>>()
    var clock = 1000L
    var refuse = false

    private val hovering = HashMap<Int, MutableSet<Int>>()
    private val touching = HashMap<Int, MutableSet<Int>>()

    override fun now() = clock

    override fun inject(motion: InjectedMotion): Boolean {
        if (refuse) return false
        val reason = verify(motion)
        if (reason != null) {
            rejected += motion to reason
            return false
        }
        injected += motion
        return true
    }

    /** Applies [motion] to the device's state; the reason it is refused, or null. */
    private fun verify(motion: InjectedMotion): String? {
        val device = motion.deviceId
        val ids = motion.pointers.map { it.id }.toSet()
        val down = touching[device].orEmpty()
        val index = (motion.action and android.view.MotionEvent.ACTION_POINTER_INDEX_MASK) shr
            android.view.MotionEvent.ACTION_POINTER_INDEX_SHIFT
        when (motion.action and android.view.MotionEvent.ACTION_MASK) {
            android.view.MotionEvent.ACTION_HOVER_ENTER -> {
                if (hovering.containsKey(device)) return "Invalid HOVER_ENTER event - pointers already hovering for device $device"
                hovering[device] = ids.toMutableSet()
            }
            android.view.MotionEvent.ACTION_HOVER_MOVE -> hovering[device] = ids.toMutableSet()
            android.view.MotionEvent.ACTION_HOVER_EXIT -> {
                if (hovering.remove(device) == null) return "Invalid HOVER_EXIT event - no pointers hovering for device $device"
            }
            android.view.MotionEvent.ACTION_DOWN -> {
                if (touching.containsKey(device)) return "Invalid DOWN event - pointers already down for device $device"
                if (hovering.containsKey(device)) return "Invalid DOWN event - pointers hovering for device $device"
                touching[device] = ids.toMutableSet()
            }
            android.view.MotionEvent.ACTION_POINTER_DOWN -> {
                val id = motion.pointers[index].id
                if (device !in touching || id in down || ids - id != down) {
                    return "Invalid POINTER_DOWN event for pointer $id of device $device"
                }
                touching.getValue(device).add(id)
            }
            android.view.MotionEvent.ACTION_MOVE -> {
                if (ids != down) return "Invalid MOVE event - pointers $ids do not match those down, $down"
            }
            android.view.MotionEvent.ACTION_POINTER_UP -> {
                val id = motion.pointers[index].id
                if (ids != down || down.size < 2) return "Invalid POINTER_UP event for pointer $id of device $device"
                touching.getValue(device).remove(id)
            }
            android.view.MotionEvent.ACTION_UP -> {
                if (ids != down || down.size != 1) return "Invalid UP event - pointers $ids do not match those down, $down"
                touching.remove(device)
            }
            android.view.MotionEvent.ACTION_CANCEL -> {
                if (motion.flags and CANCELED == 0) return "For ACTION_CANCEL, must set FLAG_CANCELED"
                if (ids != down) return "Invalid CANCEL event - pointers $ids do not match those down, $down"
                touching.remove(device)
            }
        }
        return null
    }

    private companion object {
        /** MotionEvent.FLAG_CANCELED. */
        const val CANCELED = 0x20
    }

    val keys = mutableListOf<InjectedKey>()

    override fun injectKey(key: InjectedKey): Boolean {
        if (refuse) return false
        keys += key
        return true
    }
}

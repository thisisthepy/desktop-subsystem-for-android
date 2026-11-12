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

    override fun setCaptionInset(taskId: Int, frame: TaskBounds?, boundingRects: List<TaskBounds>): Boolean {
        captionInsets += taskId to frame
        captionBoundingRects += boundingRects
        return insetsTaken
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
}

/** The framework's input: records what was injected, on a clock the test moves. */
class FakeInjector : InputInjector {
    val injected = mutableListOf<InjectedMotion>()
    var clock = 1000L
    var refuse = false

    override fun now() = clock

    override fun inject(motion: InjectedMotion): Boolean {
        if (refuse) return false
        injected += motion
        return true
    }

    val keys = mutableListOf<InjectedKey>()

    override fun injectKey(key: InjectedKey): Boolean {
        if (refuse) return false
        keys += key
        return true
    }
}

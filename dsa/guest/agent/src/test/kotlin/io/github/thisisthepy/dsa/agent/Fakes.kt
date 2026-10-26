package io.github.thisisthepy.dsa.agent

import io.github.thisisthepy.dsa.agent.channel.ChannelEndpoint
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.requests.ResizeOutcome
import io.github.thisisthepy.dsa.agent.input.InjectedMotion
import io.github.thisisthepy.dsa.agent.input.InputInjector
import io.github.thisisthepy.dsa.agent.requests.TaskActions
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
) = TaskSnapshot(taskId, kind, packageName, label, bounds, focused, densityDpi)

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

    fun sentMessages(): List<Envelope> = sent.map(::decode)
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
}

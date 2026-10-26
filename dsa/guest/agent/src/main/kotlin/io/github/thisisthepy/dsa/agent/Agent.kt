package io.github.thisisthepy.dsa.agent

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.channel.ChannelClient
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.input.InputInjector
import io.github.thisisthepy.dsa.agent.input.PointerInput
import io.github.thisisthepy.dsa.agent.requests.HostRequests
import io.github.thisisthepy.dsa.agent.requests.RequestResult
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import io.github.thisisthepy.dsa.agent.tasks.TaskReporter
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.windowing.FreeformSetup
import io.github.thisisthepy.dsa.agent.windowing.TaskArranger
import io.github.thisisthepy.dsa.agent.windowing.WindowingControl
import io.github.thisisthepy.dsa.control.v1.Envelope

/**
 * The agent: makes the display freeform, keeps app tasks from overlapping on it, reports
 * them to the host over the control daemon, and acts on the host's requests: launching an
 * app, ending a task, and giving a task the size and density of its host window.
 *
 * Everything runs on the scheduler's one thread. Task changes are coalesced: however many
 * arrive before the agent gets to them, it reads the tasks once. Pointer input from the
 * host is injected into its task as it arrives, and reads nothing.
 */
class Agent(
    private val ownPackage: String,
    private val tasks: TaskSource,
    actions: TaskActions,
    locator: ChannelLocator,
    windowing: WindowingControl,
    injector: InputInjector,
    private val scheduler: Scheduler,
    private val log: Log,
    apps: AppInfoSource = AppInfoSource.NONE,
) {
    private val channel = ChannelClient(
        locator = locator,
        scheduler = scheduler,
        log = log,
        onFrame = ::onFrame,
        onConnected = ::onConnected,
    )
    private val reporter = TaskReporter(ownPackage, apps) { frame -> channel.send(frame) }
    private val requests = HostRequests(ownPackage, actions, reporter::isReported)
    private val pointer = PointerInput(injector, reporter::reportedBounds, log)
    private val freeform = FreeformSetup(windowing, scheduler, log)
    private val arranger = TaskArranger(actions, windowing::displaySize, log)
    private var refreshPending = false
    private var checkPending = false

    fun start() {
        scheduler.post {
            freeform.start()
            listen()
            channel.start()
            requestRefresh()
        }
    }

    private fun listen() {
        try {
            tasks.listen { scheduler.post(::requestRefresh) }
        } catch (e: Exception) {
            log.warn("listening to task changes failed; trying again", e)
            scheduler.postDelayed(RETRY_MILLIS, ::listen)
        }
    }

    private fun onConnected() {
        reporter.resend()
        requestRefresh()
    }

    private fun onFrame(frame: ByteArray) {
        if (pointerInput(frame)) return
        val result = try {
            requests.handle(frame)
        } catch (e: Exception) {
            log.warn("acting on a frame from the channel failed", e)
            return
        }
        when (result) {
            is RequestResult.Launched -> log.info("launched ${result.packageName}")
            is RequestResult.LaunchFailed -> log.warn("could not launch ${result.packageName}: ${result.reason}")
            is RequestResult.Closed -> log.info("ended task ${result.taskId}")
            is RequestResult.CloseRefused -> log.warn("did not end task ${result.taskId}: ${result.reason}")
            is RequestResult.Resize -> arranger.resize(result.taskId, result.size, result.live)
            is RequestResult.Density -> {
                log.info("task ${result.taskId} draws at ${result.densityDpi} dpi")
                arranger.setDensity(result.taskId, result.densityDpi)
            }
            is RequestResult.SizeRefused -> log.warn("did not resize task ${result.taskId}: ${result.reason}")
            is RequestResult.Malformed -> log.warn("dropped a frame: ${result.reason}")
            is RequestResult.NotForTheAgent -> Unit
        }
        if (result !is RequestResult.NotForTheAgent) requestRefresh()
    }

    /** Injects the frame if it is pointer input. False for any other frame. */
    private fun pointerInput(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        if (envelope.bodyCase != Envelope.BodyCase.POINTER_EVENT) return false
        try {
            pointer.handle(envelope.pointerEvent)
        } catch (e: Exception) {
            log.warn("injecting pointer input failed", e)
        }
        return true
    }

    private fun requestRefresh() {
        if (refreshPending) return
        refreshPending = true
        scheduler.post {
            refreshPending = false
            refresh()
        }
    }

    /**
     * Reads the tasks, gives them the sizes the host asked for, moves app tasks that overlap
     * apart, and reports them. A task that is being moved or resized is reported at the
     * bounds it is going to, so the host never hears of two app tasks that overlap, and a
     * window hears at once that its task is taking its size.
     */
    private fun refresh() {
        val running = try {
            tasks.snapshot()
        } catch (e: Exception) {
            log.warn("reading the running tasks failed", e)
            return
        }
        val bounds = arranger.arrange(running.filter(reporter::reportable))
        if (arranger.waiting) checkMoves()
        if (!channel.connected) return
        reporter.update(
            running.map { task ->
                val placed = bounds[task.taskId]
                if (placed == null || placed == task.bounds) task else task.copy(bounds = placed)
            },
            arranger.display,
        )
        pointer.retainTasks(reporter.reported)
    }

    /**
     * Reads the tasks again shortly: a move or a resize does not always call the task stack
     * listener.
     */
    private fun checkMoves() {
        if (checkPending) return
        checkPending = true
        val interval = arranger.checkIntervalMillis
        scheduler.postDelayed(interval) {
            checkPending = false
            arranger.tick(interval)
            requestRefresh()
        }
    }

    private companion object {
        const val RETRY_MILLIS = 1000L
    }
}

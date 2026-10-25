package io.github.thisisthepy.dsa.agent

import io.github.thisisthepy.dsa.agent.channel.ChannelClient
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.requests.HostRequests
import io.github.thisisthepy.dsa.agent.requests.RequestResult
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.TaskReporter
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.windowing.FreeformSetup
import io.github.thisisthepy.dsa.agent.windowing.TaskArranger
import io.github.thisisthepy.dsa.agent.windowing.WindowingControl

/**
 * The agent: makes the display freeform, keeps app tasks from overlapping on it, reports
 * them to the host over the control daemon, and acts on the host's requests.
 *
 * Everything runs on the scheduler's one thread. Task changes are coalesced: however many
 * arrive before the agent gets to them, it reads the tasks once.
 */
class Agent(
    private val ownPackage: String,
    private val tasks: TaskSource,
    actions: TaskActions,
    locator: ChannelLocator,
    windowing: WindowingControl,
    private val scheduler: Scheduler,
    private val log: Log,
) {
    private val channel = ChannelClient(
        locator = locator,
        scheduler = scheduler,
        log = log,
        onFrame = ::onFrame,
        onConnected = ::onConnected,
    )
    private val reporter = TaskReporter(ownPackage) { frame -> channel.send(frame) }
    private val requests = HostRequests(ownPackage, actions, reporter::isReported)
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
            is RequestResult.Malformed -> log.warn("dropped a frame: ${result.reason}")
            is RequestResult.NotForTheAgent -> Unit
        }
        if (result !is RequestResult.NotForTheAgent) requestRefresh()
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
     * Reads the tasks, moves app tasks that overlap apart, and reports them. A task that is
     * being moved is reported at the place it is going to, so the host never hears of two
     * app tasks that overlap.
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
        )
    }

    /** Reads the tasks again shortly: a move does not always call the task stack listener. */
    private fun checkMoves() {
        if (checkPending) return
        checkPending = true
        scheduler.postDelayed(MOVE_CHECK_MILLIS) {
            checkPending = false
            arranger.tick()
            requestRefresh()
        }
    }

    private companion object {
        const val RETRY_MILLIS = 1000L
        const val MOVE_CHECK_MILLIS = 100L
    }
}

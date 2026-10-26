package io.github.thisisthepy.dsa.agent

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.channel.ChannelClient
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.input.EditorConnection
import io.github.thisisthepy.dsa.agent.input.InputInjector
import io.github.thisisthepy.dsa.agent.input.InputMethodSetup
import io.github.thisisthepy.dsa.agent.input.KeyInput
import io.github.thisisthepy.dsa.agent.input.PointerInput
import io.github.thisisthepy.dsa.agent.input.TextInput
import io.github.thisisthepy.dsa.agent.input.TextKeys
import io.github.thisisthepy.dsa.agent.memory.MemoryReporter
import io.github.thisisthepy.dsa.agent.memory.MemorySource
import io.github.thisisthepy.dsa.agent.requests.HostRequests
import io.github.thisisthepy.dsa.agent.requests.RequestResult
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import io.github.thisisthepy.dsa.agent.tasks.InstalledAppReporter
import io.github.thisisthepy.dsa.agent.tasks.LauncherApps
import io.github.thisisthepy.dsa.agent.tasks.TaskReporter
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.trace.ResizeTrace
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
 * host is injected into its task as it arrives, and reads nothing; a key first gives its
 * task the input focus if the task does not hold it. Text from the host's input method goes
 * to the focused editor through the agent's own input method, which the agent makes the
 * guest's at boot, and the editor's cursor goes back to the host. The apps a launcher lists
 * go to the host too, on each connection and whenever one is installed, updated or removed,
 * so the host's start menu can show them. With a [MemorySource], it also reports the guest's
 * memory every few minutes and whenever the host asks.
 */
class Agent(
    private val ownPackage: String,
    private val tasks: TaskSource,
    private val actions: TaskActions,
    locator: ChannelLocator,
    windowing: WindowingControl,
    injector: InputInjector,
    private val scheduler: Scheduler,
    private val log: Log,
    apps: AppInfoSource = AppInfoSource.NONE,
    editor: () -> EditorConnection = { EditorConnection.NONE },
    textKeys: TextKeys = TextKeys { null },
    private val inputMethods: InputMethodSetup = InputMethodSetup.NONE,
    private val trace: ResizeTrace = ResizeTrace(log),
    private val launcherApps: LauncherApps = LauncherApps.NONE,
    memory: MemorySource? = null,
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
    private val keys = KeyInput(injector, reporter::isReported, reporter::reportedFocused, actions::focusTask, log)
    private val text = TextInput(
        editor = editor,
        injector = injector,
        keys = textKeys,
        isReported = reporter::isReported,
        isFocused = reporter::reportedFocused,
        focus = actions::focusTask,
        bounds = reporter::reportedBounds,
        focusedTask = reporter::focusedTask,
        send = { frame -> channel.send(frame) },
        log = log,
    )
    private val memoryReporter = memory?.let {
        MemoryReporter(it, scheduler, log, isConnected = { channel.connected }, send = { frame -> channel.send(frame) })
    }
    private val freeform = FreeformSetup(windowing, scheduler, log)
    private val arranger = TaskArranger(actions, windowing::displaySize, log)
    private val installed = InstalledAppReporter(ownPackage) { frame -> channel.send(frame) }
    private var refreshPending = false
    private var appsPending = false
    private var checkPending = false

    fun start() {
        scheduler.post {
            freeform.start()
            try {
                inputMethods.useHostInputMethod()
            } catch (e: Exception) {
                log.warn("could not make the agent's input method the guest's; typed text may not reach editors", e)
            }
            listen()
            listenToApps()
            channel.start()
            requestRefresh()
        }
    }

    private fun listenToApps() {
        try {
            launcherApps.listen { scheduler.post(::requestAppsRefresh) }
        } catch (e: Exception) {
            log.warn("listening to installed apps failed; trying again", e)
            scheduler.postDelayed(RETRY_MILLIS, ::listenToApps)
        }
    }

    private fun requestAppsRefresh() {
        if (appsPending) return
        appsPending = true
        scheduler.post {
            appsPending = false
            refreshApps()
        }
    }

    /** Tells the host which apps a launcher lists, when that changed. */
    private fun refreshApps() {
        if (!channel.connected) return
        val apps = try {
            launcherApps.list()
        } catch (e: Exception) {
            log.warn("reading the installed apps failed", e)
            return
        } ?: return
        installed.update(apps)
    }

    private fun listen() {
        try {
            tasks.listen { scheduler.post(::requestRefresh) }
        } catch (e: Exception) {
            log.warn("listening to task changes failed; trying again", e)
            scheduler.postDelayed(RETRY_MILLIS, ::listen)
        }
    }

    /**
     * The focused editor's cursor is at these display pixels, as the agent's input method
     * read it. Called from any thread.
     */
    fun cursorMoved(left: Float, top: Float, right: Float, bottom: Float) {
        scheduler.post {
            if (channel.connected) text.cursorMoved(left, top, right, bottom)
        }
    }

    private fun onConnected() {
        reporter.resend()
        installed.resend()
        memoryReporter?.connected()
        requestRefresh()
        requestAppsRefresh()
    }

    private fun onFrame(frame: ByteArray) {
        if (input(frame)) return
        if (memoryRequest(frame)) return
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
            is RequestResult.Resize -> {
                trace.received(result.taskId, result.size, result.live)
                arranger.resize(result.taskId, result.size, result.live)
            }
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

    /** Injects the frame if it is pointer, key or text input. False for any other frame. */
    private fun input(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        when (envelope.bodyCase) {
            Envelope.BodyCase.POINTER_EVENT -> try {
                pointer.handle(envelope.pointerEvent)
            } catch (e: Exception) {
                log.warn("injecting pointer input failed", e)
            }
            Envelope.BodyCase.KEY_EVENT -> try {
                keys.handle(envelope.keyEvent)
            } catch (e: Exception) {
                log.warn("injecting key input failed", e)
            }
            Envelope.BodyCase.IME_COMPOSITION -> try {
                text.handle(envelope.imeComposition)
            } catch (e: Exception) {
                log.warn("putting a composition into the editor failed", e)
            }
            Envelope.BodyCase.IME_COMMIT -> try {
                text.handle(envelope.imeCommit)
            } catch (e: Exception) {
                log.warn("typing committed text failed", e)
            }
            else -> return false
        }
        return true
    }

    /** Reports the guest's memory if the frame asks for it. False for any other frame. */
    private fun memoryRequest(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        if (envelope.bodyCase != Envelope.BodyCase.REQUEST_GUEST_MEMORY) return false
        val reporter = memoryReporter
        if (reporter == null) {
            log.warn("the host asked for the guest's memory, which this agent does not read")
        } else {
            try {
                reporter.report()
            } catch (e: Exception) {
                log.warn("reporting the guest's memory failed", e)
            }
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
        for (task in running) trace.boundsRead(task.taskId, task.bounds.right - task.bounds.left, task.bounds.bottom - task.bounds.top)
        val arranging = System.nanoTime()
        val bounds = arranger.arrange(running.filter(reporter::reportable))
        trace.applied((System.nanoTime() - arranging) / 1_000_000.0)
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
        keys.retainTasks(reporter.reported)
        text.retainTasks(reporter.reported)
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

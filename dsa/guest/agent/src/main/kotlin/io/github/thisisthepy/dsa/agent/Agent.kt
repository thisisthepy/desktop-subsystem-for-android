package io.github.thisisthepy.dsa.agent

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilityExport
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilitySource
import io.github.thisisthepy.dsa.agent.channel.ChannelClient
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.clipboard.ClipboardSync
import io.github.thisisthepy.dsa.agent.clipboard.GuestClipboard
import io.github.thisisthepy.dsa.agent.diagnostics.DiagnosticsCollector
import io.github.thisisthepy.dsa.agent.diagnostics.DiagnosticsSource
import io.github.thisisthepy.dsa.agent.input.EditorConnection
import io.github.thisisthepy.dsa.agent.install.ApkInstaller
import io.github.thisisthepy.dsa.agent.install.PackageInfoPort
import io.github.thisisthepy.dsa.agent.install.PackageInstallerPort
import io.github.thisisthepy.dsa.agent.install.PackageQueries
import io.github.thisisthepy.dsa.agent.input.InputInjector
import io.github.thisisthepy.dsa.agent.input.InputMethodSetup
import io.github.thisisthepy.dsa.agent.input.KeyInput
import io.github.thisisthepy.dsa.agent.input.PointerInput
import io.github.thisisthepy.dsa.agent.input.PointerQueue
import io.github.thisisthepy.dsa.agent.input.TextInput
import io.github.thisisthepy.dsa.agent.input.TextKeys
import io.github.thisisthepy.dsa.agent.input.TouchSounds
import io.github.thisisthepy.dsa.agent.files.FileOpener
import io.github.thisisthepy.dsa.agent.files.FileReceiver
import io.github.thisisthepy.dsa.agent.files.TransferStore
import io.github.thisisthepy.dsa.agent.links.BrowserRole
import io.github.thisisthepy.dsa.agent.links.GuestBrowser
import io.github.thisisthepy.dsa.agent.links.Links
import io.github.thisisthepy.dsa.agent.power.DevicePower
import io.github.thisisthepy.dsa.agent.power.Lifecycle
import io.github.thisisthepy.dsa.agent.memory.AppProcesses
import io.github.thisisthepy.dsa.agent.memory.ClosedApps
import io.github.thisisthepy.dsa.agent.memory.MemoryReporter
import io.github.thisisthepy.dsa.agent.memory.MemorySource
import io.github.thisisthepy.dsa.agent.notifications.NotificationForwarder
import io.github.thisisthepy.dsa.agent.notifications.NotificationSource
import io.github.thisisthepy.dsa.agent.requests.HostRequests
import io.github.thisisthepy.dsa.agent.requests.RequestResult
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import io.github.thisisthepy.dsa.agent.tasks.InstalledAppReporter
import io.github.thisisthepy.dsa.agent.tasks.LauncherApps
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskReporter
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.trace.FrameStatsTrace
import io.github.thisisthepy.dsa.agent.trace.InputTrace
import io.github.thisisthepy.dsa.agent.trace.ResizeTrace
import io.github.thisisthepy.dsa.agent.widgets.WidgetManager
import io.github.thisisthepy.dsa.agent.widgets.WidgetPlatform
import io.github.thisisthepy.dsa.agent.widgets.WidgetStore
import io.github.thisisthepy.dsa.agent.windowing.FreeformSetup
import io.github.thisisthepy.dsa.agent.windowing.TaskArranger
import io.github.thisisthepy.dsa.agent.windowing.WindowingControl
import io.github.thisisthepy.dsa.control.v1.AgentConnected
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PointerAction

/**
 * The agent: makes the display freeform, keeps app tasks from overlapping on it, reports
 * them to the host over the control daemon, and acts on the host's requests: launching an
 * app, ending a task, and giving a task the size and density of its host window.
 *
 * Everything but pointer input runs on the scheduler's one thread. Task changes are
 * coalesced: however many arrive before the agent gets to them, it reads the tasks once.
 * Pointer input from the host goes to [inputScheduler]'s thread as it arrives, past whatever
 * the scheduler's thread is doing, because reading tasks and starting apps are binder calls
 * into a framework that may be busy for hundreds of milliseconds, and a touch must not wait
 * for them. A key first gives its
 * task the input focus if the task does not hold it. Text from the host's input method goes
 * to the focused editor through the agent's own input method, which the agent makes the
 * guest's at boot, and the editor's cursor goes back to the host. The apps a launcher lists
 * go to the host too, on each connection and whenever one is installed, updated or removed,
 * so the host's start menu can show them. With a [MemorySource], it also reports the guest's
 * memory every few minutes and whenever the host asks. When the host says no task window is
 * shown the agent turns the screen off and puts the device into Doze, and the host's battery
 * becomes the guest's ([Lifecycle]). Text copied on the host becomes the guest's clipboard
 * text, and text copied in the guest goes to the host, while the user shares the clipboard
 * ([ClipboardSync]). Web links apps open go to the host's browser unless the host says they
 * stay in the guest ([Links]), and files the host sends are stored and offered to the apps
 * that can open them ([FileReceiver]). APKs the host sends are installed through the guest's
 * package installer ([ApkInstaller]), and the host is told how each install went. The guest's
 * notifications go to the host's notification centre, and a click or a dismissal there comes
 * back to them ([NotificationForwarder]). While a host accessibility client watches a task, the
 * agent's accessibility service is on and the task's accessibility tree goes to the host
 * ([AccessibilityExport]); otherwise the service is off. The widgets the user placed on the
 * host's desktop are hosted and drawn here, and their images go to the host ([WidgetManager]).
 * When the host gathers diagnostics, the agent sends its part: the system services' state and
 * the dropbox ([DiagnosticsCollector]).
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
    power: DevicePower = DevicePower.NONE,
    private val launcherApps: LauncherApps = LauncherApps.NONE,
    memory: MemorySource? = null,
    private val inputScheduler: Scheduler = scheduler,
    private val inputTrace: InputTrace = InputTrace(log),
    private val frameTrace: FrameStatsTrace? = null,
    private val touchSounds: TouchSounds = TouchSounds.NONE,
    browserRole: BrowserRole = BrowserRole.NONE,
    guestBrowser: GuestBrowser = GuestBrowser.NONE,
    fileStore: TransferStore? = null,
    fileOpener: FileOpener = FileOpener.NONE,
    clipboard: GuestClipboard = GuestClipboard.NONE,
    notifications: NotificationSource = NotificationSource.NONE,
    packageInstaller: PackageInstallerPort = PackageInstallerPort.NONE,
    packages: PackageInfoPort = PackageInfoPort.NONE,
    widgets: WidgetPlatform = WidgetPlatform.NONE,
    appProcesses: AppProcesses = AppProcesses.NONE,
    widgetStore: WidgetStore = WidgetStore.memory(),
    accessibility: AccessibilitySource = AccessibilitySource.NONE,
    clock: () -> Long = { System.nanoTime() / 1_000_000 },
    diagnostics: DiagnosticsSource = DiagnosticsSource.NONE,
    diagnosticsRunner: (Runnable) -> Unit = { Thread(it, "dsa-diagnostics").start() },
) {
    private val channel = ChannelClient(
        locator = locator,
        scheduler = scheduler,
        log = log,
        onFrame = ::onFrame,
        onConnected = ::onConnected,
        intercept = ::pointerFrame,
    )
    private val reporter = TaskReporter(ownPackage, apps) { frame -> channel.send(frame) }
    private val requests = HostRequests(ownPackage, actions, reporter::isReported)

    /** Where the host was last told each task is, for the input thread to read. */
    @Volatile
    private var taskBounds: Map<Int, TaskBounds> = emptyMap()
    private val pointer = PointerInput(injector, { taskId -> taskBounds[taskId] }, log)
    private val pointerQueue = PointerQueue(inputScheduler) { event, queuedNanos ->
        val started = System.nanoTime()
        try {
            pointer.handle(event)
        } catch (e: Exception) {
            log.warn("injecting pointer input failed", e)
        }
        inputTrace.injected(event, queuedNanos, System.nanoTime() - started)
        val frames = frameTrace
        if (frames != null && event.action == PointerAction.POINTER_ACTION_UP) {
            // The reporter belongs to the scheduler's thread.
            scheduler.post { frames.released(reporter.reportedPackage(event.taskId)) }
        }
    }
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
    private val lifecycle = Lifecycle(power, { frame -> channel.send(frame) }, log, { reporter.reported.isNotEmpty() })
    private val memoryReporter = memory?.let {
        MemoryReporter(it, scheduler, log, isConnected = { channel.connected }, send = { frame -> channel.send(frame) })
    }
    private val clipboardSync = ClipboardSync(clipboard, { frame -> channel.send(frame) }, log, isConnected = { channel.connected })
    private val diagnosticsCollector = DiagnosticsCollector(diagnostics, { frame -> channel.send(frame) }, log, diagnosticsRunner)
    private val notificationForwarder = NotificationForwarder(
        ownPackage,
        notifications,
        { frame -> channel.send(frame) },
        log,
        isConnected = { channel.connected },
    )
    private val apkInstaller = ApkInstaller(
        packageInstaller,
        { frame -> channel.send(frame) },
        log,
        post = { work -> scheduler.post(work) },
        onInstalled = { requestAppsRefresh() },
    )
    private val packageQueries = PackageQueries(packages, { frame -> channel.send(frame) }, log)
    private val widgetManager = WidgetManager(
        widgets,
        widgetStore,
        { frame -> channel.send(frame) },
        log,
        isConnected = { channel.connected },
    )
    private val closedApps = ClosedApps(
        appProcesses,
        hasPlacedWidget = { packageName ->
            widgets.placedIds().any { id -> widgets.info(id)?.packageName == packageName }
        },
        scheduler = scheduler,
        log = log,
        clock = clock,
    )
    private val freeform = FreeformSetup(windowing, scheduler, log)
    private val arranger = TaskArranger(actions, windowing::displaySize, log)
    private val installed = InstalledAppReporter(ownPackage) { frame -> channel.send(frame) }
    private val links = Links(browserRole, guestBrowser, { frame -> channel.send(frame) }, log)
    private val files = FileReceiver(fileStore, fileOpener, { frame -> channel.send(frame) }, log)
    private val accessibilityExport = AccessibilityExport(
        accessibility,
        scheduler,
        { frame -> channel.send(frame) },
        log,
        reporter::reportedBounds,
        clock,
    )
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
            try {
                touchSounds.turnOff()
            } catch (e: Exception) {
                log.warn("could not turn click sounds off; each click spends time on a sound nobody hears", e)
            }
            accessibilityExport.start()
            listen()
            lifecycle.start { work -> scheduler.post(work) }
            clipboardSync.start { work -> scheduler.post(work) }
            notificationForwarder.start { work -> scheduler.post(work) }
            widgetManager.start(later = { work -> scheduler.postDelayed(RETRY_MILLIS, work) }) { work ->
                scheduler.post(work)
            }
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
        // The daemon tells the host from this that the guest's requests have someone to reach.
        channel.send(Envelope.newBuilder().setAgentConnected(AgentConnected.getDefaultInstance()).build().toByteArray())
        reporter.resend()
        lifecycle.connected()
        clipboardSync.connected()
        notificationForwarder.connected()
        widgetManager.connected()
        apkInstaller.connected()
        installed.resend()
        memoryReporter?.connected()
        links.connected()
        files.connected()
        accessibilityExport.connected()
        requestRefresh()
        requestAppsRefresh()
    }

    private fun onFrame(frame: ByteArray) {
        if (input(frame)) return
        if (memoryRequest(frame)) return
        if (stopRequest(frame)) return
        if (diagnosticsCollector.handle(frame)) return
        if (linksOrFiles(frame)) return
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

    /** Queues the frame for the input thread if it is pointer input. Called on a binder thread. */
    private fun pointerFrame(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            // The scheduler's thread reports it.
            return false
        }
        if (envelope.bodyCase != Envelope.BodyCase.POINTER_EVENT) return false
        pointerQueue.add(envelope.pointerEvent)
        return true
    }

    /** Injects the frame if it is key or text input. False for any other frame. */
    private fun input(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        if (envelope.bodyCase == Envelope.BodyCase.SCREEN_STATE) widgetManager.screen(envelope.screenState.on)
        if (lifecycle.handle(envelope)) return true
        try {
            if (widgetManager.handle(envelope)) return true
        } catch (e: Exception) {
            log.warn("acting on a widget frame from the host failed", e)
            return true
        }
        if (clipboardSync.handle(envelope)) return true
        if (notificationForwarder.handle(envelope)) return true
        if (apkInstaller.handle(envelope)) return true
        if (packageQueries.handle(envelope)) return true
        when (envelope.bodyCase) {
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

    /** Takes the frame if it is about web links or a file from the host. False for any other frame. */
    private fun linksOrFiles(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        return try {
            links.handle(envelope) || files.handle(envelope) || accessibilityExport.handle(envelope)
        } catch (e: Exception) {
            log.warn("acting on a link, file or accessibility frame from the host failed", e)
            true
        }
    }

    /**
     * An app opened [url] through the agent's browser activity: it goes to the host, or opens
     * in a browser in the guest when links stay there or the host cannot be reached. Called
     * from any thread.
     */
    fun openLink(url: String?) {
        scheduler.post { links.open(url, channel.connected) }
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

    /**
     * Ends an app when the host says Stop (StopApp), even one a widget or a foreground service
     * keeps running. False for any other frame.
     */
    private fun stopRequest(frame: ByteArray): Boolean {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return false
        }
        if (envelope.bodyCase != Envelope.BodyCase.STOP_APP) return false
        val packageName = envelope.stopApp.packageName
        if (packageName.isEmpty() || packageName == ownPackage) {
            log.warn("the host asked to stop ${packageName.ifEmpty { "no app" }}, which is not an app it can stop")
        } else {
            closedApps.stop(packageName, force = true)
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
        val reported = reporter.reported
        val told = reported.mapNotNull { taskId -> reporter.reportedBounds(taskId)?.let { taskId to it } }.toMap()
        if (told != taskBounds) {
            taskBounds = told
            inputScheduler.post { pointer.retainTasks(reported) }
        }
        keys.retainTasks(reporter.reported)
        text.retainTasks(reporter.reported)
        closedApps.tasks(reported.mapNotNull(reporter::reportedPackage).toSet())
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

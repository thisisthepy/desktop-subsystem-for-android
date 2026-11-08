package io.github.thisisthepy.dsa.agent

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilityExport
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilitySource
import io.github.thisisthepy.dsa.agent.channel.ChannelClient
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.agent.clipboard.ClipboardSync
import io.github.thisisthepy.dsa.agent.clipboard.GuestClipboard
import io.github.thisisthepy.dsa.agent.diagnostics.CrashRecordSource
import io.github.thisisthepy.dsa.agent.diagnostics.DiagnosticsCollector
import io.github.thisisthepy.dsa.agent.diagnostics.DiagnosticsSource
import io.github.thisisthepy.dsa.agent.diagnostics.NativeLibraryCrashes
import io.github.thisisthepy.dsa.agent.input.EditorConnection
import io.github.thisisthepy.dsa.agent.install.ApkInstaller
import io.github.thisisthepy.dsa.agent.install.AppReinstaller
import io.github.thisisthepy.dsa.agent.install.AppUninstaller
import io.github.thisisthepy.dsa.agent.install.PackageReinstallerPort
import io.github.thisisthepy.dsa.agent.install.PackageUninstallerPort
import io.github.thisisthepy.dsa.agent.install.GoogleDeviceIdPort
import io.github.thisisthepy.dsa.agent.install.GoogleDeviceIds
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
import io.github.thisisthepy.dsa.agent.input.UnhandledKeys
import io.github.thisisthepy.dsa.agent.files.DragCatcher
import io.github.thisisthepy.dsa.agent.files.DragOut
import io.github.thisisthepy.dsa.agent.files.FileOpener
import io.github.thisisthepy.dsa.agent.files.FileReceiver
import io.github.thisisthepy.dsa.agent.files.MediaScan
import io.github.thisisthepy.dsa.agent.files.SharedFolders
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
import io.github.thisisthepy.dsa.agent.network.NetworkSetup
import io.github.thisisthepy.dsa.agent.network.WifiPort
import io.github.thisisthepy.dsa.agent.background.AppStopper
import io.github.thisisthepy.dsa.agent.background.BackgroundApps
import io.github.thisisthepy.dsa.agent.background.MediaSource
import io.github.thisisthepy.dsa.agent.notifications.NotificationForwarder
import io.github.thisisthepy.dsa.agent.notifications.NotificationSource
import io.github.thisisthepy.dsa.agent.print.HostPrinter
import io.github.thisisthepy.dsa.agent.print.QueuedPrintJob
import io.github.thisisthepy.dsa.agent.requests.HostRequests
import io.github.thisisthepy.dsa.agent.requests.RequestResult
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.settings.DeviceNamePort
import io.github.thisisthepy.dsa.agent.settings.DeviceNameSync
import io.github.thisisthepy.dsa.agent.settings.SystemSettingsPort
import io.github.thisisthepy.dsa.agent.settings.SystemSettingsSync
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
import io.github.thisisthepy.dsa.agent.windowing.GestureExclusion
import io.github.thisisthepy.dsa.agent.windowing.CaptionInsets
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
 * that can open them ([FileReceiver]). Files and text an app drags out of its window go on to
 * the host's own drag and drop ([DragOut]). APKs the host sends are installed through the guest's
 * package installer ([ApkInstaller]), and the host is told how each install went. The guest's
 * notifications go to the host's notification centre, and a click or a dismissal there comes
 * back to them ([NotificationForwarder]). Apps that keep running with no window, because they
 * run a foreground service, are shown in the host's notification area, and their media sessions
 * reach the host's media keys ([BackgroundApps]). While a host accessibility client watches a task, the
 * agent's accessibility service is on and the task's accessibility tree goes to the host
 * ([AccessibilityExport]); otherwise the service is off. The widgets the user placed on the
 * host's desktop are hosted and drawn here, and their images go to the host ([WidgetManager]).
 * When the host gathers diagnostics, the agent sends its part: the system services' state and
 * the dropbox ([DiagnosticsCollector]). Documents apps print to the agent's print service go
 * to the host, which prints them on its printers ([HostPrinter]). The host's time zone, display
 * languages, regional format, dark theme and accent color become the guest's
 * ([SystemSettingsSync]), and the computer's name becomes the guest's device name ([DeviceNameSync]).
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
    mediaScan: MediaScan = MediaScan.NONE,
    sharedStorage: java.io.File = java.io.File("/storage/emulated/0"),
    clipboard: GuestClipboard = GuestClipboard.NONE,
    notifications: NotificationSource = NotificationSource.NONE,
    media: MediaSource = MediaSource.NONE,
    stopper: AppStopper = AppStopper.NONE,
    packageInstaller: PackageInstallerPort = PackageInstallerPort.NONE,
    expansionFiles: io.github.thisisthepy.dsa.agent.install.ExpansionFiles =
        io.github.thisisthepy.dsa.agent.install.ExpansionFiles.NONE,
    packages: PackageInfoPort = PackageInfoPort.NONE,
    googleDeviceId: GoogleDeviceIdPort = GoogleDeviceIdPort.NONE,
    packageUninstaller: PackageUninstallerPort = PackageUninstallerPort.NONE,
    packageReinstaller: PackageReinstallerPort = PackageReinstallerPort.NONE,
    private val crashRecords: CrashRecordSource = CrashRecordSource.NONE,
    arm64Supported: () -> Boolean = { false },
    widgets: WidgetPlatform = WidgetPlatform.NONE,
    appProcesses: AppProcesses = AppProcesses.NONE,
    widgetStore: WidgetStore = WidgetStore.memory(),
    accessibility: AccessibilitySource = AccessibilitySource.NONE,
    clock: () -> Long = { System.nanoTime() / 1_000_000 },
    diagnostics: DiagnosticsSource = DiagnosticsSource.NONE,
    diagnosticsRunner: (Runnable) -> Unit = { Thread(it, "dsa-diagnostics").start() },
    systemSettings: SystemSettingsPort = SystemSettingsPort.NONE,
    deviceName: DeviceNamePort = DeviceNamePort.NONE,
    wifi: WifiPort = WifiPort.NONE,
    dragCatcher: DragCatcher = DragCatcher.NONE,
    dragRunner: ((Runnable) -> Unit)? = null,
    private val gestureExclusion: GestureExclusion = GestureExclusion.NONE,
    private val unhandledKeys: UnhandledKeys = UnhandledKeys.NONE,
    private val editorFocused: () -> Boolean = { false },
) {
    private val network = NetworkSetup(wifi, log)
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
    private val pointer = PointerInput(injector, { taskId -> taskBounds[taskId] }, log) { taskId, keyCode, down ->
        // Keys belong to the scheduler's thread.
        scheduler.post {
            try {
                keys.navigationButton(taskId, keyCode, down)
            } catch (e: Exception) {
                log.warn("injecting a mouse's back or forward button failed", e)
            }
        }
    }
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
    private val keys = KeyInput(
        injector,
        reporter::isReported,
        reporter::reportedFocused,
        actions::focusTask,
        log,
        // A task the agent has not read yet counts as on its first screen, so a key never
        // takes it back past where it began.
        onFirstScreen = { taskId -> taskId !in pastFirstScreen },
        editorFocused = { editorFocused() },
    )
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
    private val settingsSync = SystemSettingsSync(systemSettings, log)
    private val deviceNameSync = DeviceNameSync(deviceName, log)
    private val diagnosticsCollector = DiagnosticsCollector(diagnostics, { frame -> channel.send(frame) }, log, diagnosticsRunner)
    private val backgroundApps = BackgroundApps(
        ownPackage,
        notifications,
        media,
        stopper,
        { frame -> channel.send(frame) },
        log,
        isConnected = { channel.connected },
    )
    private val notificationForwarder = NotificationForwarder(
        ownPackage,
        notifications,
        { frame -> channel.send(frame) },
        log,
        isConnected = { channel.connected },
        also = backgroundApps,
    )
    private val apkInstaller = ApkInstaller(
        packageInstaller,
        { frame -> channel.send(frame) },
        log,
        post = { work -> scheduler.post(work) },
        onInstalled = { requestAppsRefresh() },
        expansionFiles = expansionFiles,
    )
    private val printer = HostPrinter(
        { frame -> channel.send(frame) },
        log,
        isConnected = { channel.connected },
        post = { work -> scheduler.post(work) },
    )
    private val packageQueries = PackageQueries(packages, { frame -> channel.send(frame) }, log)
    private val googleDeviceIds = GoogleDeviceIds(googleDeviceId, { frame -> channel.send(frame) }, log)
    private val appUninstaller = AppUninstaller(
        ownPackage,
        packageUninstaller,
        { frame -> channel.send(frame) },
        log,
        post = { work -> scheduler.post(work) },
        onUninstalled = { requestAppsRefresh() },
    )
    private val appReinstaller = AppReinstaller(
        ownPackage,
        packageReinstaller,
        { frame -> channel.send(frame) },
        log,
        post = { work -> scheduler.post(work) },
        onReinstalled = { requestAppsRefresh() },
    )
    private val nativeCrashes = NativeLibraryCrashes(
        ownPackage,
        { frame -> channel.send(frame) },
        log,
        arm64Supported,
        clock,
    )
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
    private val captions = CaptionInsets(actions, log)
    private val installed = InstalledAppReporter(ownPackage) { frame -> channel.send(frame) }
    private val links = Links(browserRole, guestBrowser, { frame -> channel.send(frame) }, log)
    private val files = FileReceiver(fileStore, fileOpener, { frame -> channel.send(frame) }, log)
    private val sharedFolders = SharedFolders(sharedStorage, mediaScan, scheduler, log)
    private val dragThread by lazy {
        java.util.concurrent.Executors.newSingleThreadExecutor { work -> Thread(work, "dsa-drag-out") }
    }
    private val dragOut = DragOut(
        dragCatcher,
        releasePointer = { taskId, x, y, tool, pointerId ->
            // The pointer lifts where it left the window, as the host's own UP would, so the
            // app's drag drops on the agent's window and the input stays in step.
            pointerQueue.add(
                io.github.thisisthepy.dsa.control.v1.PointerEvent.newBuilder()
                    .setTaskId(taskId)
                    .setAction(PointerAction.POINTER_ACTION_UP)
                    .setTool(tool)
                    .setPointerId(pointerId)
                    .setX(x)
                    .setY(y)
                    .setButtons(0)
                    .setPressure(0f)
                    .build(),
            )
        },
        send = { frame -> channel.send(frame) },
        log = log,
        run = dragRunner ?: { work -> dragThread.execute(work) },
    )
    private val accessibilityExport = AccessibilityExport(
        accessibility,
        scheduler,
        { frame -> channel.send(frame) },
        log,
        reporter::reportedBounds,
        clock,
    )
    /** Where the apps on the display keep system gestures out, as last heard. */
    private var exclusion: List<TaskBounds> = emptyList()

    /** The tasks past their first screen, as last read: Back has somewhere to go in them. */
    private var pastFirstScreen: Set<Int> = emptySet()
    private var refreshPending = false
    private var appsPending = false
    private var appsForced = false
    private var checkPending = false

    fun start() {
        scheduler.post {
            freeform.start()
            closedApps.configure()
            try {
                inputMethods.useHostInputMethod()
            } catch (e: Exception) {
                log.warn("could not make the agent's input method the guest's; typed text may not reach editors", e)
            }
            try {
                network.joinHostNetwork()
            } catch (e: Exception) {
                log.warn("could not join the host's network; apps have no network until Wi-Fi joins it", e)
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
            backgroundApps.start { work -> scheduler.post(work) }
            widgetManager.start(later = { work -> scheduler.postDelayed(RETRY_MILLIS, work) }) { work ->
                scheduler.post(work)
            }
            listenToApps()
            listenToCrashes()
            listenToExclusion()
            listenToUnhandledKeys()
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

    /** Hears where apps keep system gestures out, which marks an app's controls in its title bar. */
    private fun listenToExclusion() {
        try {
            gestureExclusion.listen { rects ->
                scheduler.post {
                    if (rects != exclusion) {
                        exclusion = rects
                        requestRefresh()
                    }
                }
            }
        } catch (e: Exception) {
            log.warn("listening to the display's gesture exclusion failed; apps that draw their own title bar get no controls there", e)
        }
    }

    /** Hears the Esc presses no app took, which become Back past a task's first screen. */
    private fun listenToUnhandledKeys() {
        try {
            unhandledKeys.listen { scheduler.post { keys.escapeUnhandled() } }
        } catch (e: Exception) {
            log.warn("listening to the keys no app takes failed; Esc never goes back", e)
        }
    }

    /** Reports the apps that end because a native library did not load, while the host listens. */
    private fun listenToCrashes() {
        try {
            crashRecords.listen { tag, text ->
                scheduler.post { if (channel.connected) nativeCrashes.record(tag, text) }
            }
        } catch (e: Exception) {
            log.warn("listening to the system's crash records failed; crashes of apps are not reported", e)
        }
    }

    private fun requestAppsRefresh(force: Boolean = false) {
        if (force) appsForced = true
        if (appsPending) return
        appsPending = true
        scheduler.post {
            appsPending = false
            val forced = appsForced
            appsForced = false
            refreshApps(forced)
        }
    }

    /**
     * Tells the host which apps a launcher lists, when that changed; when [force]d, sends the
     * list even when nothing changed, since the host waits for it.
     */
    private fun refreshApps(force: Boolean) {
        if (!channel.connected) return
        val apps = try {
            launcherApps.list()
        } catch (e: Exception) {
            log.warn("reading the installed apps failed", e)
            return
        } ?: return
        installed.update(apps, force)
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

    /**
     * The focused editor is now [state], as the agent's input method read it. Called from any
     * thread.
     */
    fun editorChanged(state: io.github.thisisthepy.dsa.agent.input.EditorState) {
        scheduler.post {
            if (channel.connected) text.editorChanged(state)
        }
    }

    private fun onConnected() {
        // The daemon tells the host from this that the guest's requests have someone to reach.
        channel.send(Envelope.newBuilder().setAgentConnected(AgentConnected.getDefaultInstance()).build().toByteArray())
        reporter.resend()
        settingsSync.connected()
        deviceNameSync.connected()
        lifecycle.connected()
        clipboardSync.connected()
        notificationForwarder.connected()
        backgroundApps.connected()
        widgetManager.connected()
        apkInstaller.connected()
        printer.connected()
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
        if (settingsSync.handle(frame)) return
        if (deviceNameSync.handle(frame)) return
        if (linksOrFiles(frame)) return
        val result = try {
            requests.handle(frame)
        } catch (e: Exception) {
            log.warn("acting on a frame from the channel failed", e)
            return
        }
        when (result) {
            is RequestResult.Launched ->
                log.info(if (result.newWindow) "opened a new window of ${result.packageName}" else "launched ${result.packageName}")
            is RequestResult.LaunchFailed -> log.warn("could not launch ${result.packageName}: ${result.reason}")
            is RequestResult.SettingsOpened ->
                log.info("opened the ${result.page.name.lowercase()} settings of ${result.packageName ?: "every app"}")
            is RequestResult.SettingsFailed ->
                log.warn("could not open the settings of ${result.packageName ?: "every app"}: ${result.reason}")
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
            is RequestResult.Caption -> {
                log.info("task ${result.taskId} is under a title bar ${result.height} pixels high")
                captions.setHeight(
                    result.taskId,
                    result.height,
                    reporter.reportedBounds(result.taskId),
                    result.startWidth,
                    result.endWidth,
                )
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
        if (sharedFolders.handle(envelope)) return true
        if (clipboardSync.handle(envelope)) return true
        if (notificationForwarder.handle(envelope)) return true
        if (backgroundApps.handle(envelope)) return true
        if (apkInstaller.handle(envelope)) return true
        if (packageQueries.handle(envelope)) return true
        if (googleDeviceIds.handle(envelope)) return true
        if (dragOut.handle(envelope)) return true
        if (printer.handle(envelope)) return true
        if (appUninstaller.handle(envelope)) return true
        if (appReinstaller.handle(envelope)) return true
        when (envelope.bodyCase) {
            Envelope.BodyCase.REFRESH_APPS -> requestAppsRefresh(force = true)
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
            Envelope.BodyCase.IME_DELETE_SURROUNDING -> try {
                text.handle(envelope.imeDeleteSurrounding)
            } catch (e: Exception) {
                log.warn("deleting text around the cursor failed", e)
            }
            Envelope.BodyCase.IME_SET_SELECTION -> try {
                text.handle(envelope.imeSetSelection)
            } catch (e: Exception) {
                log.warn("selecting text in the editor failed", e)
            }
            Envelope.BodyCase.IME_SET_COMPOSING_REGION -> try {
                text.handle(envelope.imeSetComposingRegion)
            } catch (e: Exception) {
                log.warn("making text the composition again failed", e)
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

    /**
     * The print service was given [job]: it goes to the host, which prints it. Called from any
     * thread.
     */
    fun print(job: QueuedPrintJob) {
        scheduler.post { printer.queued(job) }
    }

    /** Android canceled a job the print service was given. Called from any thread. */
    fun cancelPrint(job: QueuedPrintJob) {
        scheduler.post { printer.cancelRequested(job) }
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
        val placedTasks = running.map { task ->
            val placed = bounds[task.taskId]
            val moved = if (placed == null || placed == task.bounds) task else task.copy(bounds = placed)
            val controls = captions.controls(moved.taskId, moved.bounds, moved.transparentCaption, exclusion)
            if (controls.isEmpty()) moved else moved.copy(captionInteractive = controls)
        }
        reporter.update(placedTasks, arranger.display)
        pastFirstScreen = placedTasks.filter { it.canGoBack }.map { it.taskId }.toSet()
        val reported = reporter.reported
        val told = reported.mapNotNull { taskId -> reporter.reportedBounds(taskId)?.let { taskId to it } }.toMap()
        captions.update(told, placedTasks.filter { it.transparentCaption }.map { it.taskId }.toSet())
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

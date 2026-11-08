package io.github.thisisthepy.dsa.agent.platform

import android.app.Application
import android.os.HandlerThread
import android.os.Process
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.trace.FrameStatsTrace
import io.github.thisisthepy.dsa.agent.trace.InputTrace
import io.github.thisisthepy.dsa.agent.trace.SurfaceFlingerTrace

/**
 * The agent's process. The activity manager starts it at boot because it is persistent, and
 * starts it again if it dies; the agent runs on a thread of its own from here.
 */
class AgentApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        val thread = HandlerThread("dsa-agent").apply { start() }
        // Pointer input has a thread of its own, at the priority of the framework's input
        // threads, so it waits neither behind the agent's binder calls nor behind apps.
        val input = HandlerThread("dsa-input", Process.THREAD_PRIORITY_URGENT_DISPLAY).apply { start() }
        val log = AndroidLog
        val traced = inputTraceRequested()
        // Reading an app's frame timings is a binder call that can take tens of milliseconds,
        // so it has a thread of its own, made only when the trace is on.
        val frameTrace = if (traced) {
            val reads = HandlerThread("dsa-frame-trace").apply { start() }
            val surfaceFlinger = SurfaceFlingerTrace(
                log,
                list = AndroidFrameStats::surfaceFlingerLayers,
                latency = AndroidFrameStats::surfaceFlingerLatency,
            )
            FrameStatsTrace(
                log,
                enabled = true,
                scheduler = HandlerScheduler(reads.looper),
                dump = AndroidFrameStats::dump,
                surfaceFlinger = surfaceFlinger,
            )
        } else {
            null
        }
        val dragCatcher = AndroidDragCatcher(this, log)
        val installer = AndroidPackageInstaller(this, log)
        val uninstaller = AndroidPackageUninstaller(this, log)
        val agent = Agent(
            ownPackage = packageName,
            tasks = AndroidTaskSource(this, log),
            actions = AndroidTaskActions(this, log),
            locator = BinderChannelLocator(log),
            windowing = AndroidWindowing(this),
            injector = AndroidInputInjector(this),
            scheduler = HandlerScheduler(thread.looper),
            log = log,
            apps = AndroidAppInfoSource(this, log),
            editor = { HostInputMethod.connection },
            editorFocused = { HostInputMethod.editorFocused },
            gestureExclusion = AndroidGestureExclusion(log),
            unhandledKeys = AndroidUnhandledKeys(this, log),
            textKeys = AndroidTextKeys(),
            inputMethods = AndroidInputMethodSetup(this),
            power = AndroidDevicePower(this, log),
            // `setprop log.tag.DsaResizeTrace D` turns on the per-stage resize trace lines.
            trace = io.github.thisisthepy.dsa.agent.trace.ResizeTrace(log, detailed = {
                android.util.Log.isLoggable("DsaResizeTrace", android.util.Log.DEBUG)
            }),
            launcherApps = AndroidLauncherApps(this, log),
            memory = AndroidMemorySource(this),
            inputScheduler = HandlerScheduler(input.looper),
            inputTrace = InputTrace(log, enabled = traced),
            frameTrace = frameTrace,
            touchSounds = AndroidTouchSounds(this),
            systemSettings = AndroidSystemSettings(this),
            deviceName = AndroidDeviceName(this),
            browserRole = AndroidBrowserRole(this, log),
            guestBrowser = AndroidGuestBrowser(this, log),
            fileStore = io.github.thisisthepy.dsa.agent.files.TransferStore(hostFilesDir(this)),
            fileOpener = AndroidFileOpener(this),
            mediaScan = AndroidMediaScan(this),
            clipboard = AndroidClipboard(this, log),
            notifications = AndroidNotifications(this, log),
            media = AndroidMediaSessions(this, log, android.os.Handler(thread.looper)),
            stopper = AndroidAppStopper(this, log),
            packageInstaller = installer,
            expansionFiles = AndroidExpansionFiles(
                java.io.File(cacheDir, "expansion-files"),
                @Suppress("DEPRECATION") android.os.Environment.getExternalStorageDirectory(),
                log,
            ),
            packageUninstaller = uninstaller,
            packageReinstaller = AndroidPackageReinstaller(this, installer, uninstaller, AndroidApks()),
            crashRecords = AndroidCrashRecords(this, log),
            arm64Supported = { "arm64-v8a" in android.os.Build.SUPPORTED_64_BIT_ABIS },
            packages = AndroidPackageInfo(this),
            googleDeviceId = AndroidGoogleDeviceId(this),
            widgets = AndroidWidgets(this, log),
            appProcesses = AndroidAppProcesses(this),
            widgetStore = AndroidWidgetStore(this),
            accessibility = AndroidAccessibilitySource(this, log),
            diagnostics = AndroidDiagnostics(this),
            wifi = AndroidWifi(this),
            dragCatcher = dragCatcher,
        )
        AgentApplication.agent = agent
        HostInputMethod.onCursor = { rect -> agent.cursorMoved(rect.left, rect.top, rect.right, rect.bottom) }
        HostInputMethod.onEditor = { state -> agent.editorChanged(state) }
        agent.start()
        dragCatcher.start()
        // Starting windows draw on a thread of their own, so a slow app theme never holds up
        // the agent's work or the home activity on the main thread.
        val splash = HandlerThread("dsa-splash").apply { start() }
        AndroidStartingWindows(this, splash.looper, log).start()
    }

    companion object {
        /** The running agent, for the link activity, which runs in the same process. */
        @Volatile
        var agent: Agent? = null
            private set
    }

    /** SystemProperties is not in the SDK and is called by reflection; off when it fails. */
    private fun inputTraceRequested(): Boolean = try {
        Class.forName("android.os.SystemProperties")
            .getMethod("get", String::class.java)
            .invoke(null, InputTrace.PROPERTY) == "1"
    } catch (e: ReflectiveOperationException) {
        false
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.app.Application
import android.os.HandlerThread
import android.os.Process
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.trace.InputTrace

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
            inputTrace = InputTrace(log, enabled = inputTraceRequested()),
        )
        HostInputMethod.onCursor = { rect -> agent.cursorMoved(rect.left, rect.top, rect.right, rect.bottom) }
        agent.start()
        // Starting windows draw on a thread of their own, so a slow app theme never holds up
        // the agent's work or the home activity on the main thread.
        val splash = HandlerThread("dsa-splash").apply { start() }
        AndroidStartingWindows(this, splash.looper, log).start()
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

package io.github.thisisthepy.dsa.agent.platform

import android.app.Application
import android.os.HandlerThread
import io.github.thisisthepy.dsa.agent.Agent

/**
 * The agent's process. The activity manager starts it at boot because it is persistent, and
 * starts it again if it dies; the agent runs on a thread of its own from here.
 */
class AgentApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        val thread = HandlerThread("dsa-agent").apply { start() }
        val log = AndroidLog
        Agent(
            ownPackage = packageName,
            tasks = AndroidTaskSource(this, log),
            actions = AndroidTaskActions(this, log),
            locator = BinderChannelLocator(log),
            windowing = AndroidWindowing(this),
            injector = AndroidInputInjector(this),
            scheduler = HandlerScheduler(thread.looper),
            log = log,
            apps = AndroidAppInfoSource(this, log),
            // `setprop log.tag.DsaResizeTrace D` turns on the per-stage resize trace lines.
            trace = io.github.thisisthepy.dsa.agent.trace.ResizeTrace(log, detailed = {
                android.util.Log.isLoggable("DsaResizeTrace", android.util.Log.DEBUG)
            }),
        ).start()
        // Starting windows draw on a thread of their own, so a slow app theme never holds up
        // the agent's work or the home activity on the main thread.
        val splash = HandlerThread("dsa-splash").apply { start() }
        AndroidStartingWindows(this, splash.looper, log).start()
    }
}

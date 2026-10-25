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
            scheduler = HandlerScheduler(thread.looper),
            log = log,
        ).start()
    }
}

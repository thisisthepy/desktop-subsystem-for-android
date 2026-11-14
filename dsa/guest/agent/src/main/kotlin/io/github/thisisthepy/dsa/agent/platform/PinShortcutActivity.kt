package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.os.Bundle

/**
 * Where Android sends an app's request to pin a shortcut (requestPinShortcut), since the
 * agent is the guest's default launcher. It shows nothing: it hands the request to the agent,
 * which asks the host, and the host asks the user; it finishes at once, before a window is
 * drawn. A request that arrives before the agent started, or that is not for a shortcut,
 * lapses, and the app's shortcut is not pinned.
 */
class PinShortcutActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val agent = AgentApplication.agent
        val request = AndroidShortcuts(this, AndroidLog).pinRequest(intent)
        when {
            request == null -> AndroidLog.warn("an app asked to pin something that is not a shortcut, or Android withdrew it")
            agent == null -> AndroidLog.warn("an app asked to pin a shortcut before the agent started; the request lapses")
            else -> agent.pinRequested(request)
        }
        finish()
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.os.Bundle

/**
 * The guest's browser, while web links go to the host. It shows nothing: it hands the link to
 * the agent, which sends it to the host (or opens it in a browser in the guest when links stay
 * there), and finishes at once, before a window is drawn.
 */
class LinkActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val url = intent?.dataString
        val agent = AgentApplication.agent
        if (agent == null) {
            AndroidLog.warn("a link arrived before the agent started; opening it in the guest")
            if (url != null) AndroidGuestBrowser(this, AndroidLog).open(url)
        } else {
            agent.openLink(url)
        }
        finish()
    }
}

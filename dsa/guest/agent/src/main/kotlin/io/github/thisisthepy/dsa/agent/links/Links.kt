package io.github.thisisthepy.dsa.agent.links

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.OpenLink

/** Where a web link an app opened goes. */
sealed interface LinkDecision {
    /** To the host, which opens it in the user's default browser. */
    data class ToHost(val url: String) : LinkDecision

    /** To a browser in the guest, as on a phone. */
    data class InGuest(val url: String) : LinkDecision

    /** Nowhere: it is not a web link. */
    data class Refused(val reason: String) : LinkDecision
}

/** The guest's browser role, which the agent holds while links go to the host. */
interface BrowserRole {
    /** Makes the agent the guest's browser. */
    fun take()

    /** Gives the browser role back to a browser app in the guest, if there is one. */
    fun giveBack()

    companion object {
        val NONE = object : BrowserRole {
            override fun take() = Unit

            override fun giveBack() = Unit
        }
    }
}

/** Opens a link in a browser app in the guest. */
fun interface GuestBrowser {
    fun open(url: String)

    companion object {
        val NONE = GuestBrowser { }
    }
}

/**
 * Sends the web links apps in the guest open to the host, or lets them open in the guest.
 *
 * While links go to the host (the default, until the host says otherwise with LinkHandling),
 * the agent holds the guest's browser role, so Android hands it every http and https link no
 * installed app has verified as its own; a verified link (an App Link) still opens in its
 * app, which Android decides by itself. A link that cannot reach the host (no connection)
 * opens in the guest instead, so a tap is never lost. Anything but an http or https link is
 * refused: the host opens links with its shell, and must never be handed anything else.
 *
 * Every method runs on the agent's scheduler thread.
 */
class Links(
    private val role: BrowserRole,
    private val browser: GuestBrowser,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
) {
    /** Whether links open in the guest, as the host last said. */
    var inGuest = false
        private set

    /** A new connection to the host: links go to the host until it says otherwise. */
    fun connected() {
        inGuest = false
        applyRole()
    }

    /** Takes the host's word on where links go. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.LINK_HANDLING) return false
        val wanted = envelope.linkHandling.inGuest
        if (wanted != inGuest) {
            inGuest = wanted
            log.info(if (wanted) "web links open in the guest" else "web links open on the host")
            applyRole()
        }
        return true
    }

    /** Sends or opens a link an app opened through the agent's browser activity. */
    fun open(url: String?, connected: Boolean): LinkDecision {
        val decision = decide(url, connected, inGuest)
        when (decision) {
            is LinkDecision.ToHost -> {
                val frame = Envelope.newBuilder().setOpenLink(OpenLink.newBuilder().setUrl(decision.url)).build()
                if (!send(frame.toByteArray())) {
                    log.warn("the host did not take a link; opening it in the guest")
                    openInGuest(decision.url)
                    return LinkDecision.InGuest(decision.url)
                }
            }
            is LinkDecision.InGuest -> openInGuest(decision.url)
            is LinkDecision.Refused -> log.warn("dropped a link: ${decision.reason}")
        }
        return decision
    }

    private fun openInGuest(url: String) {
        try {
            browser.open(url)
        } catch (e: Exception) {
            log.warn("no browser in the guest opened a link", e)
        }
    }

    private fun applyRole() {
        try {
            if (inGuest) role.giveBack() else role.take()
        } catch (e: Exception) {
            log.warn("could not change the guest's browser; links may not go where the user chose", e)
        }
    }

    companion object {
        /**
         * The longest link that is sent to the host, in characters. The host hands a link to
         * its browser on a command line, which holds at most 32767 characters.
         */
        const val MAX_URL_LENGTH = 32_000

        /** Where a link goes: the host, the guest, or nowhere. */
        fun decide(url: String?, connected: Boolean, inGuest: Boolean): LinkDecision {
            val web = webUrl(url) ?: return LinkDecision.Refused("not an http or https link")
            return if (inGuest || !connected) LinkDecision.InGuest(web) else LinkDecision.ToHost(web)
        }

        /** The link without the spaces around it if it is an http or https link with a host. */
        fun webUrl(url: String?): String? {
            val link = url?.trim() ?: return null
            if (link.isEmpty() || link.length > MAX_URL_LENGTH) return null
            if (link.any { it.isWhitespace() || it.isISOControl() }) return null
            val colon = link.indexOf(':')
            if (colon <= 0) return null
            val scheme = link.substring(0, colon)
            if (!scheme.equals("http", ignoreCase = true) && !scheme.equals("https", ignoreCase = true)) return null
            val rest = link.substring(colon + 1)
            if (!rest.startsWith("//")) return null
            val authority = rest.substring(2).split('/', '?', '#').first()
            val host = authority.substringAfterLast('@')
            if (host.isEmpty() || host.startsWith(":") || host.contains('\\')) return null
            return link
        }
    }
}

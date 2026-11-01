package io.github.thisisthepy.dsa.agent.links

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.LinkHandling
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's browser role: records who the agent made the browser. */
class FakeRole : BrowserRole {
    val calls = mutableListOf<String>()

    override fun take() {
        calls += "take"
    }

    override fun giveBack() {
        calls += "giveBack"
    }
}

class LinksTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val role = FakeRole()
    private val opened = mutableListOf<String>()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            browserRole = role,
            guestBrowser = GuestBrowser { opened += it },
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun linksStayInGuest(inGuest: Boolean) {
        daemon.deliver(
            Envelope.newBuilder().setLinkHandling(LinkHandling.newBuilder().setInGuest(inGuest)).build().toByteArray(),
        )
        scheduler.runDue()
    }

    private fun linksSent(): List<String> = daemon.sentMessages().filter { it.hasOpenLink() }.map { it.openLink.url }

    @Test
    fun fr9_a_web_link_an_app_opens_goes_to_the_host() {
        val agent = started()
        assertEquals("the agent becomes the browser when it connects", listOf("take"), role.calls)

        agent.openLink("https://example.com/news?id=1")
        scheduler.runDue()

        assertEquals(listOf("https://example.com/news?id=1"), linksSent())
        assertEquals(emptyList<String>(), opened)
    }

    @Test
    fun fr9_links_the_user_keeps_in_android_open_in_a_guest_browser() {
        val agent = started()
        linksStayInGuest(true)
        assertEquals(listOf("take", "giveBack"), role.calls)

        agent.openLink("https://example.com/")
        scheduler.runDue()

        assertEquals(emptyList<String>(), linksSent())
        assertEquals(listOf("https://example.com/"), opened)

        linksStayInGuest(false)
        assertEquals(listOf("take", "giveBack", "take"), role.calls)
    }

    @Test
    fun fr9_a_new_connection_sends_links_to_the_host_again() {
        started()
        linksStayInGuest(true)
        daemon.die()
        val again = FakeEndpoint()
        locator.up = again
        scheduler.advance(2000)
        assertEquals(listOf("take", "giveBack", "take"), role.calls)
    }

    @Test
    fun fr9_only_http_and_https_links_are_sent_and_the_rest_dropped() {
        val agent = started()
        for (url in listOf("file:///data/system/packages.xml", "intent://x#Intent;end", "javascript:alert(1)", "https://", null)) {
            agent.openLink(url)
        }
        scheduler.runDue()
        assertEquals(emptyList<String>(), linksSent())
        assertEquals(emptyList<String>(), opened)
        assertTrue(log.lines.any { it.contains("dropped a link") })
    }

    @Test
    fun fr9_the_forwarding_decision_follows_the_connection_and_the_setting() {
        val url = "HTTPS://Example.com/a b".replace(" ", "")
        assertEquals(LinkDecision.ToHost(url), Links.decide(url, connected = true, inGuest = false))
        assertEquals(LinkDecision.InGuest(url), Links.decide(url, connected = true, inGuest = true))
        assertEquals(
            "a link that cannot reach the host is not lost",
            LinkDecision.InGuest(url),
            Links.decide(url, connected = false, inGuest = false),
        )
        assertTrue(Links.decide("ftp://example.com", connected = true, inGuest = false) is LinkDecision.Refused)
        assertNull(Links.webUrl("http://\\\\server\\share"))
        assertNull(Links.webUrl("https://example.com/" + "a".repeat(Links.MAX_URL_LENGTH)))
        assertEquals("http://example.com", Links.webUrl("  http://example.com "))
    }

    @Test
    fun fr9_a_link_the_host_cannot_take_opens_in_the_guest() {
        val agent = started()
        daemon.dead = true
        agent.openLink("https://example.com/")
        scheduler.runDue()
        assertEquals(listOf("https://example.com/"), opened)
    }
}

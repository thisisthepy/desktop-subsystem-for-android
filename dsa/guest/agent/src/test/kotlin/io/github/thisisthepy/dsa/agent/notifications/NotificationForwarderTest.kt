package io.github.thisisthepy.dsa.agent.notifications

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
import io.github.thisisthepy.dsa.control.v1.NotificationClicked
import io.github.thisisthepy.dsa.control.v1.NotificationDismissed
import io.github.thisisthepy.dsa.control.v1.ScreenState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's notifications: what is shown, and what clicking and cancelling did. */
class FakeNotifications : NotificationSource {
    val shown = linkedMapOf<String, GuestNotice>()
    val opened = mutableListOf<String>()
    val cancelled = mutableListOf<String>()
    var listener: NoticeListener? = null

    override fun listen(listener: NoticeListener) {
        this.listener = listener
    }

    override fun active() = shown.values.toList()

    override fun open(key: String): Boolean {
        if (key !in shown) return false
        opened += key
        return true
    }

    override fun cancel(key: String): Boolean {
        if (shown.remove(key) == null) return false
        cancelled += key
        listener?.removed(key)
        return true
    }

    /** An app posts [notice], or posts it again. */
    fun post(notice: GuestNotice) {
        shown[notice.key] = notice
        listener?.posted(notice)
    }

    /** An app removes the notification with [key]. */
    fun remove(key: String) {
        shown.remove(key)
        listener?.removed(key)
    }
}

const val CHAT = "0|com.example.chat|1|null|10100"

fun notice(
    key: String = CHAT,
    packageName: String = "com.example.chat",
    title: String = "Ann",
    text: String = "see you at noon",
    importance: Int = 3,
    priority: Int = 0,
    ongoing: Boolean = false,
    foregroundService: Boolean = false,
    groupSummary: Boolean = false,
    clearable: Boolean = true,
    icon: ByteArray = byteArrayOf(0x89.toByte(), 'P'.code.toByte(), 'N'.code.toByte(), 'G'.code.toByte()),
) = GuestNotice(
    key = key,
    packageName = packageName,
    appLabel = "Chat",
    title = title,
    text = text,
    iconPng = icon,
    importance = importance,
    priority = priority,
    ongoing = ongoing,
    foregroundService = foregroundService,
    groupSummary = groupSummary,
    clearable = clearable,
    whenMillis = 1_700_000_000_000,
)

class NotificationForwarderTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val notifications = FakeNotifications()

    private fun started(connected: Boolean = true): Agent {
        if (connected) locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            notifications = notifications,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun sentNotifications() = daemon.sentMessages().filter { it.hasNotification() }.map { it.notification }

    private fun sentRemovals() = daemon.sentMessages().filter { it.hasNotificationRemoved() }.map { it.notificationRemoved.key }

    private fun fromHost(envelope: Envelope) {
        daemon.deliver(envelope.toByteArray())
        scheduler.runDue()
    }

    @Test
    fun fr8_a_posted_notification_reaches_the_host_with_its_content() {
        started()
        notifications.post(notice(importance = 4))
        scheduler.runDue()
        val sent = sentNotifications().single()
        assertEquals(CHAT, sent.key)
        assertEquals("com.example.chat", sent.packageName)
        assertEquals("Chat", sent.appLabel)
        assertEquals("Ann", sent.title)
        assertEquals("see you at noon", sent.text)
        assertEquals(4, sent.iconPng.size())
        assertEquals(4, sent.importance)
        assertTrue(sent.highPriority)
        assertTrue(sent.clearable)
        assertEquals(1_700_000_000_000, sent.whenMillis)
    }

    @Test
    fun fr8_high_priority_follows_the_channel_or_the_old_priority() {
        val forwarder = NotificationForwarder(AGENT, notifications, { true }, log) { true }
        assertFalse(NotificationForwarder.highPriority(notice(importance = 3)))
        assertTrue(NotificationForwarder.highPriority(notice(importance = 4)))
        assertTrue(NotificationForwarder.highPriority(notice(importance = 3, priority = 1)))
        assertTrue(forwarder.forwardable(notice()))
    }

    @Test
    fun fr8_an_update_replaces_and_a_removal_removes_on_the_host() {
        started()
        notifications.post(notice(title = "Ann"))
        notifications.post(notice(title = "Ann (2 messages)"))
        notifications.remove(CHAT)
        scheduler.runDue()
        assertEquals(listOf("Ann", "Ann (2 messages)"), sentNotifications().map { it.title })
        assertEquals(listOf(CHAT), sentRemovals())
    }

    @Test
    fun fr8_summaries_foreground_services_minimum_importance_empty_and_own_notifications_stay_in_the_guest() {
        started()
        notifications.post(notice(key = "summary", groupSummary = true))
        notifications.post(notice(key = "service", ongoing = true, foregroundService = true))
        notifications.post(notice(key = "min", importance = 1))
        notifications.post(notice(key = "empty", title = "", text = " "))
        notifications.post(notice(key = "own", packageName = AGENT))
        // An ongoing notification that is not a service's, such as a call, does go.
        notifications.post(notice(key = "call", ongoing = true, clearable = false))
        scheduler.runDue()
        assertEquals(listOf("call"), sentNotifications().map { it.key })
        assertTrue(sentRemovals().isEmpty())
    }

    @Test
    fun fr8_a_notification_that_stops_being_shown_on_the_host_is_removed_there() {
        started()
        notifications.post(notice())
        notifications.post(notice(groupSummary = true))
        scheduler.runDue()
        assertEquals(listOf(CHAT), sentRemovals())
    }

    @Test
    fun fr8_a_removal_of_a_notification_the_host_never_had_is_not_sent() {
        started()
        notifications.post(notice(key = "summary", groupSummary = true))
        notifications.remove("summary")
        scheduler.runDue()
        assertTrue(sentRemovals().isEmpty())
    }

    @Test
    fun fr8_on_connecting_the_host_hears_of_every_notification_shown() {
        notifications.shown[CHAT] = notice()
        notifications.shown["summary"] = notice(key = "summary", groupSummary = true)
        started()
        assertEquals(listOf(CHAT), sentNotifications().map { it.key })
    }

    @Test
    fun fr8_nothing_is_sent_while_the_agent_is_not_connected() {
        started(connected = false)
        notifications.post(notice())
        scheduler.runDue()
        assertTrue(daemon.sent.isEmpty())
    }

    @Test
    fun fr8_a_click_on_the_host_opens_the_notification() {
        started()
        notifications.post(notice())
        scheduler.runDue()
        fromHost(Envelope.newBuilder().setNotificationClicked(NotificationClicked.newBuilder().setKey(CHAT)).build())
        assertEquals(listOf(CHAT), notifications.opened)
        assertTrue(sentRemovals().isEmpty())
    }

    @Test
    fun fr8_a_click_while_dozing_runs_after_the_screen_is_on() {
        started()
        notifications.post(notice(importance = 4))
        scheduler.runDue()
        // The host sends the screen's word before the click, on the same channel; the agent
        // handles them in order on its one thread.
        daemon.deliver(Envelope.newBuilder().setScreenState(ScreenState.newBuilder().setOn(true)).build().toByteArray())
        daemon.deliver(Envelope.newBuilder().setNotificationClicked(NotificationClicked.newBuilder().setKey(CHAT)).build().toByteArray())
        scheduler.runDue()
        val screenOn = log.lines.indexOfFirst { it.contains("screen on") }
        assertTrue("the screen came on", screenOn >= 0)
        assertEquals(listOf(CHAT), notifications.opened)
    }

    @Test
    fun fr8_a_dismissal_on_the_host_cancels_the_notification_in_the_guest() {
        started()
        notifications.post(notice())
        scheduler.runDue()
        fromHost(Envelope.newBuilder().setNotificationDismissed(NotificationDismissed.newBuilder().setKey(CHAT)).build())
        assertEquals(listOf(CHAT), notifications.cancelled)
        // The cancellation removes it on the host too, once.
        assertEquals(listOf(CHAT), sentRemovals())
    }

    @Test
    fun fr8_acting_on_a_notification_the_guest_no_longer_has_removes_it_on_the_host() {
        started()
        fromHost(Envelope.newBuilder().setNotificationClicked(NotificationClicked.newBuilder().setKey("gone")).build())
        fromHost(Envelope.newBuilder().setNotificationDismissed(NotificationDismissed.newBuilder().setKey("gone too")).build())
        assertEquals(listOf("gone", "gone too"), sentRemovals())
        assertTrue(notifications.opened.isEmpty())
    }

    @Test
    fun nfr5_a_source_that_throws_does_not_stop_the_agent() {
        val throwing = object : NotificationSource by notifications {
            override fun open(key: String): Boolean = throw IllegalStateException("binder died")
        }
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            notifications = throwing,
        )
        agent.start()
        scheduler.runDue()
        fromHost(Envelope.newBuilder().setNotificationClicked(NotificationClicked.newBuilder().setKey(CHAT)).build())
        assertTrue(log.lines.any { it.startsWith("warn: could not open a notification") })
    }

    @Test
    fun nfr5_an_icon_past_the_limit_is_left_out_and_the_notification_still_goes() {
        started()
        notifications.post(notice(icon = ByteArray(NotificationForwarder.MAX_ICON_BYTES + 1)))
        notifications.post(notice(key = "long", text = "x".repeat(NotificationForwarder.MAX_TEXT_CHARS + 10)))
        scheduler.runDue()
        val sent = sentNotifications()
        assertEquals(0, sent[0].iconPng.size())
        assertEquals(NotificationForwarder.MAX_TEXT_CHARS, sent[1].text.length)
    }
}

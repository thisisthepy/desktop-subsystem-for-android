package io.github.thisisthepy.dsa.agent.notifications

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.GuestNotification
import io.github.thisisthepy.dsa.control.v1.NotificationRemoved

/** One notification posted in the guest, as the agent reads it. */
data class GuestNotice(
    val key: String,
    val packageName: String,
    val appLabel: String,
    val title: String,
    val text: String,
    /** A PNG image of at most [NotificationForwarder.ICON_PIXELS] on a side; empty for none. */
    val iconPng: ByteArray,
    /** The importance of its channel, as NotificationManager numbers it. */
    val importance: Int,
    /** Notification.priority, which apps from before channels still set. */
    val priority: Int,
    val ongoing: Boolean,
    val foregroundService: Boolean,
    val groupSummary: Boolean,
    val clearable: Boolean,
    val whenMillis: Long,
) {
    override fun equals(other: Any?): Boolean =
        other is GuestNotice && key == other.key && packageName == other.packageName &&
            appLabel == other.appLabel && title == other.title && text == other.text &&
            iconPng.contentEquals(other.iconPng) && importance == other.importance &&
            priority == other.priority && ongoing == other.ongoing &&
            foregroundService == other.foregroundService && groupSummary == other.groupSummary &&
            clearable == other.clearable && whenMillis == other.whenMillis

    override fun hashCode(): Int = key.hashCode()
}

/** Hears of the guest's notifications, on any thread. */
interface NoticeListener {
    /** The agent may read the notifications now: it is told of each one shown again. */
    fun connected()

    /** A notification was posted, or posted again with new content. */
    fun posted(notice: GuestNotice)

    /** The notification with [key] is gone. */
    fun removed(key: String)
}

/** The guest's notifications, as the agent reaches them. */
interface NotificationSource {
    /** Starts listening; the framework calls [listener] from then on. */
    fun listen(listener: NoticeListener)

    /** The notifications shown now; empty while the agent cannot read them. */
    fun active(): List<GuestNotice>

    /** Runs the notification's content intent, as clicking it does. False when there is no such notification. */
    fun open(key: String): Boolean

    /** Cancels the notification. False when there is no such notification. */
    fun cancel(key: String): Boolean

    companion object {
        val NONE = object : NotificationSource {
            override fun listen(listener: NoticeListener) = Unit
            override fun active(): List<GuestNotice> = emptyList()
            override fun open(key: String) = false
            override fun cancel(key: String) = false
        }
    }
}

/**
 * Sends the guest's notifications to the host, and acts on what the user does with them
 * there. The guest has no SystemUI: the host's notification centre is where they are seen.
 *
 * Only notifications a person would look at go ([forwardable]). A notification that is
 * posted again with the same key replaces the host's; one that stops being forwardable is
 * removed there. When the agent connects, the host is told of every notification shown.
 *
 * A click from the host runs the notification's content intent; a dismissal cancels it. A key
 * the guest no longer has is answered with its removal, so the host never keeps a notification
 * that does nothing.
 *
 * Every method but [start]'s listener runs on the agent's one thread.
 */
class NotificationForwarder(
    private val ownPackage: String,
    private val source: NotificationSource,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val isConnected: () -> Boolean,
) {
    /** The keys the host has, so a removal is sent only for what it was told of. */
    private val forwarded = mutableSetOf<String>()

    /** Starts listening; [post] runs work on the agent's thread. */
    fun start(post: (() -> Unit) -> Unit) {
        try {
            source.listen(object : NoticeListener {
                override fun connected() = post(::resend)
                override fun posted(notice: GuestNotice) = post { posted(notice) }
                override fun removed(key: String) = post { removed(key) }
            })
        } catch (e: Exception) {
            log.warn("cannot read Android's notifications; none reach the host", e)
        }
    }

    /** A new connection: the host has none of the guest's notifications that it can act on. */
    fun connected() = resend()

    private fun resend() {
        if (!isConnected()) return
        forwarded.clear()
        val active = try {
            source.active()
        } catch (e: Exception) {
            log.warn("reading Android's notifications failed", e)
            return
        }
        for (notice in active) posted(notice)
    }

    /** Sends a posted notification to the host, or removes it there when it should not show. */
    fun posted(notice: GuestNotice) {
        if (!isConnected()) return
        if (!forwardable(notice)) {
            if (notice.key in forwarded) removed(notice.key)
            return
        }
        val icon = if (notice.iconPng.size > MAX_ICON_BYTES) {
            log.warn("left out the icon of a notification of ${notice.packageName}: ${notice.iconPng.size} bytes")
            ByteArray(0)
        } else {
            notice.iconPng
        }
        val message = GuestNotification.newBuilder()
            .setKey(notice.key)
            .setPackageName(notice.packageName)
            .setAppLabel(notice.appLabel)
            .setTitle(notice.title.take(MAX_LINE_CHARS))
            .setText(notice.text.take(MAX_TEXT_CHARS))
            .setIconPng(ByteString.copyFrom(icon))
            .setImportance(notice.importance)
            .setHighPriority(highPriority(notice))
            .setClearable(notice.clearable)
            .setWhenMillis(notice.whenMillis)
        if (send(Envelope.newBuilder().setNotification(message).build().toByteArray())) forwarded += notice.key
    }

    /** The notification is gone from the guest; it goes from the host too. */
    fun removed(key: String) {
        if (!forwarded.remove(key)) return
        sendRemoved(key)
    }

    private fun sendRemoved(key: String) {
        send(Envelope.newBuilder().setNotificationRemoved(NotificationRemoved.newBuilder().setKey(key)).build().toByteArray())
    }

    /** Acts on the frame if it is the user's click or dismissal on the host. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.NOTIFICATION_CLICKED -> act(envelope.notificationClicked.key, "open") { source.open(it) }
            Envelope.BodyCase.NOTIFICATION_DISMISSED -> act(envelope.notificationDismissed.key, "cancel") { source.cancel(it) }
            else -> return false
        }
        return true
    }

    private fun act(key: String, what: String, action: (String) -> Boolean) {
        val done = try {
            action(key)
        } catch (e: Exception) {
            log.warn("could not $what a notification the host asked about", e)
            return
        }
        if (!done) {
            log.info("the host asked to $what a notification Android no longer has; removing it there")
            forwarded.remove(key)
            sendRemoved(key)
        }
    }

    /**
     * Whether a person would look at the notification on the host:
     *
     * - Not the agent's own.
     * - Not a group's summary. Android shows a group as its summary with the members under
     *   it; the host's notification centre shows each notification on its own, so the
     *   summary ("3 new messages") would only repeat the members, which are forwarded.
     * - Not the ongoing notice of a foreground service ("syncing", "playing"). Android shows
     *   it while the service runs, and updates it often; on the host each update would pop up
     *   again, and the host's taskbar already shows which apps run.
     * - Not of minimum importance: Android shows those nowhere but in the shade, folded away.
     * - Not empty: with no title and no text there is nothing to show.
     */
    fun forwardable(notice: GuestNotice): Boolean = when {
        notice.packageName == ownPackage -> false
        notice.groupSummary -> false
        notice.ongoing && notice.foregroundService -> false
        notice.importance in 0..IMPORTANCE_MIN -> false
        notice.title.isBlank() && notice.text.isBlank() -> false
        else -> true
    }

    companion object {
        /** NotificationManager.IMPORTANCE_MIN, IMPORTANCE_HIGH and Notification.PRIORITY_HIGH. */
        const val IMPORTANCE_MIN = 1
        const val IMPORTANCE_HIGH = 4
        const val PRIORITY_HIGH = 1

        /** How large the icon is drawn, on a side: a notification's image is shown at 48 pixels at 200 %. */
        const val ICON_PIXELS = 96

        /** The host takes no larger icon. */
        const val MAX_ICON_BYTES = 64 * 1024

        /** The host keeps no longer title or body; the rest is cut here, before it crosses. */
        const val MAX_LINE_CHARS = 256
        const val MAX_TEXT_CHARS = 4096

        /** Whether the app asked for the notification to interrupt the user. */
        fun highPriority(notice: GuestNotice): Boolean =
            notice.importance >= IMPORTANCE_HIGH || notice.priority >= PRIORITY_HIGH
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityOptions
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.view.Display
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.notifications.GuestNotice
import io.github.thisisthepy.dsa.agent.notifications.NoticeListener
import io.github.thisisthepy.dsa.agent.notifications.NotificationForwarder
import io.github.thisisthepy.dsa.agent.notifications.NotificationSource
import java.util.concurrent.ConcurrentHashMap

/**
 * The guest's notifications, through the agent's notification listener
 * ([HostNotificationListener]).
 *
 * A listener runs only once the user has allowed it, which the guest has no screen to ask.
 * The agent is a platform app holding MANAGE_NOTIFICATION_LISTENERS, so it allows its own
 * listener at start through NotificationManager.setNotificationListenerAccessGranted, the
 * call Settings makes when a person allows one. The call is not in the SDK and is made by
 * reflection.
 *
 * The listener is called by the notification manager in system_server over binder, so it
 * keeps working while the device is in Doze: Doze holds back apps' network, jobs and alarms,
 * not binder calls into a persistent process such as the agent's. A high priority message
 * that wakes an app in Doze, as FCM's do, reaches the listener as soon as the app posts it.
 *
 * Each notification shown is kept by key, so a click from the host can send its content
 * intent and a dismissal can cancel it.
 */
class AndroidNotifications(private val context: Context, private val log: Log) : NotificationSource {
    private val shown = ConcurrentHashMap<String, StatusBarNotification>()

    @Volatile
    private var listener: NoticeListener? = null

    override fun listen(listener: NoticeListener) {
        this.listener = listener
        current = this
        allowListener()
        // The listener may have connected before the agent started listening.
        HostNotificationListener.instance?.let { connected(it) }
    }

    private fun allowListener() {
        val manager = context.getSystemService(NotificationManager::class.java)
        val component = ComponentName(context, HostNotificationListener::class.java)
        try {
            val granted = NotificationManager::class.java
                .getMethod("isNotificationListenerAccessGranted", ComponentName::class.java)
                .invoke(manager, component) as Boolean
            if (granted) return
            NotificationManager::class.java
                .getMethod("setNotificationListenerAccessGranted", ComponentName::class.java, Boolean::class.javaPrimitiveType)
                .invoke(manager, component, true)
            log.info("allowed the agent's notification listener")
        } catch (e: ReflectiveOperationException) {
            log.warn("could not allow the agent's notification listener; notifications do not reach the host", e)
        } catch (e: SecurityException) {
            log.warn("Android refused the agent its notification listener; notifications do not reach the host", e)
        }
    }

    internal fun connected(service: NotificationListenerService) {
        shown.clear()
        val active = try {
            service.activeNotifications.orEmpty()
        } catch (e: Exception) {
            log.warn("reading the notifications shown failed", e)
            emptyArray()
        }
        for (sbn in active) shown[sbn.key] = sbn
        listener?.connected()
    }

    internal fun posted(service: NotificationListenerService, sbn: StatusBarNotification, ranking: NotificationListenerService.RankingMap?) {
        shown[sbn.key] = sbn
        val notice = try {
            notice(sbn, ranking ?: service.currentRanking)
        } catch (e: Exception) {
            log.warn("could not read a notification of ${sbn.packageName}", e)
            return
        }
        listener?.posted(notice)
    }

    internal fun removed(sbn: StatusBarNotification) {
        shown.remove(sbn.key)
        listener?.removed(sbn.key)
    }

    override fun active(): List<GuestNotice> {
        val service = HostNotificationListener.instance ?: return emptyList()
        val ranking = service.currentRanking
        return shown.values.mapNotNull { sbn ->
            try {
                notice(sbn, ranking)
            } catch (e: Exception) {
                log.warn("could not read a notification of ${sbn.packageName}", e)
                null
            }
        }
    }

    /**
     * Sends the notification's content intent as the user's click: it may start an activity
     * though no window of the agent is in front, as the agent does for every launch, and it
     * goes to the guest's one display. A notification with no content intent opens its app.
     * One that cancels itself when clicked is cancelled, as SystemUI does.
     */
    @Suppress("DEPRECATION")
    override fun open(key: String): Boolean {
        val sbn = shown[key] ?: return false
        val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
        options.setPendingIntentBackgroundActivityStartMode(ActivityOptions.MODE_BACKGROUND_ACTIVITY_START_ALLOWED)
        val content = sbn.notification.contentIntent
        if (content != null) {
            try {
                content.send(context, 0, null, null, null, null, options.toBundle())
            } catch (e: PendingIntent.CanceledException) {
                log.warn("the notification's intent was cancelled by its app; opening the app instead", e)
                launch(sbn.packageName, options)
            }
        } else {
            launch(sbn.packageName, options)
        }
        if (sbn.notification.flags and Notification.FLAG_AUTO_CANCEL != 0) cancel(key)
        return true
    }

    private fun launch(packageName: String, options: ActivityOptions) {
        val intent = context.packageManager.getLaunchIntentForPackage(packageName)
        if (intent == null) {
            log.warn("$packageName has nothing to open")
            return
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent, options.toBundle())
    }

    override fun cancel(key: String): Boolean {
        if (!shown.containsKey(key)) return false
        val service = HostNotificationListener.instance ?: return false
        service.cancelNotification(key)
        return true
    }

    private fun notice(sbn: StatusBarNotification, ranking: NotificationListenerService.RankingMap?): GuestNotice {
        val notification = sbn.notification
        val extras = notification.extras
        val title = extras.getCharSequence(Notification.EXTRA_TITLE)?.toString().orEmpty()
        val text = (extras.getCharSequence(Notification.EXTRA_TEXT) ?: extras.getCharSequence(Notification.EXTRA_BIG_TEXT))
            ?.toString().orEmpty()
        val importance = ranking?.let {
            val rank = NotificationListenerService.Ranking()
            if (it.getRanking(sbn.key, rank)) rank.importance else null
        } ?: NotificationManager.IMPORTANCE_DEFAULT
        val pm = context.packageManager
        val label = try {
            pm.getApplicationLabel(pm.getApplicationInfo(sbn.packageName, 0)).toString()
        } catch (e: PackageManager.NameNotFoundException) {
            sbn.packageName
        }
        return GuestNotice(
            key = sbn.key,
            packageName = sbn.packageName,
            appLabel = label,
            title = title,
            text = text,
            iconPng = icon(sbn),
            importance = importance,
            priority = priority(notification),
            ongoing = notification.flags and Notification.FLAG_ONGOING_EVENT != 0,
            foregroundService = notification.flags and Notification.FLAG_FOREGROUND_SERVICE != 0,
            groupSummary = notification.flags and Notification.FLAG_GROUP_SUMMARY != 0,
            clearable = sbn.isClearable,
            whenMillis = notification.`when`,
        )
    }

    @Suppress("DEPRECATION")
    private fun priority(notification: Notification): Int = notification.priority

    /**
     * The notification's large icon (a contact's picture, an album's cover), else the app's
     * icon. The small icon is a one-colour mask meant for a status bar, which the host has
     * none of.
     */
    private fun icon(sbn: StatusBarNotification): ByteArray = try {
        val drawable = sbn.notification.getLargeIcon()?.loadDrawable(context)
            ?: context.packageManager.getApplicationIcon(sbn.packageName)
        iconPng(drawable, NotificationForwarder.ICON_PIXELS)
    } catch (e: Exception) {
        log.warn("could not draw the icon of a notification of ${sbn.packageName}", e)
        ByteArray(0)
    }

    companion object {
        /** The one the listener service reports to. */
        @Volatile
        internal var current: AndroidNotifications? = null
    }
}

/**
 * The agent's notification listener. The notification manager binds it once the agent has
 * allowed it, and calls it for every notification. Only the notification manager may bind it
 * (BIND_NOTIFICATION_LISTENER_SERVICE).
 */
class HostNotificationListener : NotificationListenerService() {
    override fun onListenerConnected() {
        instance = this
        AndroidNotifications.current?.connected(this)
    }

    override fun onListenerDisconnected() {
        instance = null
        // The notification manager unbinds a listener when its process restarts or the
        // access is changed; asking keeps notifications coming once it can bind again.
        requestRebind(ComponentName(this, HostNotificationListener::class.java))
    }

    override fun onNotificationPosted(sbn: StatusBarNotification, rankingMap: RankingMap?) {
        AndroidNotifications.current?.posted(this, sbn, rankingMap)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification, rankingMap: RankingMap?, reason: Int) {
        AndroidNotifications.current?.removed(sbn)
    }

    companion object {
        @Volatile
        internal var instance: HostNotificationListener? = null
    }
}

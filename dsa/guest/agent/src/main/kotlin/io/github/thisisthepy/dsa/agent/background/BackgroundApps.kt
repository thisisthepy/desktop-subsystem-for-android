package io.github.thisisthepy.dsa.agent.background

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.notifications.GuestNotice
import io.github.thisisthepy.dsa.agent.notifications.NoticeListener
import io.github.thisisthepy.dsa.agent.notifications.NotificationSource
import io.github.thisisthepy.dsa.control.v1.BackgroundApp
import io.github.thisisthepy.dsa.control.v1.BackgroundAppCommand
import io.github.thisisthepy.dsa.control.v1.BackgroundAppGone
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.MediaCommand
import io.github.thisisthepy.dsa.control.v1.MediaPlayback
import io.github.thisisthepy.dsa.control.v1.MediaSession
import io.github.thisisthepy.dsa.control.v1.MediaSessionGone

/** Where a media session's playback is. */
enum class Playback { STOPPED, PLAYING, PAUSED }

/** An app's active media session, as the agent reads it. */
data class MediaState(
    val packageName: String,
    val appLabel: String,
    val title: String,
    val artist: String,
    val album: String,
    /** A PNG image of at most [BackgroundApps.ART_PIXELS] on a side; empty for none. */
    val artPng: ByteArray,
    val playback: Playback,
    val canPlay: Boolean,
    val canPause: Boolean,
    val canNext: Boolean,
    val canPrevious: Boolean,
) {
    override fun equals(other: Any?): Boolean =
        other is MediaState && packageName == other.packageName && appLabel == other.appLabel &&
            title == other.title && artist == other.artist && album == other.album &&
            artPng.contentEquals(other.artPng) && playback == other.playback &&
            canPlay == other.canPlay && canPause == other.canPause && canNext == other.canNext &&
            canPrevious == other.canPrevious

    override fun hashCode(): Int = packageName.hashCode()
}

/** Hears of the guest's media sessions, on any thread. */
interface MediaListener {
    /** A session became active, or its metadata or playback changed. */
    fun changed(state: MediaState)

    /** The package has no active session any more. */
    fun gone(packageName: String)
}

/** The guest's active media sessions, as the agent reaches them. */
interface MediaSource {
    fun listen(listener: MediaListener)

    /** The active sessions now. */
    fun active(): List<MediaState>

    /** Runs [command] on the package's session. False when it has none. */
    fun control(packageName: String, command: MediaCommand): Boolean

    companion object {
        val NONE = object : MediaSource {
            override fun listen(listener: MediaListener) = Unit
            override fun active(): List<MediaState> = emptyList()
            override fun control(packageName: String, command: MediaCommand) = false
        }
    }
}

/** Ends an app in the guest: its services, its processes, its alarms. */
fun interface AppStopper {
    /** Force-stops [packageName]. False when Android refused. */
    fun stop(packageName: String): Boolean

    companion object {
        val NONE = AppStopper { false }
    }
}

/**
 * Tells the host which apps keep running with no window, because they run a foreground
 * service (music playback, a download, a VPN), and acts on what the user does with them
 * there: open the app, run one of its notification's actions, stop it, or press a media key.
 *
 * A foreground service always shows an ongoing notification; that notification is what the
 * agent reads ([GuestNotice.foregroundService]). An app may run several services; the host
 * gets one entry per package, from its latest service notification, and hears the package is
 * gone only when its last one is. The guest has no status bar, so the host's notification
 * area is where such an app is seen and stopped.
 *
 * The app's active media session goes to the host as well, so the host's media keys and
 * media controls reach it.
 *
 * Every method but the listeners' runs on the agent's one thread.
 */
class BackgroundApps(
    private val ownPackage: String,
    private val notifications: NotificationSource,
    private val media: MediaSource,
    private val stopper: AppStopper,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val isConnected: () -> Boolean,
) : NoticeListener {
    /** The ongoing notices of foreground services, by key, the latest last. */
    private val services = LinkedHashMap<String, GuestNotice>()

    /** What the host was last told of each package. */
    private val sentApps = mutableMapOf<String, BackgroundApp>()

    private val sessions = mutableMapOf<String, MediaState>()
    private val sentMedia = mutableMapOf<String, MediaSession>()

    /** Starts listening to the media sessions; [post] runs work on the agent's thread. */
    fun start(post: (() -> Unit) -> Unit) {
        try {
            val apps = this
            media.listen(object : MediaListener {
                override fun changed(state: MediaState) {
                    post { apps.mediaChanged(state) }
                }

                override fun gone(packageName: String) {
                    post { apps.mediaGone(packageName) }
                }
            })
        } catch (e: Exception) {
            log.warn("cannot read Android's media sessions; media keys do not reach its apps", e)
        }
    }

    /** The notifications can be read again, or the host connected: it is told of everything. */
    override fun connected() {
        sentApps.clear()
        sentMedia.clear()
        services.clear()
        val active = try {
            notifications.active()
        } catch (e: Exception) {
            log.warn("reading Android's notifications failed", e)
            emptyList()
        }
        for (notice in active) if (isService(notice)) services[notice.key] = notice
        for (packageName in services.values.map { it.packageName }.toSet()) publish(packageName)
        sessions.clear()
        val playing = try {
            media.active()
        } catch (e: Exception) {
            log.warn("reading Android's media sessions failed", e)
            emptyList()
        }
        for (state in playing) mediaChanged(state)
    }

    override fun posted(notice: GuestNotice) {
        if (isService(notice)) {
            services.remove(notice.key)
            services[notice.key] = notice
            publish(notice.packageName)
        } else if (services.remove(notice.key) != null) {
            publish(notice.packageName)
        }
    }

    override fun removed(key: String) {
        val gone = services.remove(key) ?: return
        publish(gone.packageName)
    }

    private fun isService(notice: GuestNotice) =
        notice.foregroundService && notice.packageName != ownPackage

    /** The latest service notice of [packageName], if it runs one. */
    private fun latest(packageName: String): GuestNotice? =
        services.values.lastOrNull { it.packageName == packageName }

    /** Tells the host what is known of [packageName] now, if that changed. */
    private fun publish(packageName: String) {
        val notice = latest(packageName)
        if (notice == null) {
            if (sentApps.remove(packageName) != null) sendGone(packageName)
            return
        }
        if (!isConnected()) return
        val icon = if (notice.iconPng.size > MAX_ICON_BYTES) ByteArray(0) else notice.iconPng
        val message = BackgroundApp.newBuilder()
            .setPackageName(packageName)
            .setAppLabel(notice.appLabel.take(MAX_LINE_CHARS))
            .setIconPng(ByteString.copyFrom(icon))
            .setTitle(notice.title.take(MAX_LINE_CHARS))
            .setText(notice.text.take(MAX_LINE_CHARS))
            .addAllActions(notice.actions.take(MAX_ACTIONS).map { it.title.take(MAX_LINE_CHARS) })
            .build()
        if (sentApps[packageName] == message) return
        if (send(Envelope.newBuilder().setBackgroundApp(message).build().toByteArray())) {
            sentApps[packageName] = message
        }
    }

    private fun sendGone(packageName: String) {
        if (!isConnected()) return
        send(
            Envelope.newBuilder()
                .setBackgroundAppGone(BackgroundAppGone.newBuilder().setPackageName(packageName))
                .build().toByteArray(),
        )
    }

    fun mediaChanged(state: MediaState) {
        if (state.packageName == ownPackage) return
        sessions[state.packageName] = state
        if (!isConnected()) return
        val art = if (state.artPng.size > MAX_ART_BYTES) ByteArray(0) else state.artPng
        val message = MediaSession.newBuilder()
            .setPackageName(state.packageName)
            .setAppLabel(state.appLabel.take(MAX_LINE_CHARS))
            .setTitle(state.title.take(MAX_LINE_CHARS))
            .setArtist(state.artist.take(MAX_LINE_CHARS))
            .setAlbum(state.album.take(MAX_LINE_CHARS))
            .setArtPng(ByteString.copyFrom(art))
            .setPlayback(
                when (state.playback) {
                    Playback.PLAYING -> MediaPlayback.MEDIA_PLAYBACK_PLAYING
                    Playback.PAUSED -> MediaPlayback.MEDIA_PLAYBACK_PAUSED
                    Playback.STOPPED -> MediaPlayback.MEDIA_PLAYBACK_STOPPED
                },
            )
            .setCanPlay(state.canPlay)
            .setCanPause(state.canPause)
            .setCanNext(state.canNext)
            .setCanPrevious(state.canPrevious)
            .build()
        if (sentMedia[state.packageName] == message) return
        if (send(Envelope.newBuilder().setMediaSession(message).build().toByteArray())) {
            sentMedia[state.packageName] = message
        }
    }

    fun mediaGone(packageName: String) {
        sessions.remove(packageName)
        if (sentMedia.remove(packageName) != null) sendMediaGone(packageName)
    }

    private fun sendMediaGone(packageName: String) {
        if (!isConnected()) return
        send(
            Envelope.newBuilder()
                .setMediaSessionGone(MediaSessionGone.newBuilder().setPackageName(packageName))
                .build().toByteArray(),
        )
    }

    /** Acts on the frame if it is the user's word about an app that runs in the background. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.BACKGROUND_APP_ACTION -> act(
                envelope.backgroundAppAction.packageName,
                envelope.backgroundAppAction.command,
                envelope.backgroundAppAction.action,
            )
            Envelope.BodyCase.MEDIA_CONTROL -> control(
                envelope.mediaControl.packageName,
                envelope.mediaControl.command,
            )
            else -> return false
        }
        return true
    }

    private fun act(packageName: String, command: BackgroundAppCommand, action: Int) {
        if (command == BackgroundAppCommand.BACKGROUND_APP_COMMAND_STOP) {
            stop(packageName)
            return
        }
        val notice = latest(packageName)
        if (notice == null) {
            log.info("the host acted on $packageName, which runs nothing in the background; removing it there")
            sentApps.remove(packageName)
            sendGone(packageName)
            return
        }
        val done = try {
            when (command) {
                BackgroundAppCommand.BACKGROUND_APP_COMMAND_OPEN -> notifications.open(notice.key)
                BackgroundAppCommand.BACKGROUND_APP_COMMAND_ACTION -> notifications.runAction(notice.key, action)
                else -> {
                    log.info("the host sent an unknown command for $packageName; ignored")
                    true
                }
            }
        } catch (e: Exception) {
            log.warn("could not act on $packageName as the host asked", e)
            return
        }
        if (!done) log.info("$packageName did not take what the host asked of its notification")
    }

    /**
     * Force-stops the package, as Settings' "Force stop" does: its services end, its processes
     * die and their memory is freed. The agent itself and the system are never stopped.
     */
    private fun stop(packageName: String) {
        if (packageName.isBlank() || packageName == ownPackage || packageName == SYSTEM) {
            log.info("the host asked to stop $packageName, which is not stopped")
            return
        }
        log.info("force-stopping $packageName: the host asked to stop it from its background apps")
        val stopped = try {
            stopper.stop(packageName)
        } catch (e: Exception) {
            log.warn("stopping $packageName failed", e)
            false
        }
        if (!stopped) {
            log.info("Android did not stop $packageName")
            return
        }
        log.info("stopped $packageName as the host asked")
        services.values.removeAll { it.packageName == packageName }
        sentApps.remove(packageName)
        sendGone(packageName)
        mediaGone(packageName)
    }

    private fun control(packageName: String, command: MediaCommand) {
        val done = try {
            media.control(packageName, command)
        } catch (e: Exception) {
            log.warn("the media key for $packageName failed", e)
            return
        }
        if (!done) {
            log.info("the host pressed a media key for $packageName, which has no session; removing it there")
            sessions.remove(packageName)
            sentMedia.remove(packageName)
            sendMediaGone(packageName)
        }
    }

    companion object {
        /** The package of the system itself, which is never stopped. */
        const val SYSTEM = "android"

        /** The host keeps no more, and no longer text, than these; the rest is cut here. */
        const val MAX_LINE_CHARS = 256
        const val MAX_ACTIONS = 8
        const val MAX_ICON_BYTES = 64 * 1024
        const val MAX_ART_BYTES = 256 * 1024

        /** How large album art is drawn, on a side: the media flyout shows it at most this big. */
        const val ART_PIXELS = 256
    }
}

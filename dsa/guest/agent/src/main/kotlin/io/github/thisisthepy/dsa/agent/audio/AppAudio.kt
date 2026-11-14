package io.github.thisisthepy.dsa.agent.audio

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.AppAudioStream
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.AppAudio as AppAudioMessage
import kotlin.math.abs
import kotlin.math.roundToInt

/** Android's media volume as the agent reads it: its index, its range and its mute. */
data class MediaVolume(val index: Int, val min: Int, val max: Int, val muted: Boolean) {
    /** The index as a volume from 0 to 1. */
    val fraction: Float
        get() = if (max <= min) 1f else ((index - min).toFloat() / (max - min)).coerceIn(0f, 1f)

    /** The index nearest [volume], from 0 to 1. */
    fun indexOf(volume: Float): Int = min + (volume.coerceIn(0f, 1f) * (max - min)).roundToInt()
}

/** One app's volume, from 0 to 1, and its mute. */
data class AppVolume(val volume: Float, val muted: Boolean) {
    companion object {
        /** A new app plays at full volume, as a new program on the host does. */
        val FULL = AppVolume(1f, false)
    }
}

/**
 * What the agent needs of Android for the apps' sound. The platform's is
 * [io.github.thisisthepy.dsa.agent.platform.AndroidAppAudio]; a test's records the calls.
 */
interface AppAudioPort {
    /**
     * Sets up the routing of apps to the audio module's app outputs, and calls
     * [onMediaVolumeChanged], on any thread, whenever Android's media volume or its mute
     * changes. Returns how many app outputs there are to route to; 0 when apps cannot be
     * routed, and every app plays on the shared output.
     */
    fun start(onMediaVolumeChanged: () -> Unit): Int

    /** Routes [packageName]'s sound to app output [slot], or back to the shared output (null). */
    fun route(packageName: String, slot: Int?): Boolean

    /** The app's name as a launcher shows it. */
    fun label(packageName: String): String

    /** Android's media volume now; null when it cannot be read. */
    fun mediaVolume(): MediaVolume?

    /** Sets Android's media volume and its mute, showing nothing. */
    fun setMediaVolume(index: Int, muted: Boolean)

    /** The packages whose sound plays now. */
    fun playing(): Set<String>

    companion object {
        val NONE = object : AppAudioPort {
            override fun start(onMediaVolumeChanged: () -> Unit) = 0
            override fun route(packageName: String, slot: Int?) = false
            override fun label(packageName: String) = packageName
            override fun mediaVolume(): MediaVolume? = null
            override fun setMediaVolume(index: Int, muted: Boolean) {}
            override fun playing(): Set<String> = emptySet()
        }
    }
}

/** Where each app's volume is kept across restarts of the agent and of the guest. */
interface AppVolumeStore {
    fun load(): Map<String, AppVolume>

    fun save(packageName: String, volume: AppVolume)

    companion object {
        fun memory(): AppVolumeStore = object : AppVolumeStore {
            private val kept = HashMap<String, AppVolume>()
            override fun load(): Map<String, AppVolume> = HashMap(kept)
            override fun save(packageName: String, volume: AppVolume) {
                kept[packageName] = volume
            }
        }
    }
}

/**
 * Makes the host the one place the guest's sound is turned up or down.
 *
 * The audio module sends every output at full scale, so Android's media volume no longer
 * makes the sound quieter on its way to the host. Instead, each app with a window gets an
 * app output of its own ([tasks]); the host plays it as that app, under the app's name and
 * icon in its volume mixer, at the app's volume. The agent tells the host which app each
 * output plays and that volume (AppAudio), whenever either changes.
 *
 * An app's volume is what Android's media volume says while the app is the one it stands
 * for: the app whose window has the focus, or the last one that had it. When an app sets its
 * volume, or a volume key is pressed, the new media volume is that app's ([mediaVolumeChanged]).
 * When the focus moves to another app, Android's media volume becomes that app's, so the app
 * reads its own volume. A volume the user sets in the host's mixer (SetAppVolume) becomes the
 * app's, and Android's media volume while the app is the one it stands for. Every app's
 * volume is kept ([AppVolumeStore]). The agent's own changes of the media volume come back
 * as broadcasts, and are known.
 *
 * There are as many app outputs as the audio module has. An app keeps its output after its
 * windows close, so music it plays on goes on under its own name; when every output is taken,
 * the one of the app that had a window longest ago and plays nothing goes to the new app. An
 * app without an output plays on the shared output, which is DSA's in the host's mixer and has
 * a volume of its own (the empty package).
 *
 * Every method runs on the agent's thread.
 */
class AppAudio(
    private val port: AppAudioPort,
    private val store: AppVolumeStore,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val isConnected: () -> Boolean = { true },
) {
    private var outputs = 0

    /** The app on each app output, by package, in the order they last had a window. */
    private val outputOf = LinkedHashMap<String, Int>()
    private val labels = HashMap<String, String>()
    private val volumes = HashMap<String, AppVolume>()

    /** The app Android's media volume stands for now, if any has had the focus. */
    private var owner: String? = null

    /** The media volume the agent set last, which comes back as a broadcast. */
    private var applied: MediaVolume? = null
    private var sent: List<AppAudioStream>? = null

    /** Sets up the routing, and hears Android's media volume through [post]. */
    fun start(post: (() -> Unit) -> Unit) {
        try {
            volumes.putAll(store.load())
        } catch (e: Exception) {
            log.warn("could not read the apps' volumes; each starts at full volume", e)
        }
        outputs = try {
            port.start { post { mediaVolumeChanged() } }
        } catch (e: Exception) {
            log.warn("could not route apps to outputs of their own; every app plays as one on the host", e)
            0
        }
        log.info("apps' sound: $outputs outputs for apps, the volume is the host's")
    }

    /** A new connection: the host hears the whole list again. */
    fun connected() {
        sent = null
        report()
    }

    /**
     * The apps with a window are [packages], and [focused] has the focus, if any. Each gets an
     * output, and Android's media volume becomes the focused app's.
     */
    fun tasks(packages: Set<String>, focused: String?) {
        for (packageName in packages) {
            give(packageName, packages)
            // The order of last use: an app with a window is the newest.
            outputOf.remove(packageName)?.let { outputOf[packageName] = it }
        }
        if (focused != null && focused != owner) {
            owner = focused
            toAndroid(volumeOf(key(focused)))
        }
        report()
    }

    /** Acts on SetAppVolume from the host, and says whether [envelope] was one. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.SET_APP_VOLUME) return false
        val set = envelope.setAppVolume
        val packageName = set.packageName
        val volume = AppVolume(clamp(set.volume), set.muted)
        keep(packageName, volume)
        log.info("the host set ${packageName.ifEmpty { "the shared output" }} to ${percent(volume)}")
        if (packageName == ownerKey()) toAndroid(volume)
        report()
        return true
    }

    /** Android's media volume or its mute changed: the app it stands for has that volume now. */
    fun mediaVolumeChanged() {
        val now = try {
            port.mediaVolume()
        } catch (e: Exception) {
            log.warn("could not read Android's media volume", e)
            null
        } ?: return
        val own = applied
        if (own != null && own.index == now.index && own.muted == now.muted) return
        applied = null
        val key = ownerKey()
        val volume = AppVolume(now.fraction, now.muted)
        val known = volumeOf(key)
        if (abs(known.volume - volume.volume) < TOLERANCE && known.muted == volume.muted) return
        keep(key, volume)
        log.info("Android set ${key.ifEmpty { "the shared output" }} to ${percent(volume)}")
        report()
    }

    /** The volume key of [packageName]: its own, or the shared output's when it has no output. */
    private fun key(packageName: String): String = if (packageName in outputOf) packageName else ""

    private fun ownerKey(): String = owner?.let(::key) ?: ""

    private fun volumeOf(key: String): AppVolume = volumes[key] ?: AppVolume.FULL

    private fun keep(key: String, volume: AppVolume) {
        volumes[key] = volume
        try {
            store.save(key, volume)
        } catch (e: Exception) {
            log.warn("could not keep the volume of ${key.ifEmpty { "the shared output" }}", e)
        }
    }

    /**
     * Gives [packageName] an app output, unless it has one or none can be had. An app in
     * [windowed] keeps its own.
     */
    private fun give(packageName: String, windowed: Set<String>) {
        if (packageName in outputOf || outputs <= 0) return
        val taken = outputOf.values.toSet()
        val slot: Int = (0 until outputs).firstOrNull { it !in taken } ?: run {
            // The app that had a window longest ago and plays nothing gives its output up.
            val playing = try {
                port.playing()
            } catch (e: Exception) {
                emptySet()
            }
            val oldest = outputOf.keys.firstOrNull { it !in playing && it !in windowed } ?: return
            val freed = outputOf.getValue(oldest)
            if (!route(oldest, null)) return
            outputOf.remove(oldest)
            freed
        }
        if (!route(packageName, slot)) return
        outputOf[packageName] = slot
        labels[packageName] = try {
            port.label(packageName)
        } catch (e: Exception) {
            packageName
        }
    }

    private fun route(packageName: String, slot: Int?): Boolean {
        val routed = try {
            port.route(packageName, slot)
        } catch (e: Exception) {
            log.warn("could not route $packageName's sound", e)
            false
        }
        if (routed) {
            log.info(if (slot == null) "$packageName plays on the shared output" else "$packageName plays on app output $slot")
        }
        return routed
    }

    /** Makes [volume] Android's media volume, unless it already is. */
    private fun toAndroid(volume: AppVolume) {
        val now = try {
            port.mediaVolume()
        } catch (e: Exception) {
            null
        } ?: return
        val index = now.indexOf(volume.volume)
        if (index == now.index && volume.muted == now.muted) return
        applied = now.copy(index = index, muted = volume.muted)
        try {
            port.setMediaVolume(index, volume.muted)
        } catch (e: Exception) {
            applied = null
            log.warn("could not set Android's media volume", e)
        }
    }

    private fun report() {
        if (!isConnected()) return
        val streams = ArrayList<AppAudioStream>()
        val shared = volumeOf("")
        streams.add(
            AppAudioStream.newBuilder()
                .setStream(PRIMARY_STREAM)
                .setVolume(shared.volume)
                .setMuted(shared.muted)
                .build(),
        )
        for ((packageName, slot) in outputOf.entries.sortedBy { it.value }) {
            val volume = volumeOf(packageName)
            streams.add(
                AppAudioStream.newBuilder()
                    .setStream("app $slot")
                    .setPackageName(packageName)
                    .setAppLabel(labels[packageName] ?: packageName)
                    .setVolume(volume.volume)
                    .setMuted(volume.muted)
                    .build(),
            )
        }
        if (streams == sent) return
        val frame = Envelope.newBuilder()
            .setAppAudio(AppAudioMessage.newBuilder().addAllStreams(streams))
            .build()
            .toByteArray()
        if (send(frame)) sent = streams
    }

    private fun clamp(volume: Float): Float = if (volume.isNaN()) 1f else volume.coerceIn(0f, 1f)

    private fun percent(volume: AppVolume): String =
        "${(volume.volume * 100).roundToInt()}%" + if (volume.muted) ", muted" else ""

    companion object {
        /** The audio port's name of the shared output. */
        const val PRIMARY_STREAM = "primary"

        /** How far apart two volumes must be to be different: half a percent. */
        const val TOLERANCE = 0.005f
    }
}

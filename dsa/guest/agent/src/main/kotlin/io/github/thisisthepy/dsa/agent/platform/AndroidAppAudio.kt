package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.audiopolicy.AudioMix
import android.media.audiopolicy.AudioMixingRule
import android.media.audiopolicy.AudioPolicy
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.audio.AppAudioPort
import io.github.thisisthepy.dsa.agent.audio.AppVolume
import io.github.thisisthepy.dsa.agent.audio.AppVolumeStore
import io.github.thisisthepy.dsa.agent.audio.MediaVolume

/**
 * Android's half of the apps' sound: an audio policy that routes each app with a window to an
 * app output of DSA's audio module, and Android's media volume.
 *
 * The audio module has a bus device for each app output, at `dsa_app_0`, `dsa_app_1`, ...
 * The agent registers an audio policy (`AudioManager.registerAudioPolicy`, which
 * MODIFY_AUDIO_ROUTING lets it), and gives an app an output by attaching a mix whose rule
 * matches the app's uid and which plays on that output's bus device. Audio policy moves the
 * app's tracks there, playing ones too. Taking the mix out sends the app back to the shared
 * output. The policy, the mixes and their rules are system APIs, which the agent reaches
 * through the framework's own classes (the hidden API module declares them); the registering
 * method and the constants are read by reflection, so no value of the framework's is copied.
 *
 * Android's media volume is the music stream's (`STREAM_MUSIC`), which every app's media
 * plays at. Its changes arrive as the framework's volume and mute broadcasts.
 */
class AndroidAppAudio(private val context: Context, private val log: Log) : AppAudioPort {
    private val audio = context.getSystemService(AudioManager::class.java)
    private var policy: AudioPolicy? = null
    private val devices = HashMap<Int, AudioDeviceInfo>()
    private val mixes = HashMap<Int, AudioMix>()
    private val uids = HashMap<String, Int>()

    override fun start(onMediaVolumeChanged: () -> Unit): Int {
        listen(onMediaVolumeChanged)
        return registerPolicy()
    }

    /** Registers the policy, and finds the app outputs' bus devices; how many there are. */
    private fun registerPolicy(): Int {
        for (device in audio.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            if (device.type != AudioDeviceInfo.TYPE_BUS) continue
            val slot = device.address.removePrefix(ADDRESS_PREFIX).toIntOrNull() ?: continue
            if (device.address.startsWith(ADDRESS_PREFIX) && slot >= 0) devices[slot] = device
        }
        if (devices.isEmpty()) {
            log.warn("the audio module has no app outputs; every app plays on the shared output")
            return 0
        }
        val made = AudioPolicy.Builder(context).build()
        val registered = AudioManager::class.java
            .getMethod("registerAudioPolicy", AudioPolicy::class.java)
            .invoke(audio, made) as Int
        if (registered != AUDIO_POLICY_SUCCESS) {
            log.warn("audio policy refused the agent's routing ($registered); every app plays on the shared output")
            return 0
        }
        policy = made
        // The outputs are numbered from 0 with no gap; past a gap none is used.
        return (0..devices.size).first { it !in devices }
    }

    override fun route(packageName: String, slot: Int?): Boolean {
        val policy = policy ?: return false
        val uid = uids[packageName] ?: uidOf(packageName)?.also { uids[packageName] = it } ?: return false
        // An output plays one app: what was routed there before goes first.
        mixes.remove(uid)?.let { old ->
            if (policy.detachMixes(listOf(old)) != AUDIO_POLICY_SUCCESS) {
                log.warn("audio policy did not take $packageName off its output")
            }
        }
        if (slot == null) return true
        val device = devices[slot] ?: return false
        val rule = AudioMixingRule.Builder()
            .setTargetMixRole(constant(AudioMixingRule::class.java, "MIX_ROLE_PLAYERS"))
            .addMixRule(constant(AudioMixingRule::class.java, "RULE_MATCH_UID"), uid)
            .build()
        val mix = AudioMix.Builder(rule)
            .setFormat(
                AudioFormat.Builder()
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .build(),
            )
            .setRouteFlags(constant(AudioMix::class.java, "ROUTE_FLAG_RENDER"))
            .setDevice(device)
            .build()
        if (policy.attachMixes(listOf(mix)) != AUDIO_POLICY_SUCCESS) return false
        mixes[uid] = mix
        return true
    }

    override fun label(packageName: String): String = try {
        val pm = context.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(packageName, 0)).toString()
    } catch (e: Exception) {
        packageName
    }

    override fun mediaVolume(): MediaVolume = MediaVolume(
        index = audio.getStreamVolume(AudioManager.STREAM_MUSIC),
        min = audio.getStreamMinVolume(AudioManager.STREAM_MUSIC),
        max = audio.getStreamMaxVolume(AudioManager.STREAM_MUSIC),
        muted = audio.isStreamMute(AudioManager.STREAM_MUSIC),
    )

    override fun setMediaVolume(index: Int, muted: Boolean) {
        audio.setStreamVolume(AudioManager.STREAM_MUSIC, index, 0)
        if (audio.isStreamMute(AudioManager.STREAM_MUSIC) != muted) {
            audio.adjustStreamVolume(
                AudioManager.STREAM_MUSIC,
                if (muted) AudioManager.ADJUST_MUTE else AudioManager.ADJUST_UNMUTE,
                0,
            )
        }
    }

    override fun playing(): Set<String> {
        val playingUids = audio.activePlaybackConfigurations.mapNotNull { configuration ->
            try {
                configuration.javaClass.getMethod("getClientUid").invoke(configuration) as Int
            } catch (e: Exception) {
                null
            }
        }.toSet()
        return uids.filterValues { it in playingUids }.keys
    }

    private fun uidOf(packageName: String): Int? = try {
        context.packageManager.getApplicationInfo(packageName, 0).uid
    } catch (e: Exception) {
        log.warn("could not find $packageName's uid", e)
        null
    }

    private fun listen(onMediaVolumeChanged: () -> Unit) {
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                val stream = intent.getIntExtra(EXTRA_STREAM_TYPE, -1)
                if (stream == AudioManager.STREAM_MUSIC) onMediaVolumeChanged()
            }
        }
        val filter = IntentFilter().apply {
            addAction(VOLUME_CHANGED)
            addAction(MUTE_CHANGED)
        }
        context.registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
    }

    /** The framework's value of the constant [name] of [type]. */
    private fun constant(type: Class<*>, name: String): Int = type.getField(name).getInt(null)

    private companion object {
        /** The address of an app output's bus device, before its number. */
        const val ADDRESS_PREFIX = "dsa_app_"
        const val SAMPLE_RATE = 48_000
        /** AudioManager.SUCCESS, what the audio policy calls return when they work; the SDK hides it. */
        const val AUDIO_POLICY_SUCCESS = 0
        /** The framework's broadcasts of a change of a stream's volume and of its mute. */
        const val VOLUME_CHANGED = "android.media.VOLUME_CHANGED_ACTION"
        const val MUTE_CHANGED = "android.media.STREAM_MUTE_CHANGED_ACTION"
        const val EXTRA_STREAM_TYPE = "android.media.EXTRA_VOLUME_STREAM_TYPE"
    }
}

/** Each app's volume in the agent's preferences, as `volume,muted` by package. */
class AndroidAppVolumeStore(context: Context) : AppVolumeStore {
    private val preferences = context.getSharedPreferences("app_volumes", Context.MODE_PRIVATE)

    override fun load(): Map<String, AppVolume> = preferences.all.mapNotNull { (key, value) ->
        val parts = (value as? String)?.split(',') ?: return@mapNotNull null
        val volume = parts.getOrNull(0)?.toFloatOrNull() ?: return@mapNotNull null
        val muted = parts.getOrNull(1)?.toBooleanStrictOrNull() ?: return@mapNotNull null
        key to AppVolume(volume.coerceIn(0f, 1f), muted)
    }.toMap()

    override fun save(packageName: String, volume: AppVolume) {
        preferences.edit().putString(packageName, "${volume.volume},${volume.muted}").apply()
    }
}

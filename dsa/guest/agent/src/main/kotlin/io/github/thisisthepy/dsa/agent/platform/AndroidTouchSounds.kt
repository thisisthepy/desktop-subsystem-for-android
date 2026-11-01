package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.input.TouchSounds

/**
 * Writes the setting the framework reads before it plays a click sound, at every boot, so a
 * user's disk from an earlier image is covered too.
 *
 * It needs WRITE_SETTINGS, which the agent holds as a platform app.
 */
class AndroidTouchSounds(private val context: Context) : TouchSounds {
    override fun turnOff() {
        Settings.System.putInt(context.contentResolver, Settings.System.SOUND_EFFECTS_ENABLED, 0)
    }
}

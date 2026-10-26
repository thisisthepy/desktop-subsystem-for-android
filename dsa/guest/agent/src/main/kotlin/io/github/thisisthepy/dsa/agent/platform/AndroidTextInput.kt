package io.github.thisisthepy.dsa.agent.platform

import android.content.ComponentName
import android.content.Context
import android.os.SystemClock
import android.provider.Settings
import android.view.KeyCharacterMap
import io.github.thisisthepy.dsa.agent.input.InjectedKey
import io.github.thisisthepy.dsa.agent.input.InputMethodSetup
import io.github.thisisthepy.dsa.agent.input.TextKeys

/**
 * Makes [HostInputMethod] the guest's input method through the secure settings the input
 * method manager watches: the only enabled one and the selected one. The image has no other
 * input method, so the manager would pick it anyway; writing the settings at every boot keeps
 * it selected on a user's disk whatever an app or an earlier image left there. It also turns
 * off showing a soft keyboard while a hardware keyboard is attached, which the input method
 * refuses on its own in any case.
 *
 * It needs WRITE_SECURE_SETTINGS, which the agent holds as a privileged platform app.
 */
class AndroidInputMethodSetup(private val context: Context) : InputMethodSetup {
    override fun useHostInputMethod() {
        val id = ComponentName(context, HostInputMethod::class.java).flattenToShortString()
        val resolver = context.contentResolver
        Settings.Secure.putString(resolver, Settings.Secure.ENABLED_INPUT_METHODS, id)
        Settings.Secure.putString(resolver, Settings.Secure.DEFAULT_INPUT_METHOD, id)
        Settings.Secure.putInt(resolver, SHOW_IME_WITH_HARD_KEYBOARD, 0)
    }

    private companion object {
        // Settings.Secure.SHOW_IME_WITH_HARD_KEYBOARD, which the SDK hides.
        const val SHOW_IME_WITH_HARD_KEYBOARD = "show_ime_with_hard_keyboard"
    }
}

/**
 * The key events of the framework's virtual keyboard that type some text, from its key
 * character map: a full keyboard, so a character it has comes with the Shift it needs.
 */
class AndroidTextKeys : TextKeys {
    private val map: KeyCharacterMap = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD)

    override fun keysFor(text: String): List<InjectedKey>? {
        val events = map.getEvents(text.toCharArray()) ?: return null
        val now = SystemClock.uptimeMillis()
        return events.map { event ->
            InjectedKey(
                downTime = now,
                eventTime = now,
                action = event.action,
                keyCode = event.keyCode,
                repeatCount = 0,
                metaState = event.metaState,
                flags = 0,
            )
        }
    }
}

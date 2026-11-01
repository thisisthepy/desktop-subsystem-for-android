package io.github.thisisthepy.dsa.agent.platform

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.clipboard.GuestClipboard

/**
 * The guest's clipboard through the framework's ClipboardManager.
 *
 * Since Android 10 only the default input method and the app with the input focus may read
 * the clipboard or hear that it changed. The clipboard service decides by the calling
 * package, and the agent's package is the guest's default input method ([HostInputMethod],
 * made so at boot by [AndroidInputMethodSetup]), so reads from the agent's process are
 * allowed whichever app has the focus. Should that ever not hold, the clipboard says it has a
 * clip but gives none, and that is logged once. Setting the clip needs no permission.
 */
class AndroidClipboard(context: Context, private val log: Log) : GuestClipboard {
    private val manager = context.getSystemService(ClipboardManager::class.java)
    private var refusedLogged = false

    override fun text(): String? {
        val clip = manager.primaryClip
        if (clip == null) {
            if (manager.hasPrimaryClip() && !refusedLogged) {
                refusedLogged = true
                log.warn("Android refused the agent its clipboard; is the agent still the default input method?")
            }
            return null
        }
        if (clip.itemCount == 0) return null
        // Only text: a clip of a URI or an intent stays in Android.
        return clip.getItemAt(0).text?.toString()
    }

    override fun setText(text: String) {
        manager.setPrimaryClip(ClipData.newPlainText(LABEL, text))
    }

    override fun listen(onChange: () -> Unit) {
        manager.addPrimaryClipChangedListener { onChange() }
    }

    private companion object {
        /** What a clip from the host is called, where Android shows a clip's label. */
        const val LABEL = "DSA"
    }
}

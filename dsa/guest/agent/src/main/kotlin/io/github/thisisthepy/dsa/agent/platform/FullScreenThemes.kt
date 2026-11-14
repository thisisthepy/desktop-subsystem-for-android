package io.github.thisisthepy.dsa.agent.platform

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Point
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.view.Display
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.launchBounds

/**
 * Reads whether an activity sets android:windowFullscreen in its theme: an app's way of asking
 * for full screen that its manifest and resources say before any of its code runs. The agent
 * starts such an app at the size of the whole display, and reports its task as full screen
 * from the moment it is created, so its window opens filling its monitor and the task is not
 * resized, and relaunched, right after its activity is created.
 *
 * It reads android:windowNoDisplay the same way, for the title bar's back arrow, which never
 * counts an activity that shows nothing as a screen to go back to.
 *
 * The answer is kept for each activity, theme and installed copy of its app (its APK's path
 * changes when the app is updated), because the task source asks on every change to any task.
 */
class FullScreenThemes(
    private val context: Context,
    private val log: Log,
) {
    private data class Key(val activity: ComponentName, val theme: Int, val sourceDir: String?, val attr: Int)

    private val known = LinkedHashMap<Key, Boolean>()
    private var warned = false

    /** Whether [activity]'s theme sets android:windowFullscreen. False when it cannot be read. */
    fun asks(activity: ActivityInfo?): Boolean = flag(activity, android.R.attr.windowFullscreen)

    /**
     * Whether [activity]'s theme sets android:windowNoDisplay (Theme.NoDisplay): the activity
     * shows nothing, as a trampoline that only starts another one. False when it cannot be read.
     */
    fun drawsNothing(activity: ActivityInfo?): Boolean = flag(activity, android.R.attr.windowNoDisplay)

    /** The boolean theme attribute [attr] of [activity]'s theme, read once for each. */
    private fun flag(activity: ActivityInfo?, attr: Int): Boolean {
        if (activity == null) return false
        // The activity's own theme, else its application's.
        val theme = activity.themeResource
        if (theme == 0) return false
        val key = Key(ComponentName(activity.packageName, activity.name), theme, activity.applicationInfo?.sourceDir, attr)
        synchronized(known) { known[key]?.let { return it } }
        val set = read(activity.packageName, theme, attr)
        synchronized(known) {
            if (known.size >= MAX_KNOWN) known.remove(known.keys.first())
            known[key] = set
        }
        return set
    }

    /**
     * The bounds to start [intent] at when the activity it starts sets android:windowFullscreen
     * in its theme: the whole display ([launchBounds]), so the task opens at a size its full
     * screen window keeps instead of being resized, and relaunched, right after its activity
     * is created. Null for any other activity, which takes the framework's default bounds, or
     * when the display's size cannot be read.
     */
    fun startBounds(intent: Intent): Rect? {
        val activity = intent.resolveActivityInfo(context.packageManager, 0) ?: return null
        if (!asks(activity)) return null
        val display = context.getSystemService(DisplayManager::class.java)?.getDisplay(Display.DEFAULT_DISPLAY) ?: return null
        val size = Point()
        @Suppress("DEPRECATION")
        display.getRealSize(size)
        return launchBounds(fullscreenTheme = true, displayWidth = size.x, displayHeight = size.y)
            ?.let { Rect(it.left, it.top, it.right, it.bottom) }
    }

    private fun read(packageName: String, theme: Int, attr: Int): Boolean = try {
        val app = context.createPackageContext(packageName, Context.CONTEXT_IGNORE_SECURITY)
        val styled = app.resources.newTheme().apply { applyStyle(theme, true) }
        val values = styled.obtainStyledAttributes(intArrayOf(attr))
        try {
            values.getBoolean(0, false)
        } finally {
            values.recycle()
        }
    } catch (e: PackageManager.NameNotFoundException) {
        false
    } catch (e: RuntimeException) {
        if (!warned) {
            warned = true
            log.warn("could not read the theme of $packageName; apps that ask for full screen in their theme open in a window first", e)
        }
        false
    }

    private companion object {
        /** Far more activities than a guest runs at once. */
        const val MAX_KNOWN = 256
    }
}

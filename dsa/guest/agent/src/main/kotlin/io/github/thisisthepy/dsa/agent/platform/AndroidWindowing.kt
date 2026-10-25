package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.provider.Settings
import android.view.Display
import io.github.thisisthepy.dsa.agent.windowing.WindowingControl

/**
 * The window manager settings behind freeform windows.
 *
 * The two global settings are the ones the developer options write; the window manager
 * observes them. The display's windowing mode is set through the window manager service
 * (IWindowManager.setWindowingMode), which is not in the SDK and is called by reflection.
 */
class AndroidWindowing(private val context: Context) : WindowingControl {
    override fun enableFreeform() {
        val resolver = context.contentResolver
        Settings.Global.putInt(resolver, ENABLE_FREEFORM_SUPPORT, 1)
        Settings.Global.putInt(resolver, FORCE_RESIZABLE_ACTIVITIES, 1)
    }

    override fun displayWindowingMode(): Int {
        return windowManagerInterface().getMethod("getWindowingMode", Int::class.javaPrimitiveType)
            .invoke(windowManager(), Display.DEFAULT_DISPLAY) as Int
    }

    override fun setDisplayWindowingMode(mode: Int) {
        windowManagerInterface()
            .getMethod("setWindowingMode", Int::class.javaPrimitiveType, Int::class.javaPrimitiveType)
            .invoke(windowManager(), Display.DEFAULT_DISPLAY, mode)
    }

    // The methods are looked up on the interface: the proxy class that implements it is private.
    private fun windowManagerInterface(): Class<*> = Class.forName("android.view.IWindowManager")

    private fun windowManager(): Any =
        Class.forName("android.view.WindowManagerGlobal").getMethod("getWindowManagerService").invoke(null)
            ?: error("the window manager service is not up")

    private companion object {
        // Settings.Global.DEVELOPMENT_ENABLE_FREEFORM_WINDOWS_SUPPORT
        const val ENABLE_FREEFORM_SUPPORT = "enable_freeform_support"

        // Settings.Global.DEVELOPMENT_FORCE_RESIZABLE_ACTIVITIES
        const val FORCE_RESIZABLE_ACTIVITIES = "force_resizable_activities"
    }
}

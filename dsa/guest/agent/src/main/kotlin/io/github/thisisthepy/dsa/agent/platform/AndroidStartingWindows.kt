package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.app.ActivityTaskManager
import android.content.Context
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.graphics.PixelFormat
import android.graphics.Point
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Animatable
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
import android.hardware.display.DisplayManager
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.SurfaceControl
import android.view.View
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.ImageView
import android.window.StartingWindowInfo
import android.window.StartingWindowRemovalInfo
import android.window.TaskOrganizer
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.splash.IconKind
import io.github.thisisthepy.dsa.agent.splash.IconSource
import io.github.thisisthepy.dsa.agent.splash.LeashSurfaces
import io.github.thisisthepy.dsa.agent.splash.SplashBackground
import io.github.thisisthepy.dsa.agent.splash.SplashContent
import io.github.thisisthepy.dsa.agent.splash.SplashTheme
import io.github.thisisthepy.dsa.agent.splash.StartingWindow
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy
import io.github.thisisthepy.dsa.agent.splash.StartingWindowStyle
import io.github.thisisthepy.dsa.agent.splash.StartingWindows
import io.github.thisisthepy.dsa.agent.splash.TaskLeashes
import java.util.concurrent.Executor

/**
 * Draws each app's starting window (its splash screen) while the app starts.
 *
 * The window manager asks a task organizer for a starting window, and draws none when no
 * organizer is registered. On a phone the organizer is SystemUI's window manager shell. The
 * guest has no SystemUI, because the host is the shell, so the agent registers one.
 *
 * Registering hands every task to the organizer: the window manager then leaves each task's
 * surface position and crop to it, so the agent places them as the window manager would
 * ([TaskLeashes]). Visibility, bounds, focus and input stay with the window manager, and the
 * agent's task reports, moves and resizes do not go through the organizer. If the agent's
 * process dies, the window manager takes the tasks back.
 *
 * Every callback and every starting window runs on [looper], not the main thread, so the
 * home activity on the main thread still answers focus changes at once.
 */
class AndroidStartingWindows(
    private val context: Context,
    looper: Looper,
    private val log: Log,
) {
    private val handler = Handler(looper)
    private val windows = StartingWindows()
    private val leashes = TaskLeashes(object : LeashSurfaces<SurfaceControl> {
        override fun place(leash: SurfaceControl, x: Int, y: Int, clearCrop: Boolean) {
            if (!leash.isValid) return
            SurfaceControl.Transaction().use { t ->
                t.setPosition(leash, x.toFloat(), y.toFloat())
                if (clearCrop) t.setCrop(leash, null)
                t.apply()
            }
        }

        override fun release(leash: SurfaceControl) = leash.release()
    })

    private val organizer = object : TaskOrganizer(null, Executor { handler.post(it) }) {
        override fun addStartingWindow(info: StartingWindowInfo) = guarded("draw a starting window") { add(info) }

        override fun removeStartingWindow(removalInfo: StartingWindowRemovalInfo) = guarded("remove a starting window") {
            removalInfo.windowAnimationLeash?.let { if (it.isValid) it.release() }
            windows.remove(removalInfo.taskId).forEach { log.warn("could not remove the starting window of task ${removalInfo.taskId}", it) }
        }

        override fun copySplashScreenView(taskId: Int) = guarded("answer a splash screen copy") { declineCopy(taskId) }

        override fun onAppSplashScreenViewRemoved(taskId: Int) {}

        override fun onTaskAppeared(taskInfo: ActivityManager.RunningTaskInfo, leash: SurfaceControl) = guarded("place a task") {
            val position = position(taskInfo)
            leashes.appeared(taskInfo.taskId, leash, position.x, position.y)
        }

        override fun onTaskVanished(taskInfo: ActivityManager.RunningTaskInfo) = guarded("forget a task") {
            leashes.vanished(taskInfo.taskId)
            windows.remove(taskInfo.taskId)
        }

        override fun onTaskInfoChanged(taskInfo: ActivityManager.RunningTaskInfo) = guarded("place a task") {
            val position = position(taskInfo)
            leashes.changed(taskInfo.taskId, position.x, position.y)
        }
    }

    /**
     * Registers the organizer and takes over the tasks that are already there. Failing here
     * costs only the starting windows: the tasks stay with the window manager.
     */
    fun start() {
        handler.post {
            try {
                val tasks = organizer.registerOrganizer()
                for (task in tasks) {
                    val position = position(task.taskInfo)
                    leashes.appeared(task.taskInfo.taskId, task.leash, position.x, position.y)
                }
                log.info("drawing starting windows; organizing ${tasks.size} tasks")
            } catch (e: RuntimeException) {
                log.warn("could not register a task organizer; apps start without a splash screen", e)
            } catch (e: LinkageError) {
                log.warn("this framework has no task organizer; apps start without a splash screen", e)
            }
        }
    }

    private inline fun guarded(what: String, block: () -> Unit) {
        try {
            block()
        } catch (e: RuntimeException) {
            log.warn("could not $what", e)
        } catch (e: LinkageError) {
            log.warn("could not $what", e)
        }
    }

    private fun add(info: StartingWindowInfo) {
        val task = info.taskInfo ?: return
        val activity = info.targetActivityInfo ?: field<ActivityInfo>(task, "topActivityInfo") ?: return
        val style = StartingWindowPolicy.style(
            info.startingWindowTypeParameter,
            topIsHome = field<Int>(task, "topActivityType") == ACTIVITY_TYPE_HOME,
            hasSnapshot = field<Any>(info, "taskSnapshot") != null,
        )
        if (style == StartingWindowStyle.NONE) return
        val errors = windows.add(task.taskId) { draw(info, task, activity, style) }
        errors.forEach { log.warn("could not remove the old starting window of task ${task.taskId}", it) }
    }

    /** Adds the window for [style], or returns null when it cannot be added. */
    private fun draw(info: StartingWindowInfo, task: ActivityManager.RunningTaskInfo, activity: ActivityInfo, style: StartingWindowStyle): StartingWindow? {
        val themed = themedContext(task, activity, info.splashScreenThemeResId) ?: return null
        val attrs = Attributes(themed, activity)
        val night = themed.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        val content = StartingWindowPolicy.content(style, attrs.theme(), if (night) FALLBACK_NIGHT else FALLBACK_DAY)
        val root = view(themed, content, attrs)

        val params = WindowManager.LayoutParams(WindowManager.LayoutParams.TYPE_APPLICATION_STARTING).apply {
            setFitInsetsSides(0)
            setFitInsetsTypes(0)
            format = if (style == StartingWindowStyle.LEGACY) PixelFormat.OPAQUE else PixelFormat.TRANSLUCENT
            // Not touchable or focusable: input goes to the app's window under it.
            flags = WindowManager.LayoutParams.FLAG_HARDWARE_ACCELERATED or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN or
                WindowManager.LayoutParams.FLAG_LAYOUT_INSET_DECOR or
                WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS or
                WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_ALT_FOCUSABLE_IM
            token = info.appToken
            packageName = activity.packageName
            layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            title = "Splash Screen ${activity.packageName}"
        }
        val wm = themed.getSystemService(WindowManager::class.java)
        try {
            wm.addView(root, params)
        } catch (e: WindowManager.BadTokenException) {
            // The activity drew or ended before its starting window could be added.
            log.info("no starting window for task ${task.taskId}: ${e.message}")
            return null
        }
        return object : StartingWindow {
            override fun remove() = wm.removeView(root)
        }
    }

    /**
     * The app's context with the theme it starts with: the splash screen theme it set, else
     * the activity's theme, else the app's, in the task's day or night mode, on its display.
     */
    private fun themedContext(task: ActivityManager.RunningTaskInfo, activity: ActivityInfo, splashTheme: Int): Context? {
        val display = context.getSystemService(DisplayManager::class.java).getDisplay(field<Int>(task, "displayId") ?: 0) ?: return null
        var themed = context.createPackageContext(activity.packageName, Context.CONTEXT_RESTRICTED).createDisplayContext(display)
        val taskConfig = field<Configuration>(task, "configuration")
        if (taskConfig != null && taskConfig.uiMode and Configuration.UI_MODE_NIGHT_MASK != themed.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) {
            themed = themed.createConfigurationContext(taskConfig)
        }
        val theme = when {
            splashTheme != 0 -> splashTheme
            activity.themeResource != 0 -> activity.themeResource
            else -> android.R.style.Theme_DeviceDefault_DayNight
        }
        themed.setTheme(theme)
        return themed
    }

    /** The splash attributes of a themed context, read once. */
    private inner class Attributes(private val themed: Context, private val activity: ActivityInfo) {
        val splashBackground = color(android.R.attr.windowSplashScreenBackground)
        val iconBackground = color(android.R.attr.windowSplashScreenIconBackgroundColor)
        val windowBackground = drawable(android.R.attr.windowBackground)
        val splashIcon = drawable(android.R.attr.windowSplashScreenAnimatedIcon)
        val branding = drawable(android.R.attr.windowSplashScreenBrandingImage)
        val appIcon: Drawable? by lazy {
            try {
                activity.loadIcon(context.packageManager)
            } catch (e: RuntimeException) {
                null
            }
        }

        fun theme() = SplashTheme(
            splashBackground = splashBackground,
            windowBackgroundColor = (windowBackground as? ColorDrawable)?.color,
            hasWindowBackground = windowBackground != null,
            splashIcon = splashIcon?.let(::kind),
            appIcon = appIcon?.let(::kind),
            iconBackground = iconBackground,
            hasBranding = branding != null,
        )

        private fun kind(drawable: Drawable) = if (drawable is AdaptiveIconDrawable) IconKind.ADAPTIVE else IconKind.PLAIN

        // One attribute per lookup: obtainStyledAttributes expects its attributes in order.
        private fun color(attr: Int): Int? {
            val a = themed.obtainStyledAttributes(intArrayOf(attr))
            return try {
                if (a.hasValue(0)) a.getColor(0, 0).takeIf { it != 0 } else null
            } catch (e: RuntimeException) {
                null
            } finally {
                a.recycle()
            }
        }

        private fun drawable(attr: Int): Drawable? {
            val a = themed.obtainStyledAttributes(intArrayOf(attr))
            return try {
                if (a.hasValue(0)) a.getDrawable(0) else null
            } catch (e: RuntimeException) {
                null
            } finally {
                a.recycle()
            }
        }
    }

    private fun view(themed: Context, content: SplashContent, attrs: Attributes): View {
        val density = themed.resources.displayMetrics.density
        fun dp(value: Int) = (value * density + 0.5f).toInt()
        val root = FrameLayout(themed)
        root.background = when (val background = content.background) {
            is SplashBackground.Color -> ColorDrawable(background.argb)
            SplashBackground.WindowBackground -> attrs.windowBackground
        }
        content.icon?.let { icon ->
            val drawable = when (icon.source) {
                IconSource.SPLASH_THEME -> attrs.splashIcon
                IconSource.APP -> attrs.appIcon
            }
            if (drawable != null) {
                icon.background?.let { color ->
                    val circle = View(themed)
                    circle.background = GradientDrawable().apply {
                        shape = GradientDrawable.OVAL
                        setColor(color)
                    }
                    root.addView(circle, FrameLayout.LayoutParams(dp(ICON_CIRCLE_DP), dp(ICON_CIRCLE_DP), Gravity.CENTER))
                }
                val image = ImageView(themed)
                image.scaleType = ImageView.ScaleType.FIT_CENTER
                image.setImageDrawable(drawable)
                root.addView(image, FrameLayout.LayoutParams(dp(icon.sizeDp), dp(icon.sizeDp), Gravity.CENTER))
                (drawable as? Animatable)?.start()
            }
        }
        if (content.branding) {
            attrs.branding?.let { drawable ->
                val image = ImageView(themed)
                image.scaleType = ImageView.ScaleType.FIT_CENTER
                image.setImageDrawable(drawable)
                val params = FrameLayout.LayoutParams(dp(BRANDING_WIDTH_DP), dp(BRANDING_HEIGHT_DP), Gravity.CENTER_HORIZONTAL or Gravity.BOTTOM)
                params.bottomMargin = dp(BRANDING_MARGIN_DP)
                root.addView(image, params)
            }
        }
        return root
    }

    /**
     * Tells the activity it gets no copy of the splash screen. An app that asks to animate
     * the splash screen away itself waits for this answer; the agent's window is not one it
     * can hand over, so the app goes on without it.
     */
    private fun declineCopy(taskId: Int) {
        val service = ActivityTaskManager.getService()
        val method = service.javaClass.methods.firstOrNull { it.name == "onSplashScreenViewCopyFinished" && it.parameterTypes.size == 2 }
        if (method == null) {
            log.warn("this framework cannot be told a splash screen is not copied (task $taskId)")
            return
        }
        method.invoke(service, taskId, null)
    }

    /** The task's position in its parent (TaskInfo.positionInParent, not in the SDK). */
    private fun position(task: ActivityManager.RunningTaskInfo): Point = field<Point>(task, "positionInParent") ?: Point()

    @Suppress("UNCHECKED_CAST")
    private fun <T> field(target: Any, name: String): T? = try {
        target.javaClass.getField(name).get(target) as T?
    } catch (e: ReflectiveOperationException) {
        null
    }

    private companion object {
        // WindowConfiguration.ACTIVITY_TYPE_HOME.
        const val ACTIVITY_TYPE_HOME = 2

        const val FALLBACK_DAY = 0xFFFFFFFF.toInt()
        const val FALLBACK_NIGHT = 0xFF202124.toInt()

        const val ICON_CIRCLE_DP = 160
        const val BRANDING_WIDTH_DP = 200
        const val BRANDING_HEIGHT_DP = 80
        const val BRANDING_MARGIN_DP = 60
    }
}

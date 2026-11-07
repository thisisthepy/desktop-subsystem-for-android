package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.app.ActivityTaskManager
import android.content.Context
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.content.res.Resources
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.PixelFormat
import android.graphics.Point
import android.graphics.Rect
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Animatable
import android.graphics.drawable.BitmapDrawable
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
import io.github.thisisthepy.dsa.agent.splash.LeashPlace
import io.github.thisisthepy.dsa.agent.splash.LeashSurfaces
import io.github.thisisthepy.dsa.agent.splash.SplashBackground
import io.github.thisisthepy.dsa.agent.splash.SplashContent
import io.github.thisisthepy.dsa.agent.splash.SplashIconSize
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
        override fun place(leash: SurfaceControl, x: Int, y: Int, width: Int, height: Int) {
            if (!leash.isValid) return
            SurfaceControl.Transaction().use { t ->
                t.setPosition(leash, x.toFloat(), y.toFloat())
                // In the leash's own space: its origin is the task's top left corner.
                if (width > 0 && height > 0) t.setCrop(leash, Rect(0, 0, width, height))
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
            leashes.appeared(taskInfo.taskId, leash, place(taskInfo))
        }

        override fun onTaskVanished(taskInfo: ActivityManager.RunningTaskInfo) = guarded("forget a task") {
            leashes.vanished(taskInfo.taskId)
            windows.remove(taskInfo.taskId)
        }

        override fun onTaskInfoChanged(taskInfo: ActivityManager.RunningTaskInfo) = guarded("place a task") {
            leashes.changed(taskInfo.taskId, place(taskInfo))
            // The host window gives the task its monitor's density once it is open, and a new
            // one on a move to a monitor of another scale; the splash follows it.
            taskDensity(taskInfo)?.let { density ->
                windows.densityChanged(taskInfo.taskId, density).forEach {
                    log.warn("could not draw the starting window of task ${taskInfo.taskId} at $density dpi", it)
                }
            }
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
                    leashes.appeared(task.taskInfo.taskId, task.leash, place(task.taskInfo))
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
        val display = display(task) ?: return
        val density = SplashIconSize.drawDensity(taskDensity(task), display.densityDpiOf())
        val errors = windows.add(task.taskId, density) { draw(info, task, activity, style, density) }
        errors.forEach { log.warn("could not remove the old starting window of task ${task.taskId}", it) }
    }

    /** The density the task's configuration gives its app, once the host window set one. */
    private fun taskDensity(task: ActivityManager.RunningTaskInfo): Int? =
        field<Configuration>(task, "configuration")?.densityDpi?.takeIf { it > 0 }

    private fun display(task: ActivityManager.RunningTaskInfo) =
        context.getSystemService(DisplayManager::class.java).getDisplay(field<Int>(task, "displayId") ?: 0)

    private fun android.view.Display.densityDpiOf(): Int =
        context.createDisplayContext(this).resources.configuration.densityDpi

    /**
     * Adds the window for [style], drawn at [densityDpi], or returns null when it cannot be
     * added.
     */
    private fun draw(
        info: StartingWindowInfo,
        task: ActivityManager.RunningTaskInfo,
        activity: ActivityInfo,
        style: StartingWindowStyle,
        densityDpi: Int,
    ): StartingWindow? {
        val themed = themedContext(task, activity, info.splashScreenThemeResId, densityDpi) ?: return null
        val root = FrameLayout(themed)
        fill(root, themed, activity, style)

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

            // The same window, its content made again from the app's resources at the new
            // density: the window manager keeps its place, so nothing flickers.
            override fun redraw(densityDpi: Int) {
                val again = themedContext(task, activity, info.splashScreenThemeResId, densityDpi)
                    ?: throw IllegalStateException("the display of task ${task.taskId} is gone")
                root.removeAllViews()
                fill(root, again, activity, style)
            }
        }
    }

    /** Puts the content of a starting window of [style] into [root], drawn for [themed]. */
    private fun fill(root: FrameLayout, themed: Context, activity: ActivityInfo, style: StartingWindowStyle) {
        val attrs = Attributes(themed, activity)
        val night = themed.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        val content = StartingWindowPolicy.content(style, attrs.theme(), if (night) FALLBACK_NIGHT else FALLBACK_DAY)
        view(root, themed, content, attrs)
    }

    /**
     * The app's context with the theme it starts with: the splash screen theme it set, else
     * the activity's theme, else the app's, in the task's day or night mode, on its display,
     * at [densityDpi].
     *
     * The density is the task's, which is its host monitor's, and not the guest display's
     * 160 dpi: the task's frames are shown one pixel to one screen pixel, so a splash laid out
     * at the display's density on a 200% monitor would show its icon at half the size and
     * from bitmaps a quarter as fine.
     */
    private fun themedContext(task: ActivityManager.RunningTaskInfo, activity: ActivityInfo, splashTheme: Int, densityDpi: Int): Context? {
        val display = display(task) ?: return null
        var themed = context.createPackageContext(activity.packageName, Context.CONTEXT_RESTRICTED).createDisplayContext(display)
        val taskConfig = field<Configuration>(task, "configuration")
        val config = Configuration(themed.resources.configuration)
        if (taskConfig != null) {
            config.uiMode = (config.uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or (taskConfig.uiMode and Configuration.UI_MODE_NIGHT_MASK)
        }
        config.densityDpi = densityDpi
        if (config != themed.resources.configuration) themed = themed.createConfigurationContext(config)
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

        /**
         * The icon from [source], loaded so that it is at least [pixels] large: at the
         * context's density, or again at a higher one when the app's bitmap for that density
         * is smaller than the square it goes in.
         */
        fun icon(source: IconSource, pixels: Int): Drawable? {
            val resources = themed.resources
            val id = when (source) {
                IconSource.SPLASH_THEME -> resourceId(android.R.attr.windowSplashScreenAnimatedIcon)
                IconSource.APP -> activity.iconResource
            }
            val loaded = when (source) {
                IconSource.SPLASH_THEME -> splashIcon
                IconSource.APP -> if (id != 0) drawableAt(resources, id, resources.configuration.densityDpi) ?: appIcon else appIcon
            }
            if (id == 0 || loaded == null) return loaded
            return sharpDrawable(loaded, resources.configuration.densityDpi, pixels) { density -> drawableAt(resources, id, density) }
        }

        private fun drawableAt(resources: Resources, id: Int, densityDpi: Int): Drawable? = try {
            resources.getDrawableForDensity(id, densityDpi, themed.theme)
        } catch (e: Resources.NotFoundException) {
            null
        }

        private fun resourceId(attr: Int): Int {
            val a = themed.obtainStyledAttributes(intArrayOf(attr))
            return try {
                a.getResourceId(0, 0)
            } finally {
                a.recycle()
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

    /** Fills [root] with [content], laid out at [themed]'s density. */
    private fun view(root: FrameLayout, themed: Context, content: SplashContent, attrs: Attributes) {
        val densityDpi = themed.resources.configuration.densityDpi
        fun dp(value: Int) = SplashIconSize.pixels(value, densityDpi)
        root.background = when (val background = content.background) {
            is SplashBackground.Color -> ColorDrawable(background.argb)
            SplashBackground.WindowBackground -> attrs.windowBackground
        }
        content.icon?.let { icon ->
            val pixels = dp(icon.sizeDp)
            val drawable = attrs.icon(icon.source, pixels)?.let { rendered(themed, it, pixels) }
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
                // A rendered icon is exactly the view's size, so it is shown as it is; an
                // animated one is a vector and fits itself to the view.
                image.scaleType = if (drawable is Animatable) ImageView.ScaleType.FIT_CENTER else ImageView.ScaleType.CENTER
                image.setImageDrawable(drawable)
                root.addView(image, FrameLayout.LayoutParams(pixels, pixels, Gravity.CENTER))
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
    }

    /**
     * [drawable] drawn once into a bitmap of exactly [pixels] square, fitted and centred, as
     * the framework's splash does with a still icon: the view then shows it pixel for pixel.
     * An animated drawable stays as it is, so it still animates.
     */
    private fun rendered(themed: Context, drawable: Drawable, pixels: Int): Drawable {
        if (drawable is Animatable || pixels <= 0) return drawable
        val bitmap = Bitmap.createBitmap(pixels, pixels, Bitmap.Config.ARGB_8888)
        val (width, height) = drawable.intrinsicWidth to drawable.intrinsicHeight
        if (width > 0 && height > 0 && width != height) {
            // Fit the longer side, as FIT_CENTER would.
            val scale = pixels.toFloat() / maxOf(width, height)
            val w = (width * scale + 0.5f).toInt()
            val h = (height * scale + 0.5f).toInt()
            drawable.setBounds((pixels - w) / 2, (pixels - h) / 2, (pixels - w) / 2 + w, (pixels - h) / 2 + h)
        } else {
            drawable.setBounds(0, 0, pixels, pixels)
        }
        drawable.draw(Canvas(bitmap))
        bitmap.density = themed.resources.configuration.densityDpi
        return BitmapDrawable(themed.resources, bitmap)
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

    /**
     * The task's place for its surface: its position in its parent and the size of its bounds
     * (TaskInfo.configuration.windowConfiguration.getBounds, not in the SDK). A size that
     * cannot be read is 0 by 0, and the surface then keeps its crop.
     */
    private fun place(task: ActivityManager.RunningTaskInfo): LeashPlace {
        val position = position(task)
        val bounds = try {
            val configuration = task.javaClass.getField("configuration").get(task)
            val windowConfiguration = configuration.javaClass.getField("windowConfiguration").get(configuration)
            windowConfiguration.javaClass.getMethod("getBounds").invoke(windowConfiguration) as Rect?
        } catch (e: ReflectiveOperationException) {
            null
        } catch (e: RuntimeException) {
            null
        }
        return LeashPlace(position.x, position.y, bounds?.width() ?: 0, bounds?.height() ?: 0)
    }

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

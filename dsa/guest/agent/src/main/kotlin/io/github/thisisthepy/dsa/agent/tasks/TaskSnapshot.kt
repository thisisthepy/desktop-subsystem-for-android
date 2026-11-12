package io.github.thisisthepy.dsa.agent.tasks

/** A rectangle in guest display pixels: left and top inclusive, right and bottom exclusive. */
data class TaskBounds(val left: Int, val top: Int, val right: Int, val bottom: Int) {
    /** Whether it covers no pixel: the framework has not given the task bounds. */
    fun isEmpty() = right <= left || bottom <= top

    companion object {
        val EMPTY = TaskBounds(0, 0, 0, 0)
    }
}

/** What kind of task the framework says a task is. Only app tasks get a host window. */
enum class TaskKind {
    /** An ordinary app task (the framework's standard activity type). */
    APP,

    /** Home, recents, the assistant, a dream, or anything else that is not an app's own task. */
    OTHER,
}

/** One running task as the framework describes it, read on the guest. */
data class TaskSnapshot(
    val taskId: Int,
    val kind: TaskKind,
    /** The package of the task's root activity; null when the framework names none. */
    val packageName: String?,
    val label: String,
    val bounds: TaskBounds,
    val focused: Boolean,
    /** The density the task's app draws at, in dots per inch; 0 when the framework names none. */
    val densityDpi: Int = 0,
    /**
     * The screen size the task's configuration gives its app; null when the framework names
     * none. The host is not told it: it only says what to hold while a window is resized.
     */
    val screenDp: ScreenDp? = null,
    /**
     * The smallest size the framework gives the task, in pixels; 0 when the framework does not
     * say. The host keeps the task's window to it: asking for less would leave the task at
     * another size than the one asked for.
     */
    val minWidth: Int = 0,
    val minHeight: Int = 0,
    /**
     * What the task's app told the system about its status bar's icons, which the host's
     * caption buttons follow.
     */
    val statusBar: StatusBarAppearance = StatusBarAppearance.UNKNOWN,
    /** The status bar color the app set, as ARGB; 0 when it set none. */
    val statusBarColor: Int = 0,
    /** The orientation the task's top activity is locked to, which the host's window keeps. */
    val orientation: TaskOrientation = TaskOrientation.UNSPECIFIED,
    /** Whether the task's app asked to fill the screen: it left the freeform windowing mode. */
    val fullscreen: Boolean = false,
    /** Whether the task's app went into picture-in-picture. */
    val pictureInPicture: Boolean = false,
    /**
     * Whether the task's app asked to draw its own title bar (a transparent caption bar
     * background), as apps do on Android's own desktop windows.
     */
    val transparentCaption: Boolean = false,
    /**
     * Whether Back in the task lands on a screen the user saw, so the host draws the back
     * arrow: filled in by the agent from [stack] ([BackStacks]).
     */
    val canGoBack: Boolean = false,
    /**
     * The task's activities as far as Back is concerned; null for a task whose Back the agent
     * never offers (a widget's setup), which keeps [canGoBack] as it is.
     */
    val stack: TaskStack? = null,
    /**
     * Whether the task's top activity says it handles a change of density itself (its
     * manifest's configChanges include density), so Android redraws it at a new density
     * instead of relaunching it. The host gives only such a task another monitor's density,
     * and scales the window of any other.
     */
    val handlesDensityChange: Boolean = false,
    /**
     * Whether the task's top and root activities say they handle a change of font scale
     * themselves (configChanges include fontScale), so Android redraws them at a new font
     * scale instead of restarting them. The agent holds a new font scale while a task that
     * does not is open. The host is not told it.
     */
    val handlesFontScaleChange: Boolean = false,
    /**
     * Where in the title bar band the app has controls, relative to the task's top left
     * corner: filled in by the agent from what the app keeps clear of system gestures, while
     * [transparentCaption] is set.
     */
    val captionInteractive: List<TaskBounds> = emptyList(),
)

/**
 * Whether an app asked to fill the screen by hiding its bars (immersive mode): the window
 * insets types its task asks to be visible ([requestedVisibleTypes], a mask of
 * WindowInsets.Type) leave out both the status bars and the navigation bars. Null, when the
 * framework does not say, is not immersive.
 */
fun hidesBothSystemBars(requestedVisibleTypes: Int?): Boolean {
    if (requestedVisibleTypes == null) return false
    val bars = STATUS_BARS or NAVIGATION_BARS
    return requestedVisibleTypes and bars == 0
}

/** WindowInsets.Type.statusBars() and navigationBars(). */
const val STATUS_BARS = 1
const val NAVIGATION_BARS = 2

/** The orientation a task's top activity asks for. */
enum class TaskOrientation {
    /** It does not lock its orientation, or the agent could not read it. */
    UNSPECIFIED,

    /** Portrait, reverse portrait, sensor portrait or user portrait. */
    PORTRAIT,

    /** Landscape, reverse landscape, sensor landscape or user landscape. */
    LANDSCAPE,
    ;

    companion object {
        /** The orientation an activity's screenOrientation (ActivityInfo.SCREEN_ORIENTATION_*) locks. */
        fun of(screenOrientation: Int): TaskOrientation = when (screenOrientation) {
            // PORTRAIT, SENSOR_PORTRAIT, REVERSE_PORTRAIT, USER_PORTRAIT.
            1, 7, 9, 12 -> PORTRAIT
            // LANDSCAPE, SENSOR_LANDSCAPE, REVERSE_LANDSCAPE, USER_LANDSCAPE.
            0, 6, 8, 11 -> LANDSCAPE
            else -> UNSPECIFIED
        }
    }
}

/** Whether an app asked for dark status bar icons (a light status bar) or light ones. */
enum class StatusBarAppearance {
    /** The agent could not read it. */
    UNKNOWN,

    /** Dark icons on a light status bar (APPEARANCE_LIGHT_STATUS_BARS). */
    LIGHT,

    /** Light icons on a dark status bar. */
    DARK,
}

/**
 * A configuration's screen size in density-independent pixels (Configuration's
 * screenWidthDp, screenHeightDp and smallestScreenWidthDp): what an app's resources, layout
 * and orientation are chosen by.
 */
data class ScreenDp(val widthDp: Int, val heightDp: Int, val smallestWidthDp: Int)

/** Reads the guest's running tasks and says when they may have changed. */
interface TaskSource {
    /** Every running task, top first. */
    fun snapshot(): List<TaskSnapshot>

    /** Calls [onChange], on any thread, whenever a task may have appeared, changed or ended. */
    fun listen(onChange: () -> Unit)
}

/** ActivityInfo.CONFIG_DENSITY: the bit of an activity's configChanges for a change of density. */
const val CONFIG_DENSITY = 0x1000

/**
 * Whether an activity whose manifest declares [configChanges] (ActivityInfo.configChanges)
 * handles a change of density itself. Android relaunches any other activity when its density
 * changes.
 */
fun handlesDensityChange(configChanges: Int): Boolean = (configChanges and CONFIG_DENSITY) != 0

/** ActivityInfo.CONFIG_FONT_SCALE: the bit of an activity's configChanges for a change of font scale. */
const val CONFIG_FONT_SCALE = 0x40000000

/**
 * Whether an activity whose manifest declares [configChanges] handles a change of font scale
 * itself. Android restarts any other activity when the font scale changes.
 */
fun handlesFontScaleChange(configChanges: Int): Boolean = (configChanges and CONFIG_FONT_SCALE) != 0

/**
 * The open apps Android would restart for a change of font scale: each app task whose
 * activities do not say they handle one, by package.
 */
fun restartedByFontScale(tasks: List<TaskSnapshot>): List<String> =
    tasks.filter { it.kind == TaskKind.APP && !it.handlesFontScaleChange }
        .map { it.packageName ?: "task ${it.taskId}" }
        .distinct()

/**
 * Whether a task's app asked to fill the screen, in any of the ways an app asks: its task left
 * the freeform windowing mode for fullscreen ([fullscreenMode]), it hides both its status and
 * navigation bars ([requestedVisibleTypes], see [hidesBothSystemBars]), or its top activity's
 * theme sets android:windowFullscreen ([fullscreenTheme]). The theme is read from the app's
 * manifest before any of its code runs, so a task that asks this way is known to fill the
 * screen from the moment it is created, before its activity has a window.
 */
fun asksForFullScreen(fullscreenMode: Boolean, requestedVisibleTypes: Int?, fullscreenTheme: Boolean): Boolean =
    fullscreenMode || fullscreenTheme || hidesBothSystemBars(requestedVisibleTypes)

/**
 * The bounds the agent starts an app at, on a display of [displayWidth] by [displayHeight]
 * guest pixels: the whole display when the activity it starts sets android:windowFullscreen
 * in its theme ([fullscreenTheme]), else null for the framework's own freeform default.
 *
 * The host fills its monitor with the window of a task whose app asks for full screen. Started
 * at the framework's phone-sized default, such a task would be resized to the monitor right
 * after its activity was created, across Android's size classes (smallest width, screen
 * layout size), and Android relaunches an activity that does not declare it handles those,
 * so the app would lose its state and draw its first frames twice. Started at the whole
 * display, which is at least as large as the main monitor, fitting the task to its monitor
 * keeps it in the size classes of a large screen, and where the monitor is the display's size
 * it is not resized at all. Null when the display's size is not known.
 */
fun launchBounds(fullscreenTheme: Boolean, displayWidth: Int, displayHeight: Int): TaskBounds? =
    if (fullscreenTheme && displayWidth > 0 && displayHeight > 0) {
        TaskBounds(0, 0, displayWidth, displayHeight)
    } else {
        null
    }

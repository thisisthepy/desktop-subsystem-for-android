package io.github.thisisthepy.dsa.agent.splash

/** What the agent draws while an app starts. */
enum class StartingWindowStyle {
    /** Nothing: the task's own surface is already there, or it is home. */
    NONE,

    /** The splash screen: background, icon and branding image. */
    SPLASH,

    /** The splash screen's background alone. */
    SOLID_COLOR,

    /** The app's window background, for an app the framework puts on the pre-splash theme. */
    LEGACY,
}

/** The kind of drawable an icon is, which decides how large it is drawn. */
enum class IconKind {
    /** An AdaptiveIconDrawable: it draws its own masked background. */
    ADAPTIVE,

    /** Any other drawable. */
    PLAIN,
}

/**
 * The splash attributes of the theme an app starts with. A null value is an attribute the
 * theme does not set.
 */
data class SplashTheme(
    /** windowSplashScreenBackground. */
    val splashBackground: Int?,
    /** windowBackground, when it is a single color. */
    val windowBackgroundColor: Int?,
    /** Whether windowBackground is set at all, a color or any other drawable. */
    val hasWindowBackground: Boolean,
    /** windowSplashScreenAnimatedIcon. */
    val splashIcon: IconKind?,
    /** The activity's icon, or the app's. */
    val appIcon: IconKind?,
    /** windowSplashScreenIconBackgroundColor. */
    val iconBackground: Int?,
    /** Whether windowSplashScreenBrandingImage is set. */
    val hasBranding: Boolean,
)

sealed interface SplashBackground {
    data class Color(val argb: Int) : SplashBackground

    /** The theme's windowBackground drawable as it is. */
    data object WindowBackground : SplashBackground
}

enum class IconSource { SPLASH_THEME, APP }

/** The centered icon: where it comes from, its size, and a circle behind it, if any. */
data class SplashIcon(val source: IconSource, val sizeDp: Int, val background: Int?)

data class SplashContent(val background: SplashBackground, val icon: SplashIcon?, val branding: Boolean)

/**
 * Decides which starting window a launch gets and what is in it, the way the framework's own
 * shell does (its PhoneStartingWindowTypeAlgorithm and SplashscreenContentDrawer).
 */
object StartingWindowPolicy {
    // StartingWindowInfo.TYPE_PARAMETER_*, the flags the window manager describes a launch with.
    const val NEW_TASK = 0x00000001
    const val TASK_SWITCH = 0x00000002
    const val PROCESS_RUNNING = 0x00000004
    const val ALLOW_TASK_SNAPSHOT = 0x00000008
    const val ACTIVITY_CREATED = 0x00000010
    const val USE_SOLID_COLOR_SPLASH_SCREEN = 0x00000020
    const val ACTIVITY_DRAWN = 0x00000040
    const val WINDOWLESS = 0x00000100
    const val LEGACY_SPLASH_SCREEN = 0x80000000.toInt()

    /** The visible circle of an icon with a background, and its drawable's size. */
    private const val ICON_DP = 160
    private const val PLAIN_ICON_ON_BACKGROUND_DP = 240

    /** A splash icon without a background shows a 192 dp circle of a 288 dp drawable. */
    private const val PLAIN_ICON_DP = 288

    /**
     * The starting window for a launch described by [parameter]. A snapshot is never drawn:
     * a task that would get one is running and still has its surface. A windowless starting
     * surface belongs to the shell's own transitions, which this guest does not run.
     */
    fun style(parameter: Int, topIsHome: Boolean, hasSnapshot: Boolean): StartingWindowStyle {
        fun has(flag: Int) = parameter and flag != 0
        if (has(WINDOWLESS)) return StartingWindowStyle.NONE
        val splash = when {
            has(USE_SOLID_COLOR_SPLASH_SCREEN) -> StartingWindowStyle.SOLID_COLOR
            has(LEGACY_SPLASH_SCREEN) -> StartingWindowStyle.LEGACY
            else -> StartingWindowStyle.SPLASH
        }
        val taskSwitch = has(TASK_SWITCH)
        if (!topIsHome && (!has(PROCESS_RUNNING) || has(NEW_TASK) || (taskSwitch && !has(ACTIVITY_CREATED)))) {
            return splash
        }
        if (taskSwitch) {
            if (has(ALLOW_TASK_SNAPSHOT)) {
                if (hasSnapshot) return StartingWindowStyle.NONE
                if (!topIsHome) return StartingWindowStyle.SOLID_COLOR
            }
            if (!has(ACTIVITY_DRAWN) && !topIsHome) return splash
        }
        return StartingWindowStyle.NONE
    }

    /** What a starting window of [style] shows for [theme]; [fallback] is the color without one. */
    fun content(style: StartingWindowStyle, theme: SplashTheme, fallback: Int): SplashContent {
        val color = theme.splashBackground ?: theme.windowBackgroundColor ?: fallback
        return when (style) {
            StartingWindowStyle.NONE, StartingWindowStyle.SOLID_COLOR ->
                SplashContent(SplashBackground.Color(color), icon = null, branding = false)
            StartingWindowStyle.LEGACY -> SplashContent(
                if (theme.hasWindowBackground) SplashBackground.WindowBackground else SplashBackground.Color(theme.windowBackgroundColor ?: fallback),
                icon = null,
                branding = false,
            )
            StartingWindowStyle.SPLASH -> SplashContent(SplashBackground.Color(color), icon(theme), theme.hasBranding)
        }
    }

    private fun icon(theme: SplashTheme): SplashIcon? {
        val (source, kind) = when {
            theme.splashIcon != null -> IconSource.SPLASH_THEME to theme.splashIcon
            theme.appIcon != null -> IconSource.APP to theme.appIcon
            else -> return null
        }
        if (kind == IconKind.ADAPTIVE) return SplashIcon(source, ICON_DP, background = null)
        if (source == IconSource.APP) return SplashIcon(source, ICON_DP, theme.iconBackground)
        val size = if (theme.iconBackground != null) PLAIN_ICON_ON_BACKGROUND_DP else PLAIN_ICON_DP
        return SplashIcon(source, size, theme.iconBackground)
    }
}

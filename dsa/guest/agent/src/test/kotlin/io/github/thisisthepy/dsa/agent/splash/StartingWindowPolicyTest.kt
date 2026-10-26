package io.github.thisisthepy.dsa.agent.splash

import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.ACTIVITY_CREATED
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.ACTIVITY_DRAWN
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.ALLOW_TASK_SNAPSHOT
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.LEGACY_SPLASH_SCREEN
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.NEW_TASK
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.PROCESS_RUNNING
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.TASK_SWITCH
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.USE_SOLID_COLOR_SPLASH_SCREEN
import io.github.thisisthepy.dsa.agent.splash.StartingWindowPolicy.WINDOWLESS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * An app's task window shows the app's splash screen from the moment it is launched, instead
 * of black until the app draws its first frame. These tests cover which starting window the
 * agent draws for a launch, and what goes in it.
 */
class StartingWindowPolicyTest {
    private fun style(parameter: Int, topIsHome: Boolean = false, hasSnapshot: Boolean = false) =
        StartingWindowPolicy.style(parameter, topIsHome, hasSnapshot)

    @Test
    fun fr3_a_cold_launch_gets_a_splash_screen() {
        assertEquals(StartingWindowStyle.SPLASH, style(NEW_TASK))
        assertEquals(StartingWindowStyle.SPLASH, style(0))
    }

    @Test
    fun fr3_an_app_on_the_legacy_theme_gets_its_window_background() {
        assertEquals(StartingWindowStyle.LEGACY, style(NEW_TASK or LEGACY_SPLASH_SCREEN))
    }

    @Test
    fun fr3_a_solid_color_request_wins_over_the_legacy_theme() {
        assertEquals(StartingWindowStyle.SOLID_COLOR, style(NEW_TASK or USE_SOLID_COLOR_SPLASH_SCREEN))
        assertEquals(StartingWindowStyle.SOLID_COLOR, style(NEW_TASK or USE_SOLID_COLOR_SPLASH_SCREEN or LEGACY_SPLASH_SCREEN))
    }

    @Test
    fun fr3_the_home_task_gets_no_starting_window() {
        assertEquals(StartingWindowStyle.NONE, style(NEW_TASK, topIsHome = true))
        assertEquals(StartingWindowStyle.NONE, style(TASK_SWITCH or ALLOW_TASK_SNAPSHOT, topIsHome = true))
    }

    @Test
    fun fr3_bringing_back_a_drawn_task_gets_no_starting_window() {
        assertEquals(
            StartingWindowStyle.NONE,
            style(TASK_SWITCH or PROCESS_RUNNING or ACTIVITY_CREATED or ACTIVITY_DRAWN),
        )
        // A snapshot would be the right picture, and the agent does not draw snapshots; the
        // task's own surface is still there, so nothing is drawn over it.
        assertEquals(
            StartingWindowStyle.NONE,
            style(TASK_SWITCH or PROCESS_RUNNING or ACTIVITY_CREATED or ALLOW_TASK_SNAPSHOT, hasSnapshot = true),
        )
    }

    @Test
    fun fr3_switching_to_a_task_whose_activity_is_gone_gets_a_splash_screen() {
        assertEquals(StartingWindowStyle.SPLASH, style(TASK_SWITCH or PROCESS_RUNNING))
        assertEquals(
            StartingWindowStyle.SOLID_COLOR,
            style(TASK_SWITCH or PROCESS_RUNNING or ACTIVITY_CREATED or ALLOW_TASK_SNAPSHOT),
        )
    }

    @Test
    fun fr3_a_windowless_request_is_not_drawn() {
        assertEquals(StartingWindowStyle.NONE, style(NEW_TASK or WINDOWLESS))
    }

    private val noTheme = SplashTheme(
        splashBackground = null,
        windowBackgroundColor = null,
        hasWindowBackground = false,
        splashIcon = null,
        appIcon = null,
        iconBackground = null,
        hasBranding = false,
    )

    @Test
    fun fr3_the_splash_background_comes_from_the_splash_theme_first() {
        val theme = noTheme.copy(splashBackground = 0xFF112233.toInt(), windowBackgroundColor = 0xFFFFFFFF.toInt())
        val content = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, theme, FALLBACK)
        assertEquals(SplashBackground.Color(0xFF112233.toInt()), content.background)
    }

    @Test
    fun fr3_without_a_splash_background_the_window_background_color_is_used() {
        val theme = noTheme.copy(windowBackgroundColor = 0xFF445566.toInt(), hasWindowBackground = true)
        val content = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, theme, FALLBACK)
        assertEquals(SplashBackground.Color(0xFF445566.toInt()), content.background)
    }

    @Test
    fun fr3_with_no_color_at_all_the_fallback_is_used() {
        val content = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, noTheme.copy(hasWindowBackground = true), FALLBACK)
        assertEquals(SplashBackground.Color(FALLBACK), content.background)
    }

    @Test
    fun fr3_the_legacy_starting_window_is_the_window_background_alone() {
        val theme = noTheme.copy(
            splashBackground = 0xFF112233.toInt(),
            hasWindowBackground = true,
            appIcon = IconKind.ADAPTIVE,
            hasBranding = true,
        )
        val content = StartingWindowPolicy.content(StartingWindowStyle.LEGACY, theme, FALLBACK)
        assertEquals(SplashBackground.WindowBackground, content.background)
        assertNull(content.icon)
        assertFalse(content.branding)
    }

    @Test
    fun fr3_a_legacy_theme_without_a_window_background_falls_back_to_a_color() {
        val content = StartingWindowPolicy.content(StartingWindowStyle.LEGACY, noTheme, FALLBACK)
        assertEquals(SplashBackground.Color(FALLBACK), content.background)
    }

    @Test
    fun fr3_the_splash_icon_comes_from_the_theme_before_the_app_icon() {
        val theme = noTheme.copy(splashIcon = IconKind.ADAPTIVE, appIcon = IconKind.PLAIN)
        val icon = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, theme, FALLBACK).icon!!
        assertEquals(IconSource.SPLASH_THEME, icon.source)
        assertEquals(160, icon.sizeDp)
    }

    @Test
    fun fr3_without_a_splash_icon_the_app_icon_is_shown() {
        val theme = noTheme.copy(appIcon = IconKind.ADAPTIVE)
        val icon = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, theme, FALLBACK).icon!!
        assertEquals(IconSource.APP, icon.source)
        assertEquals(160, icon.sizeDp)
    }

    @Test
    fun fr3_a_plain_splash_icon_is_sized_so_its_safe_zone_is_the_icon_circle() {
        // A splash icon that is not adaptive is drawn with its outer third outside the
        // visible circle: 288 dp for a 192 dp circle, or 240 dp for 160 dp on a background.
        val bare = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, noTheme.copy(splashIcon = IconKind.PLAIN), FALLBACK)
        assertEquals(288, bare.icon!!.sizeDp)
        assertNull(bare.icon!!.background)
        val onBackground = StartingWindowPolicy.content(
            StartingWindowStyle.SPLASH,
            noTheme.copy(splashIcon = IconKind.PLAIN, iconBackground = 0xFF00FF00.toInt()),
            FALLBACK,
        )
        assertEquals(240, onBackground.icon!!.sizeDp)
        assertEquals(0xFF00FF00.toInt(), onBackground.icon!!.background)
    }

    @Test
    fun fr3_an_adaptive_icon_draws_its_own_background() {
        val theme = noTheme.copy(splashIcon = IconKind.ADAPTIVE, iconBackground = 0xFF00FF00.toInt())
        assertNull(StartingWindowPolicy.content(StartingWindowStyle.SPLASH, theme, FALLBACK).icon!!.background)
    }

    @Test
    fun fr3_a_solid_color_splash_has_the_background_only() {
        val theme = noTheme.copy(splashBackground = 0xFF112233.toInt(), appIcon = IconKind.ADAPTIVE, hasBranding = true)
        val content = StartingWindowPolicy.content(StartingWindowStyle.SOLID_COLOR, theme, FALLBACK)
        assertEquals(SplashBackground.Color(0xFF112233.toInt()), content.background)
        assertNull(content.icon)
        assertFalse(content.branding)
    }

    @Test
    fun fr3_the_branding_image_shows_on_a_splash_screen() {
        val content = StartingWindowPolicy.content(StartingWindowStyle.SPLASH, noTheme.copy(hasBranding = true), FALLBACK)
        assertTrue(content.branding)
    }

    private companion object {
        const val FALLBACK = 0xFF202124.toInt()
    }
}

package io.github.thisisthepy.dsa.agent.splash

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A splash icon is drawn at the pixel size it is shown at on its window's monitor, from a
 * bitmap at least that large, so it is never scaled up on a 125% or 200% monitor.
 */
class SplashIconSizeTest {
    private fun dpiOf(percent: Int) = SplashIconSize.BASE_DENSITY * percent / 100

    @Test
    fun fr3_the_splash_icon_is_its_size_in_pixels_at_100_125_and_200_percent() {
        // The masked adaptive icon (160 dp) and the splash theme's icon (240 dp, 288 dp).
        assertEquals(160, SplashIconSize.pixels(160, dpiOf(100)))
        assertEquals(200, SplashIconSize.pixels(160, dpiOf(125)))
        assertEquals(320, SplashIconSize.pixels(160, dpiOf(200)))
        assertEquals(300, SplashIconSize.pixels(240, dpiOf(125)))
        assertEquals(576, SplashIconSize.pixels(288, dpiOf(200)))
    }

    @Test
    fun fr4_the_splash_is_drawn_at_the_tasks_density_not_the_displays() {
        // The guest display is 160 dpi; a task on a 200% monitor draws at 320.
        assertEquals(320, SplashIconSize.drawDensity(taskDensityDpi = 320, displayDensityDpi = 160))
        assertEquals(200, SplashIconSize.drawDensity(taskDensityDpi = 200, displayDensityDpi = 160))
        // A task the host has not given a density yet draws at the display's.
        assertEquals(160, SplashIconSize.drawDensity(taskDensityDpi = null, displayDensityDpi = 160))
        assertEquals(160, SplashIconSize.drawDensity(taskDensityDpi = 0, displayDensityDpi = 160))
    }

    @Test
    fun fr3_an_adaptive_icon_is_loaded_fine_enough_for_its_160_dp_mask() {
        // An adaptive icon's intrinsic size is its 72 dp visible part: 144 px at 320 dpi,
        // which a 320 px square would scale up more than twice.
        for (percent in listOf(100, 125, 200)) {
            val density = dpiOf(percent)
            val pixels = SplashIconSize.pixels(160, density)
            val intrinsic = SplashIconSize.pixels(72, density)
            val load = SplashIconSize.loadDensity(density, intrinsic, pixels)
            // Loaded there, it is at least as large as the square it is drawn into.
            assertTrue(SplashIconSize.pixels(72, load) >= pixels)
        }
        assertEquals(712, SplashIconSize.loadDensity(320, 144, 320))
    }

    @Test
    fun fr3_a_legacy_48_dp_icon_is_never_scaled_up_into_the_splash() {
        for (percent in listOf(100, 125, 200)) {
            val density = dpiOf(percent)
            val pixels = SplashIconSize.pixels(160, density)
            val load = SplashIconSize.loadDensity(density, SplashIconSize.pixels(48, density), pixels)
            assertTrue(SplashIconSize.pixels(48, load) >= pixels)
        }
    }

    @Test
    fun fr3_an_icon_large_enough_or_without_a_size_stays_at_the_tasks_density() {
        assertEquals(320, SplashIconSize.loadDensity(320, 480, 320))
        assertEquals(320, SplashIconSize.loadDensity(320, 320, 320))
        // A color or a shape has no intrinsic size.
        assertEquals(320, SplashIconSize.loadDensity(320, -1, 320))
        assertEquals(320, SplashIconSize.loadDensity(320, 0, 320))
    }

    @Test
    fun fr3_a_tiny_icon_is_not_loaded_at_an_unbounded_density() {
        assertEquals(SplashIconSize.MAX_LOAD_DENSITY, SplashIconSize.loadDensity(640, 1, 4096))
    }
}

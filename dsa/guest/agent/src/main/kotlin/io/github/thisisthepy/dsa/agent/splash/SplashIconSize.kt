package io.github.thisisthepy.dsa.agent.splash

/**
 * How large a splash icon is in pixels, and the density its drawable is loaded at, so the icon
 * is drawn from pixels at least as fine as the ones it lands on.
 *
 * A task window shows the task's frames one guest pixel to one screen pixel, and the task
 * draws at the density of its window's monitor (320 dpi at 200%). An icon loaded at a lower
 * density, or at the size an app ships for a phone, has fewer pixels than the square it is
 * drawn into and would be scaled up, which blurs it. So an icon is loaded first at the task's
 * density, and when its bitmap is still smaller than where it is drawn, again at the density
 * at which it covers that square. The resources then pick their largest bitmap and scale it
 * once, down, to the exact size.
 */
object SplashIconSize {
    /** The density at which one dp is one pixel (DisplayMetrics.DENSITY_DEFAULT). */
    const val BASE_DENSITY = 160

    /** The most a drawable is ever loaded at: far above any bitmap an app ships. */
    const val MAX_LOAD_DENSITY = 10_000

    /** [sizeDp] in pixels at [densityDpi], rounded as the framework rounds a dimension. */
    fun pixels(sizeDp: Int, densityDpi: Int): Int = ((sizeDp.toLong() * densityDpi + BASE_DENSITY / 2) / BASE_DENSITY).toInt()

    /**
     * The density a splash is drawn at: the task's own, which follows its window's monitor,
     * and the display's only while the task has none.
     */
    fun drawDensity(taskDensityDpi: Int?, displayDensityDpi: Int): Int =
        taskDensityDpi?.takeIf { it > 0 } ?: displayDensityDpi

    /**
     * The density to load a drawable at so that it is at least [pixels] large, when loaded at
     * [densityDpi] it is [intrinsicPixels] large (its larger side). A drawable with no size of
     * its own (a color, or a size of 0 or less) and one already large enough stay at
     * [densityDpi].
     */
    fun loadDensity(densityDpi: Int, intrinsicPixels: Int, pixels: Int): Int {
        if (intrinsicPixels <= 0 || pixels <= 0 || intrinsicPixels >= pixels) return densityDpi
        val wanted = (densityDpi.toLong() * pixels + intrinsicPixels - 1) / intrinsicPixels
        return wanted.coerceAtMost(MAX_LOAD_DENSITY.toLong()).toInt()
    }
}

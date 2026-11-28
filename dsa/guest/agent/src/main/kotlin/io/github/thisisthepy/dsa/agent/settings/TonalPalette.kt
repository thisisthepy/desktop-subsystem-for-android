package io.github.thisisthepy.dsa.agent.settings

import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.cbrt
import kotlin.math.cos
import kotlin.math.hypot
import kotlin.math.pow
import kotlin.math.roundToInt
import kotlin.math.sin

/**
 * The guest's dynamic colors from a seed color: the five tonal palettes Material You apps
 * read (`system_accent1` to `system_accent3`, `system_neutral1` and `system_neutral2`), each
 * in its thirteen shades from 0 (white) to 1000 (black), and the color roles Material 3 apps
 * read (`system_primary_light`, `system_on_secondary_container_dark` and so on).
 *
 * A phone's SystemUI makes them from the wallpaper or a chosen color. The guest has no
 * SystemUI, so the agent makes them from the host's accent color, the way the image makes its
 * default ones from DSA's blue (make-image.py's dynamic_scheme, which this mirrors): the first
 * accent keeps the seed's hue and chroma, the second the same hue with little color, the third
 * the hue turned 60 degrees away from violet, and the neutrals the seed's hue nearly grey.
 * A shade's tone is its CIE L*, so text on a role always has the same contrast whatever the
 * hue; hue and chroma are OKLCh's, which keeps a hue as it gets lighter. The page and card
 * surfaces are not roles this sets: they stay the image's greys.
 */
object TonalPalette {
    /** Each shade, with the tone (CIE L*) it has. */
    val SHADES = listOf(
        0 to 100.0, 10 to 99.0, 50 to 95.0, 100 to 90.0, 200 to 80.0, 300 to 70.0,
        400 to 60.0, 500 to 50.0, 600 to 40.0, 700 to 30.0, 800 to 20.0, 900 to 10.0,
        1000 to 0.0,
    )

    /** Each palette: its hue turn from the seed's (degrees) and its OKLCh chroma. */
    private val PALETTES = listOf(
        "accent1" to Pair(0.0, 0.09),
        "accent2" to Pair(0.0, 0.04),
        "accent3" to Pair(-60.0, 0.06),
        "neutral1" to Pair(0.0, 0.008),
        "neutral2" to Pair(0.0, 0.018),
    )

    /** Each color role of the light theme: its palette and tone. */
    val LIGHT_ROLES = mapOf(
        "primary" to ("accent1" to 40.0), "on_primary" to ("accent1" to 100.0),
        "primary_container" to ("accent1" to 90.0), "on_primary_container" to ("accent1" to 10.0),
        "inverse_primary" to ("accent1" to 80.0), "surface_tint" to ("accent1" to 40.0),
        "primary_dim" to ("accent1" to 35.0), "control_activated" to ("accent1" to 90.0),
        "secondary" to ("accent2" to 40.0), "on_secondary" to ("accent2" to 100.0),
        "secondary_container" to ("accent2" to 90.0), "on_secondary_container" to ("accent2" to 10.0),
        "secondary_dim" to ("accent2" to 35.0),
        "tertiary" to ("accent3" to 40.0), "on_tertiary" to ("accent3" to 100.0),
        "tertiary_container" to ("accent3" to 90.0), "on_tertiary_container" to ("accent3" to 10.0),
        "tertiary_dim" to ("accent3" to 35.0),
        "on_background" to ("neutral1" to 10.0), "on_surface" to ("neutral1" to 10.0),
        "surface_variant" to ("neutral2" to 90.0), "on_surface_variant" to ("neutral2" to 30.0),
        "control_normal" to ("neutral2" to 30.0),
        "outline" to ("neutral2" to 50.0), "outline_variant" to ("neutral2" to 80.0),
        "inverse_surface" to ("neutral1" to 20.0), "inverse_on_surface" to ("neutral1" to 95.0),
    )

    /** Each color role of the dark theme: its palette and tone. */
    val DARK_ROLES = mapOf(
        "primary" to ("accent1" to 80.0), "on_primary" to ("accent1" to 20.0),
        "primary_container" to ("accent1" to 30.0), "on_primary_container" to ("accent1" to 90.0),
        "inverse_primary" to ("accent1" to 40.0), "surface_tint" to ("accent1" to 80.0),
        "primary_dim" to ("accent1" to 75.0), "control_activated" to ("accent1" to 30.0),
        "secondary" to ("accent2" to 80.0), "on_secondary" to ("accent2" to 20.0),
        "secondary_container" to ("accent2" to 30.0), "on_secondary_container" to ("accent2" to 90.0),
        "secondary_dim" to ("accent2" to 75.0),
        "tertiary" to ("accent3" to 80.0), "on_tertiary" to ("accent3" to 20.0),
        "tertiary_container" to ("accent3" to 30.0), "on_tertiary_container" to ("accent3" to 90.0),
        "tertiary_dim" to ("accent3" to 75.0),
        "on_background" to ("neutral1" to 90.0), "on_surface" to ("neutral1" to 90.0),
        "surface_variant" to ("neutral2" to 30.0), "on_surface_variant" to ("neutral2" to 80.0),
        "control_normal" to ("neutral2" to 80.0),
        "outline" to ("neutral2" to 60.0), "outline_variant" to ("neutral2" to 30.0),
        "inverse_surface" to ("neutral1" to 90.0), "inverse_on_surface" to ("neutral1" to 20.0),
    )

    /** The fixed roles, the same in both themes. */
    private val FIXED_ROLES = buildMap {
        for ((role, palette) in listOf("primary" to "accent1", "secondary" to "accent2", "tertiary" to "accent3")) {
            put("${role}_fixed", palette to 90.0)
            put("${role}_fixed_dim", palette to 80.0)
            put("on_${role}_fixed", palette to 10.0)
            put("on_${role}_fixed_variant", palette to 30.0)
        }
    }

    /** The palette for the seed [argb], by framework color name (such as `system_accent1_500`). */
    fun of(argb: Int): Map<String, Int> {
        val (_, seedChroma, seedHue) = lch(argb)
        val palettes = PALETTES.associate { (name, palette) ->
            val (turn, chroma) = palette
            name to Pair((seedHue + turn).mod(360.0), if (name == "accent1") maxOf(seedChroma, chroma) else chroma)
        }
        fun color(palette: String, tone: Double): Int {
            val (hue, chroma) = palettes.getValue(palette)
            return toneColor(hue, chroma, tone)
        }
        val colors = LinkedHashMap<String, Int>()
        for ((name, _) in PALETTES) {
            for ((shade, tone) in SHADES) {
                val value = color(name, tone)
                colors["system_${name}_$shade"] = value
                colors["system_${name}_${shade}_light"] = value
                colors["system_${name}_${shade}_dark"] = value
            }
        }
        for ((role, palette) in LIGHT_ROLES) colors["system_${role}_light"] = color(palette.first, palette.second)
        for ((role, palette) in DARK_ROLES) colors["system_${role}_dark"] = color(palette.first, palette.second)
        for ((role, palette) in FIXED_ROLES) colors["system_$role"] = color(palette.first, palette.second)
        return colors
    }

    private fun linear(channel: Int): Double {
        val c = channel / 255.0
        return if (c <= 0.04045) c / 12.92 else ((c + 0.055) / 1.055).pow(2.4)
    }

    private fun gamma(value: Double): Int {
        val v = if (value <= 0.0031308) 12.92 * value else 1.055 * value.coerceAtLeast(0.0).pow(1 / 2.4) - 0.055
        return (v.coerceIn(0.0, 1.0) * 255).roundToInt()
    }

    /** The relative luminance (0 to 1) of [argb], as contrast ratios use it. */
    fun luminance(argb: Int): Double =
        0.2126 * linear(argb shr 16 and 0xff) + 0.7152 * linear(argb shr 8 and 0xff) + 0.0722 * linear(argb and 0xff)

    /** The OKLCh lightness (0 to 100), chroma and hue (degrees) of [argb]. */
    fun lch(argb: Int): Triple<Double, Double, Double> {
        val r = linear(argb shr 16 and 0xff)
        val g = linear(argb shr 8 and 0xff)
        val b = linear(argb and 0xff)
        val l = cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
        val m = cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
        val s = cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
        val lightness = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s
        val a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s
        val bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s
        return Triple(lightness * 100, hypot(a, bb), Math.toDegrees(atan2(bb, a)).mod(360.0))
    }

    private fun linearRgb(lightness: Double, chroma: Double, hue: Double): DoubleArray {
        val a = chroma * cos(Math.toRadians(hue))
        val b = chroma * sin(Math.toRadians(hue))
        val ok = lightness / 100
        val l = (ok + 0.3963377774 * a + 0.2158037573 * b).pow(3)
        val m = (ok - 0.1055613458 * a - 0.0638541728 * b).pow(3)
        val s = (ok - 0.0894841775 * a - 1.2914855480 * b).pow(3)
        return doubleArrayOf(
            4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
            -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
            -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
        )
    }

    /**
     * The opaque color of [tone] (CIE L*) at the OKLCh [hue], with [chroma] or, where the
     * screen cannot show that much, the most it can.
     */
    fun toneColor(hue: Double, chroma: Double, tone: Double): Int {
        if (tone >= 100) return 0xFFFFFFFF.toInt()
        if (tone <= 0) return 0xFF000000.toInt()
        // The OKLab lightness of a grey with that L*: the cube root of its luminance.
        val f = (tone + 16) / 116
        val luminance = if (f * f * f > 216.0 / 24389) f * f * f else (116 * f - 16) / (24389.0 / 27)
        val lightness = 100 * cbrt(luminance)
        fun inside(c: Double) = linearRgb(lightness, c, hue).all { it >= -1e-4 && it <= 1 + 1e-4 }
        var best = chroma
        if (!inside(best)) {
            var low = 0.0
            var high = chroma
            repeat(24) {
                val middle = (low + high) / 2
                if (inside(middle)) low = middle else high = middle
            }
            best = low
        }
        val (r, g, b) = linearRgb(lightness, best, hue).map(::gamma)
        return (0xff shl 24) or (r shl 16) or (g shl 8) or b
    }

    /** The hue (0 to 360), saturation and lightness (0 to 1) of [argb]. */
    fun hsl(argb: Int): Triple<Double, Double, Double> {
        val r = (argb shr 16 and 0xff) / 255.0
        val g = (argb shr 8 and 0xff) / 255.0
        val b = (argb and 0xff) / 255.0
        val max = maxOf(r, g, b)
        val min = minOf(r, g, b)
        val lightness = (max + min) / 2
        val delta = max - min
        if (delta == 0.0) return Triple(0.0, 0.0, lightness)
        val saturation = delta / (1 - abs(2 * lightness - 1))
        val hue = when (max) {
            r -> 60 * (((g - b) / delta).mod(6.0))
            g -> 60 * ((b - r) / delta + 2)
            else -> 60 * ((r - g) / delta + 4)
        }
        return Triple(hue, saturation.coerceIn(0.0, 1.0), lightness)
    }
}

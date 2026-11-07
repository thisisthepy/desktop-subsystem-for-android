package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.content.res.Resources
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Drawable
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.splash.SplashIconSize
import io.github.thisisthepy.dsa.agent.tasks.AppDetails
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import java.io.ByteArrayOutputStream

/**
 * An installed app's name and launcher icon, from the package manager, for the app's own
 * window program on the host.
 *
 * The icon is drawn at [WINDOW_ICON_SIZE] pixels. The host draws the window's splash from it
 * before the app's first frame, 160 dp large like the app's own splash, which is 320 pixels on
 * a 200% monitor; at 512 the host scales it down on every monitor up to 320%, and never up.
 * The host makes the program's 256 pixel icon from it too. Adaptive icons are drawn with the
 * launcher's mask, as a launcher shows them.
 */
class AndroidAppInfoSource(
    private val context: Context,
    private val log: Log,
) : AppInfoSource {
    override fun appInfo(packageName: String): AppDetails? {
        val pm = context.packageManager
        val app = try {
            pm.getApplicationInfo(packageName, 0)
        } catch (e: PackageManager.NameNotFoundException) {
            return null
        }
        val label = app.loadLabel(pm).toString().ifEmpty { packageName }
        val icon = try {
            val large = sharpIconPng(pm, app, app.icon, WINDOW_ICON_SIZE) { pm.getApplicationIcon(app) }
            // A busy icon can compress poorly at this size; the host keeps icons up to 1 MiB.
            if (large.size <= MAX_WINDOW_ICON_BYTES) large else sharpIconPng(pm, app, app.icon) { pm.getApplicationIcon(app) }
        } catch (e: Exception) {
            log.warn("could not draw the icon of $packageName", e)
            ByteArray(0)
        }
        return AppDetails(label, icon)
    }
}

/** The largest size the host's taskbar, task list and start menu show an icon at. */
private const val ICON_SIZE = 256

/** The size of the icon an app's window program and its splash are made from. */
internal const val WINDOW_ICON_SIZE = 512

/** The most bytes the window program's icon takes before it is drawn at [ICON_SIZE] instead. */
private const val MAX_WINDOW_ICON_BYTES = 768 * 1024

/** [drawable] drawn as a square PNG image of [size] pixels on a side. */
internal fun iconPng(drawable: Drawable, size: Int = ICON_SIZE): ByteArray {
    val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    drawable.setBounds(0, 0, size, size)
    drawable.draw(canvas)
    val out = ByteArrayOutputStream()
    bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
    bitmap.recycle()
    return out.toByteArray()
}

/**
 * The icon [iconRes] of [app] drawn as a square PNG of [size] pixels, loaded at the density at
 * which its bitmaps are at least that large.
 *
 * The package manager loads an icon at the agent's own density, the guest display's 160 dpi,
 * where a launcher icon is 48 pixels and an adaptive icon's layers 108: drawn into 256 pixels
 * that is scaled up five times and blurred. [fallback] is the icon as the package manager
 * gives it, for an app whose icon cannot be read by resource.
 */
internal fun sharpIconPng(pm: PackageManager, app: ApplicationInfo, iconRes: Int, size: Int = ICON_SIZE, fallback: () -> Drawable): ByteArray {
    val drawable = if (iconRes == 0) {
        null
    } else {
        try {
            val resources = pm.getResourcesForApplication(app)
            val density = resources.configuration.densityDpi
            val load = { at: Int ->
                try {
                    resources.getDrawableForDensity(iconRes, at, null)
                } catch (e: Resources.NotFoundException) {
                    null
                }
            }
            load(density)?.let { first -> sharpDrawable(first, density, size, load) }
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
    }
    return iconPng(drawable ?: fallback(), size)
}

/**
 * [loaded], which was loaded at [densityDpi], or the same drawable loaded by [load] at the
 * higher density at which it is at least [pixels] large, when it is smaller.
 */
internal fun sharpDrawable(loaded: Drawable, densityDpi: Int, pixels: Int, load: (Int) -> Drawable?): Drawable {
    val intrinsic = maxOf(loaded.intrinsicWidth, loaded.intrinsicHeight)
    val wanted = SplashIconSize.loadDensity(densityDpi, intrinsic, pixels)
    if (wanted == densityDpi) return loaded
    return load(wanted) ?: loaded
}

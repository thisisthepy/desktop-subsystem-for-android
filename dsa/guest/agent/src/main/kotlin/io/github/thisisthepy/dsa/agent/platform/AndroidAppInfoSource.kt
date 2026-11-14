package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.content.res.Resources
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Drawable
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.splash.SplashIconSize
import io.github.thisisthepy.dsa.agent.tasks.AppDetails
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import io.github.thisisthepy.dsa.agent.tasks.CaptionIcon
import java.io.ByteArrayOutputStream

/**
 * An installed app's name and launcher icon, from the package manager, for the app's own
 * window program on the host.
 *
 * The icon is drawn at [WINDOW_ICON_SIZE] pixels. The host draws the window's splash from it
 * before the app's first frame, 160 dp large like the app's own splash, which is 262 pixels on
 * a 200% monitor; at 512 the host scales it down on every monitor up to 390%, and never up.
 * The host makes the program's 256 pixel icon from it too. Adaptive icons are drawn with the
 * launcher's mask, as a launcher shows them.
 *
 * For an adaptive icon the window's title bar gets its foreground layer alone, drawn at the
 * scale the whole icon is drawn at ([CaptionIcon.layerBounds]), so the app's shape shows
 * without the square or round plate behind it, at the same size the full icon would have.
 *
 * Whether the app supports several windows is what Android's own desktop windows read to offer
 * New window: the property android.window.PROPERTY_SUPPORTS_MULTI_INSTANCE_SYSTEM_UI on the
 * app's launcher activity, else on the app.
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
        var size = WINDOW_ICON_SIZE
        // The icon a launcher shows, which the Start menu shows too: the launcher activity's,
        // which may differ from the app's own, as an activity alias's often does.
        val iconRes = try {
            launcherIconResource(pm, packageName) ?: app.icon
        } catch (e: RuntimeException) {
            app.icon
        }
        val drawable = try {
            sharpIcon(pm, app, iconRes, WINDOW_ICON_SIZE) { pm.getApplicationIcon(app) }
        } catch (e: Exception) {
            log.warn("could not load the icon of $packageName", e)
            null
        }
        val icon = try {
            if (drawable == null) {
                ByteArray(0)
            } else {
                val large = iconPng(drawable, WINDOW_ICON_SIZE)
                // A busy icon can compress poorly at this size; the host keeps icons up to 1 MiB.
                if (large.size <= MAX_WINDOW_ICON_BYTES) {
                    large
                } else {
                    size = ICON_SIZE
                    sharpIconPng(pm, app, iconRes) { pm.getApplicationIcon(app) }
                }
            }
        } catch (e: Exception) {
            log.warn("could not draw the icon of $packageName", e)
            ByteArray(0)
        }
        val captionIcon = try {
            if (icon.isEmpty() || drawable == null) ByteArray(0) else foregroundPng(drawable, size) ?: ByteArray(0)
        } catch (e: Exception) {
            log.warn("could not draw the foreground of the icon of $packageName; its title bar shows the whole icon", e)
            ByteArray(0)
        }
        return AppDetails(label, icon, captionIcon, supportsMultipleWindows(pm, packageName))
    }
}

/**
 * The icon resource of [packageName]'s launcher activity, as the list of installed apps draws
 * it (the activity's or alias's own icon, else the app's); null when the app has no launcher
 * activity of its own or none with an icon.
 */
internal fun launcherIconResource(pm: PackageManager, packageName: String): Int? {
    val launch = pm.getLaunchIntentForPackage(packageName) ?: return null
    val resolved = pm.resolveActivity(launch, PackageManager.ResolveInfoFlags.of(0)) ?: return null
    // A match whose icon comes from another package cannot be loaded from this app's resources.
    if (resolved.resolvePackageName != null || resolved.activityInfo?.packageName != packageName) return null
    return resolved.iconResource.takeIf { it != 0 }
}

/**
 * Whether the app's launcher activity, or else the app, declares that it supports several
 * windows (WindowManager.PROPERTY_SUPPORTS_MULTI_INSTANCE_SYSTEM_UI), as Android's desktop
 * windows ask before they offer New window. False when it declares nothing. The host offers New
 * window by it, and the agent checks it again before it starts another task.
 */
internal fun supportsMultipleWindows(pm: PackageManager, packageName: String): Boolean {
    fun read(get: () -> PackageManager.Property): Boolean? = try {
        get().boolean
    } catch (e: PackageManager.NameNotFoundException) {
        null
    } catch (e: RuntimeException) {
        null
    }
    val launcher = pm.getLaunchIntentForPackage(packageName)?.component
    if (launcher != null) {
        read { pm.getProperty(PROPERTY_SUPPORTS_MULTI_INSTANCE, launcher) }?.let { return it }
    }
    return read { pm.getProperty(PROPERTY_SUPPORTS_MULTI_INSTANCE, packageName) } ?: false
}

// WindowManager.PROPERTY_SUPPORTS_MULTI_INSTANCE_SYSTEM_UI, which the SDK hides.
private const val PROPERTY_SUPPORTS_MULTI_INSTANCE = "android.window.PROPERTY_SUPPORTS_MULTI_INSTANCE_SYSTEM_UI"

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
 * The package manager loads an icon at the agent's own density, the guest display's (130 dpi
 * on a 100% main monitor), where a launcher icon is 39 pixels and an adaptive icon's layers
 * 88: drawn into 256 pixels that is scaled up more than six times and blurred. [fallback] is the icon as the package manager
 * gives it, for an app whose icon cannot be read by resource.
 */
internal fun sharpIconPng(pm: PackageManager, app: ApplicationInfo, iconRes: Int, size: Int = ICON_SIZE, fallback: () -> Drawable): ByteArray =
    iconPng(sharpIcon(pm, app, iconRes, size, fallback), size)

/**
 * [drawable]'s foreground layer alone as a square PNG image of [size] pixels on a side, at the
 * scale the whole icon is drawn at; null when [drawable] is not an adaptive icon or has no
 * foreground.
 */
internal fun foregroundPng(drawable: Drawable, size: Int): ByteArray? {
    val adaptive = drawable as? AdaptiveIconDrawable ?: return null
    val foreground = adaptive.foreground ?: return null
    val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    val bounds = CaptionIcon.layerBounds(size)
    foreground.setBounds(bounds.left, bounds.top, bounds.right, bounds.bottom)
    foreground.draw(canvas)
    val out = ByteArrayOutputStream()
    bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
    bitmap.recycle()
    return out.toByteArray()
}

/** The icon [iconRes] of [app] as [sharpIconPng] loads it, before it is drawn. */
internal fun sharpIcon(pm: PackageManager, app: ApplicationInfo, iconRes: Int, size: Int, fallback: () -> Drawable): Drawable {
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
    return drawable ?: fallback()
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

package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.AppDetails
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import java.io.ByteArrayOutputStream

/**
 * An installed app's name and launcher icon, from the package manager.
 *
 * The icon is drawn at [ICON_SIZE] pixels, which is the largest size the host's taskbar and
 * task list show, so the host scales it down rather than up. Adaptive icons are drawn with
 * the launcher's mask, as a launcher shows them.
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
            png(pm.getApplicationIcon(app))
        } catch (e: Exception) {
            log.warn("could not draw the icon of $packageName", e)
            ByteArray(0)
        }
        return AppDetails(label, icon)
    }

    private fun png(drawable: android.graphics.drawable.Drawable): ByteArray {
        val bitmap = Bitmap.createBitmap(ICON_SIZE, ICON_SIZE, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        drawable.setBounds(0, 0, ICON_SIZE, ICON_SIZE)
        drawable.draw(canvas)
        val out = ByteArrayOutputStream()
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
        bitmap.recycle()
        return out.toByteArray()
    }

    private companion object {
        const val ICON_SIZE = 256
    }
}

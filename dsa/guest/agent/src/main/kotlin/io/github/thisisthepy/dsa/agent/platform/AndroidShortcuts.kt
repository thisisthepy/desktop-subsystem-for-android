package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.Intent
import android.content.pm.LauncherApps
import android.content.pm.PackageManager
import android.content.pm.ShortcutInfo
import android.os.Process
import android.os.UserHandle
import android.util.DisplayMetrics
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.LauncherShortcut
import io.github.thisisthepy.dsa.agent.tasks.PinRequest
import io.github.thisisthepy.dsa.agent.tasks.ShortcutSource

/**
 * The apps' launcher shortcuts, read as the guest's default launcher reads them.
 *
 * The agent holds the guest's home role (its home activity is the only one), and Android lets
 * the default launcher read and start every app's shortcuts (LauncherApps.hasShortcutHostPermission);
 * no permission of its own is needed. The static, dynamic and pinned shortcuts of each app
 * are read in one query; the ones that are turned off are left out.
 */
class AndroidShortcuts(
    private val context: Context,
    private val log: Log,
) : ShortcutSource {
    private val launcher: LauncherApps? = context.getSystemService(LauncherApps::class.java)
    private var warnedNoPermission = false

    override fun shortcuts(packageName: String): List<LauncherShortcut>? {
        val launcher = launcher ?: return null
        if (!hostPermission(launcher)) return null
        val query = LauncherApps.ShortcutQuery()
            .setPackage(packageName)
            .setQueryFlags(
                LauncherApps.ShortcutQuery.FLAG_MATCH_MANIFEST or
                    LauncherApps.ShortcutQuery.FLAG_MATCH_DYNAMIC or
                    LauncherApps.ShortcutQuery.FLAG_MATCH_PINNED,
            )
        val found = try {
            launcher.getShortcuts(query, Process.myUserHandle())
        } catch (e: SecurityException) {
            log.warn("Android refused to list the shortcuts of $packageName", e)
            return null
        } catch (e: IllegalStateException) {
            // The user's storage is still locked; the shortcuts are read once it is unlocked.
            return null
        } ?: return emptyList()
        return found.filter { it.isEnabled }.map { shortcut(launcher, it) }
    }

    override fun listen(onChange: (String) -> Unit) {
        val launcher = launcher ?: return
        launcher.registerCallback(object : LauncherApps.Callback() {
            override fun onShortcutsChanged(packageName: String, shortcuts: MutableList<ShortcutInfo>, user: UserHandle) {
                if (user == Process.myUserHandle()) onChange(packageName)
            }

            // The package broadcasts already refresh the list of apps, and their shortcuts with it.
            override fun onPackageRemoved(packageName: String, user: UserHandle) = Unit
            override fun onPackageAdded(packageName: String, user: UserHandle) = Unit
            override fun onPackageChanged(packageName: String, user: UserHandle) = Unit
            override fun onPackagesAvailable(packageNames: Array<out String>, user: UserHandle, replacing: Boolean) = Unit
            override fun onPackagesUnavailable(packageNames: Array<out String>, user: UserHandle, replacing: Boolean) = Unit
        })
    }

    private fun hostPermission(launcher: LauncherApps): Boolean {
        val held = try {
            launcher.hasShortcutHostPermission()
        } catch (e: Exception) {
            false
        }
        if (!held && !warnedNoPermission) {
            warnedNoPermission = true
            log.warn("the agent is not the guest's default launcher, so it cannot read the apps' shortcuts")
        }
        return held
    }

    /**
     * The app's request to pin a shortcut that [intent] (LauncherApps.ACTION_CONFIRM_PIN_SHORTCUT)
     * carries; null for any other request, or one Android no longer takes.
     */
    fun pinRequest(intent: Intent?): PinRequest? {
        val launcher = launcher ?: return null
        val request = try {
            launcher.getPinItemRequest(intent ?: return null)
        } catch (e: Exception) {
            log.warn("could not read an app's pin request", e)
            null
        } ?: return null
        if (request.requestType != LauncherApps.PinItemRequest.REQUEST_TYPE_SHORTCUT || !request.isValid) return null
        val info = request.shortcutInfo ?: return null
        val packageName = info.`package`
        return AndroidPinRequest(request, packageName, appLabel(packageName), shortcut(launcher, info).withPinned())
    }

    private fun appLabel(packageName: String): String = try {
        val pm = context.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))).toString()
    } catch (e: PackageManager.NameNotFoundException) {
        packageName
    }.ifEmpty { packageName }

    private fun shortcut(launcher: LauncherApps, info: ShortcutInfo): LauncherShortcut {
        val icon = try {
            launcher.getShortcutIconDrawable(info, DisplayMetrics.DENSITY_XXXHIGH)?.let { iconPng(it) }
        } catch (e: Exception) {
            log.warn("could not draw the icon of the shortcut ${info.id} of ${info.`package`}", e)
            null
        } ?: ByteArray(0)
        return LauncherShortcut(
            id = info.id,
            shortLabel = info.shortLabel?.toString().orEmpty().ifEmpty { info.longLabel?.toString().orEmpty() },
            longLabel = info.longLabel?.toString().orEmpty(),
            iconPng = icon,
            rank = info.rank,
            manifest = info.isDeclaredInManifest,
            dynamic = info.isDynamic,
            pinned = info.isPinned,
        )
    }

    private fun LauncherShortcut.withPinned() = LauncherShortcut(
        id, shortLabel, longLabel, iconPng, rank, manifest, dynamic, pinned = true,
    )
}

/** An app's pin request as Android gave it to the default launcher. */
private class AndroidPinRequest(
    private val request: LauncherApps.PinItemRequest,
    override val packageName: String,
    override val appLabel: String,
    override val shortcut: LauncherShortcut,
) : PinRequest {
    override fun accept(): Boolean = request.isValid && request.accept()
}

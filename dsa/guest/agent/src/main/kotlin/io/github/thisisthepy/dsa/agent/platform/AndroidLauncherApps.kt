package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.app.usage.StorageStatsManager
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Process
import android.os.storage.StorageManager
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.LauncherApp
import io.github.thisisthepy.dsa.agent.tasks.LauncherApps

/**
 * The installed apps a launcher lists: those with an activity in the launcher category, from
 * the package manager. An app with several such activities is listed once, by the first, which
 * is the one the agent starts when the host launches the app.
 *
 * The package manager's broadcasts say when a package is added, replaced, changed (an
 * activity turned on or off) or removed; a change of language renames apps, so it counts too.
 */
class AndroidLauncherApps(
    private val context: Context,
    private val log: Log,
) : LauncherApps {
    override fun list(): List<LauncherApp> {
        val pm = context.packageManager
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        val activities = pm.queryIntentActivities(launcher, PackageManager.ResolveInfoFlags.of(0))
        val apps = LinkedHashMap<String, LauncherApp>()
        for (activity in activities) {
            val packageName = activity.activityInfo?.packageName ?: continue
            if (packageName in apps) continue
            val label = activity.loadLabel(pm).toString().ifEmpty { packageName }
            val icon = try {
                val iconRes = if (activity.resolvePackageName == null) activity.iconResource else 0
                sharpIconPng(pm, activity.activityInfo.applicationInfo, iconRes) { activity.loadIcon(pm) }
            } catch (e: Exception) {
                log.warn("could not draw the icon of $packageName", e)
                ByteArray(0)
            }
            apps[packageName] = details(pm, packageName, label, icon)
        }
        return apps.values.toList()
    }

    /**
     * The app with its version, dates and sizes. What cannot be read stays empty, so the app is
     * still listed: the sizes need the usage stats permission, which a broken grant could take.
     */
    private fun details(pm: PackageManager, packageName: String, label: String, icon: ByteArray): LauncherApp {
        val info = try {
            pm.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
        } catch (e: PackageManager.NameNotFoundException) {
            return LauncherApp(packageName, label, icon)
        }
        val flags = info.applicationInfo?.flags ?: 0
        val system = flags and (ApplicationInfo.FLAG_SYSTEM or ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) != 0
        var appBytes = 0L
        var dataBytes = 0L
        var cacheBytes = 0L
        try {
            val stats = context.getSystemService(StorageStatsManager::class.java)
                .queryStatsForPackage(StorageManager.UUID_DEFAULT, packageName, Process.myUserHandle())
            appBytes = stats.appBytes
            cacheBytes = stats.cacheBytes
            // The data the stats report includes the cache; the host shows them apart.
            dataBytes = maxOf(0L, stats.dataBytes - stats.cacheBytes)
        } catch (e: Exception) {
            if (!warnedSizes) {
                warnedSizes = true
                log.warn("could not read the apps' storage sizes; they are reported as zero", e)
            }
        }
        val application = info.applicationInfo
        // Deprecated for apps to declare from Android 13, but still what the package manager
        // groups an installed app's uid by.
        @Suppress("DEPRECATION")
        val sharedUserId = info.sharedUserId.orEmpty()
        return LauncherApp(
            packageName = packageName,
            label = label,
            iconPng = icon,
            versionName = info.versionName.orEmpty(),
            versionCode = info.longVersionCode,
            firstInstallTimeMs = info.firstInstallTime,
            lastUpdateTimeMs = info.lastUpdateTime,
            systemApp = system,
            appBytes = appBytes,
            dataBytes = dataBytes,
            cacheBytes = cacheBytes,
            primaryAbi = application?.let { AndroidApks.primaryAbi(it) }.orEmpty(),
            nativeLibraries = application?.let { apks.libraries(it) }.orEmpty(),
            sharedUserId = sharedUserId,
        )
    }

    private val apks = AndroidApks()

    private var warnedSizes = false

    override fun listen(onChange: () -> Unit) {
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) = onChange()
        }
        val packages = IntentFilter().apply {
            addAction(Intent.ACTION_PACKAGE_ADDED)
            addAction(Intent.ACTION_PACKAGE_REPLACED)
            addAction(Intent.ACTION_PACKAGE_CHANGED)
            addAction(Intent.ACTION_PACKAGE_REMOVED)
            addDataScheme("package")
        }
        // The agent starts before the user's storage is unlocked, when only the apps that run
        // that early are listed; the rest appear once it is.
        val system = IntentFilter().apply {
            addAction(Intent.ACTION_LOCALE_CHANGED)
            addAction(Intent.ACTION_USER_UNLOCKED)
        }
        // Only the system sends these, so no other app can make the agent read the list.
        context.registerReceiver(receiver, packages, Context.RECEIVER_NOT_EXPORTED)
        context.registerReceiver(receiver, system, Context.RECEIVER_NOT_EXPORTED)
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
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
                iconPng(activity.loadIcon(pm))
            } catch (e: Exception) {
                log.warn("could not draw the icon of $packageName", e)
                ByteArray(0)
            }
            apps[packageName] = LauncherApp(packageName, label, icon)
        }
        return apps.values.toList()
    }

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

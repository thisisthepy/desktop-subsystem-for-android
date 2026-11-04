package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.content.Context
import android.content.pm.PackageManager
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.memory.AppProcesses
import java.io.File

/**
 * The guest's app processes through the activity manager. Ending an app is
 * `ActivityManager.forceStopPackage`, which is not in the SDK and is called by reflection; the
 * agent is a platform app and holds FORCE_STOP_PACKAGES. Its running services are visible
 * because the agent is a system app, which `getRunningServices` answers for every app.
 */
class AndroidAppProcesses(context: Context) : AppProcesses {
    private val activities = context.getSystemService(ActivityManager::class.java)
    private val packages = context.packageManager
    private val resolver = context.contentResolver

    private companion object {
        const val AM_CONSTANTS = "activity_manager_constants"
        const val MAX_CACHED = "max_cached_processes"
    }

    @Suppress("DEPRECATION")
    override fun hasForegroundService(packageName: String): Boolean =
        activities.getRunningServices(Int.MAX_VALUE).orEmpty()
            .any { it.foreground && it.service.packageName == packageName }

    override fun stop(packageName: String) {
        ActivityManager::class.java
            .getMethod("forceStopPackage", String::class.java)
            .invoke(activities, packageName)
    }

    override fun disable(packageName: String): Boolean {
        val state = try {
            packages.getApplicationEnabledSetting(packageName)
        } catch (e: IllegalArgumentException) {
            return false // not installed
        }
        if (state == PackageManager.COMPONENT_ENABLED_STATE_DISABLED) return false
        packages.setApplicationEnabledSetting(packageName, PackageManager.COMPONENT_ENABLED_STATE_DISABLED, 0)
        return true
    }

    /**
     * The activity manager reads its limits from this global setting (a list of key=value);
     * only the cached process limit is set, and only when nothing else has set the setting.
     */
    override fun limitCachedProcesses(max: Int) {
        val current = Settings.Global.getString(resolver, AM_CONSTANTS)
        if (current.isNullOrEmpty() || current.startsWith("$MAX_CACHED=")) {
            Settings.Global.putString(resolver, AM_CONSTANTS, "$MAX_CACHED=$max")
        }
    }

    override fun freeKb(): Long =
        File("/proc/meminfo").useLines { lines ->
            lines.firstOrNull { it.startsWith("MemFree:") }
                ?.substringAfter(':')?.trim()?.substringBefore(' ')?.toLongOrNull() ?: 0L
        }
}

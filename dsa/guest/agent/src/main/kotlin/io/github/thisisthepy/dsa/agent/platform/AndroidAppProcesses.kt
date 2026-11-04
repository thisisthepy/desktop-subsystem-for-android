package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.content.Context
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

    @Suppress("DEPRECATION")
    override fun hasForegroundService(packageName: String): Boolean =
        activities.getRunningServices(Int.MAX_VALUE).orEmpty()
            .any { it.foreground && it.service.packageName == packageName }

    override fun stop(packageName: String) {
        ActivityManager::class.java
            .getMethod("forceStopPackage", String::class.java)
            .invoke(activities, packageName)
    }

    override fun freeKb(): Long =
        File("/proc/meminfo").useLines { lines ->
            lines.firstOrNull { it.startsWith("MemFree:") }
                ?.substringAfter(':')?.trim()?.substringBefore(' ')?.toLongOrNull() ?: 0L
        }
}

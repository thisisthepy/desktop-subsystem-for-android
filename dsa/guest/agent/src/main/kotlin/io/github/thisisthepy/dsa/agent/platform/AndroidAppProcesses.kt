package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.content.Context
import android.content.pm.PackageManager
import android.provider.DeviceConfig
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.memory.AppProcesses
import io.github.thisisthepy.dsa.agent.memory.GuestMemInfo
import io.github.thisisthepy.dsa.agent.memory.PackageMemory
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

    private val mainExecutor = context.mainExecutor
    private var watching = false

    private companion object {
        const val AM_CONSTANTS = "activity_manager_constants"
        const val NAMESPACE = "activity_manager"
        const val MAX_CACHED = "max_cached_processes"
    }

    @Suppress("DEPRECATION")
    override fun hasForegroundService(packageName: String): Boolean =
        activities.getRunningServices(Int.MAX_VALUE).orEmpty()
            .any { it.foreground && it.service.packageName == packageName }

    /**
     * A process of the package whose importance is foreground or visible: the activity
     * manager ranks a process so while one of its activities is resumed or seen, in any task.
     * A foreground service alone ranks it lower and is not counted here.
     */
    override fun showsActivity(packageName: String): Boolean =
        activities.runningAppProcesses.orEmpty().any { info ->
            (info.processName == packageName || info.processName.startsWith("$packageName:")) &&
                (
                    info.importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND ||
                        info.importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_VISIBLE
                    )
        }

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
     * The activity manager reads the cached process limit from the device configuration
     * (namespace activity_manager, key max_cached_processes), and only without one from the
     * framework's config_customizedMaxCachedProcesses, which the image's framework overlay sets
     * to the same limit. The global setting activity_manager_constants no longer carries it:
     * Android 17 parses that setting for its other limits and ignores this key, so the limit
     * written there before never applied and the activity manager kept up to 1024.
     *
     * The flag is set now, takes effect at once, and is set again whenever something else
     * changes it (Google Play services' flag sync may), so the limit holds for as long as the
     * agent runs. What earlier agents wrote to the global setting is taken back.
     */
    override fun limitCachedProcesses(max: Int) {
        val wanted = max.toString()
        if (DeviceConfig.getProperty(NAMESPACE, MAX_CACHED) != wanted) {
            DeviceConfig.setProperty(NAMESPACE, MAX_CACHED, wanted, false)
        }
        if (!watching) {
            watching = true
            DeviceConfig.addOnPropertiesChangedListener(NAMESPACE, mainExecutor) { changed ->
                if (MAX_CACHED in changed.keyset && changed.getString(MAX_CACHED, null) != wanted) {
                    DeviceConfig.setProperty(NAMESPACE, MAX_CACHED, wanted, false)
                }
            }
        }
        val legacy = Settings.Global.getString(resolver, AM_CONSTANTS)
        if (legacy != null && legacy.matches(Regex("$MAX_CACHED=\\d+"))) {
            Settings.Global.putString(resolver, AM_CONSTANTS, null)
        }
    }

    override fun memory(): GuestMemInfo? {
        val fields = HashMap<String, Long>()
        File("/proc/meminfo").useLines { lines ->
            for (line in lines) {
                val name = line.substringBefore(':')
                if (name != "MemTotal" && name != "MemFree" && name != "MemAvailable") continue
                line.substringAfter(':').trim().substringBefore(' ').toLongOrNull()?.let { fields[name] = it }
            }
        }
        return GuestMemInfo(
            totalKb = fields["MemTotal"] ?: return null,
            freeKb = fields["MemFree"] ?: return null,
            availableKb = fields["MemAvailable"] ?: return null,
        )
    }

    /**
     * The package's processes from `getRunningAppProcesses` (every process, as the agent holds
     * REAL_GET_TASKS), by process name: the package's own and `<package>:<name>`. What they hold
     * is `getProcessMemoryInfo`, which the agent, a privileged app, may ask of any process.
     */
    override fun packageMemory(packageName: String): PackageMemory {
        val pids = activities.runningAppProcesses.orEmpty()
            .filter { it.processName == packageName || it.processName.startsWith("$packageName:") }
            .map { it.pid }
            .sorted()
        if (pids.isEmpty()) return PackageMemory(emptyList(), 0, 0)
        val info = activities.getProcessMemoryInfo(pids.toIntArray())
        val pss = info.sumOf { it.totalPss.toLong() }
        val swapped = info.sumOf { SwapPss.of(it).coerceIn(0, it.totalPss.toLong().coerceAtLeast(0)) }
        return PackageMemory(pids, pss, swapped)
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.provider.DeviceConfig
import io.github.thisisthepy.dsa.agent.memory.AppMemoryPort
import io.github.thisisthepy.dsa.agent.memory.CompileRecord
import io.github.thisisthepy.dsa.agent.memory.InstalledPackage
import java.io.File

/**
 * The request and answer properties of the image's app memory scripts (dsa-app-memory.rc),
 * which init lets the agent set and read (sepolicy/app-memory.cil); SystemProperties is not in
 * the SDK and is called by reflection. The records of the apps the user opens are kept in the
 * agent's own preferences, one line per package.
 */
class AndroidAppMemory(private val context: Context) : AppMemoryPort {
    private val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    private fun get(name: String): String =
        Class.forName("android.os.SystemProperties")
            .getMethod("get", String::class.java)
            .invoke(null, name) as String

    private fun set(name: String, value: String) {
        Class.forName("android.os.SystemProperties")
            .getMethod("set", String::class.java, String::class.java)
            .invoke(null, name, value)
    }

    override fun requestTrim(value: String) = set(TRIM, value)

    override fun trimDone(): String = get("$TRIM.done")

    /**
     * Asks the activity manager's dsa-hidden-trim shell command, which DSA's framework adds, to
     * answer. A framework without that change answers "Unknown command", which [serviceShell]
     * turns into a failure.
     */
    override fun hiddenTrimSupported(): Boolean = try {
        serviceShell(ACTIVITY_SERVICE, HIDDEN_TRIM_COMMAND) == "supported"
    } catch (e: Exception) {
        false
    }

    /**
     * Sets the apps the host hides through the activity manager's dsa-hidden-trim shell command
     * (`cmd activity dsa-hidden-trim hidden [PACKAGE...]`), which needs the
     * MANAGE_ACTIVITY_TASKS permission the agent holds. The trim script, which runs as the
     * shell, then sends them their trims with `cmd activity dsa-hidden-trim send`.
     */
    override fun setHiddenApps(packages: Set<String>): Boolean = try {
        serviceShell(ACTIVITY_SERVICE, HIDDEN_TRIM_COMMAND, "hidden", *packages.sorted().toTypedArray())
        true
    } catch (e: Exception) {
        false
    }

    override fun requestCompile(value: String) = set(COMPILE, value)

    override fun compileDone(): String = get("$COMPILE.done")

    override fun installed(packageName: String): InstalledPackage? {
        val info = try {
            context.packageManager.getPackageInfo(packageName, 0)
        } catch (e: PackageManager.NameNotFoundException) {
            return null
        }
        val app = info.applicationInfo ?: return null
        val apks = listOfNotNull(app.sourceDir) + app.splitSourceDirs.orEmpty()
        return InstalledPackage(info.lastUpdateTime, apks.sumOf { File(it).length() })
    }

    override fun records(): Map<String, CompileRecord> =
        preferences.all.mapNotNull { (packageName, value) ->
            val fields = (value as? String)?.split(',') ?: return@mapNotNull null
            if (fields.size < 4) return@mapNotNull null
            packageName to CompileRecord(
                opens = fields[0].toIntOrNull() ?: 0,
                compiledUpdate = fields[1].toLongOrNull() ?: 0,
                result = fields[2],
                compiledAtMillis = fields[3].toLongOrNull() ?: 0,
            )
        }.toMap()

    override fun save(packageName: String, record: CompileRecord) {
        val line = "${record.opens},${record.compiledUpdate},${record.result.replace(',', ' ')},${record.compiledAtMillis}"
        preferences.edit().putString(packageName, line).apply()
    }

    override fun wallMillis(): Long = System.currentTimeMillis()

    /**
     * The activity manager compacts a process only while its compaction is on (namespace
     * activity_manager, use_compaction), which is the default but a device configuration sync
     * may turn off; the agent holds WRITE_DEVICE_CONFIG. An update sends ACTION_PACKAGE_ADDED
     * and ACTION_PACKAGE_REPLACED; hearing both costs nothing.
     */
    override fun start(onPackageChanged: (String) -> Unit) {
        try {
            if (DeviceConfig.getProperty(NAMESPACE, USE_COMPACTION) == "false") {
                DeviceConfig.setProperty(NAMESPACE, USE_COMPACTION, "true", false)
            }
        } catch (e: SecurityException) {
            // The compaction the trim asks for is skipped; the trim itself is not.
        }
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                intent.data?.schemeSpecificPart?.let(onPackageChanged)
            }
        }
        val changes = IntentFilter().apply {
            addAction(Intent.ACTION_PACKAGE_ADDED)
            addAction(Intent.ACTION_PACKAGE_REPLACED)
            addDataScheme("package")
        }
        // Only the system sends these.
        context.registerReceiver(receiver, changes, Context.RECEIVER_NOT_EXPORTED)
    }

    private companion object {
        const val TRIM = "vendor.dsa.app.trim"
        const val COMPILE = "vendor.dsa.app.compile"
        const val PREFERENCES = "compiled-apps"
        const val NAMESPACE = "activity_manager"
        const val USE_COMPACTION = "use_compaction"

        // Context.ACTIVITY_SERVICE, the activity manager's binder, and the shell command DSA's
        // framework adds to it.
        const val ACTIVITY_SERVICE = "activity"
        const val HIDDEN_TRIM_COMMAND = "dsa-hidden-trim"
    }
}

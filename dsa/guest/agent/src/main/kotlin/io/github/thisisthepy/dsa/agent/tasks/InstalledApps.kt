package io.github.thisisthepy.dsa.agent.tasks

/** An installed app a launcher lists, as the host's start menu and its list of apps show it. */
class LauncherApp(
    val packageName: String,
    /** The app's name, as a launcher shows it. */
    val label: String,
    /** The launcher icon as a square PNG image; empty when the app has none to draw. */
    val iconPng: ByteArray,
    /** The installed version, as the package manager reports it. */
    val versionName: String = "",
    val versionCode: Long = 0,
    /** When the app was first installed and last updated, in milliseconds since the Unix epoch. */
    val firstInstallTimeMs: Long = 0,
    val lastUpdateTimeMs: Long = 0,
    /** Whether the app is part of the Android image, which cannot be uninstalled. */
    val systemApp: Boolean = false,
    /** The storage the app takes, in bytes: its code, its data and its cache. */
    val appBytes: Long = 0,
    val dataBytes: Long = 0,
    val cacheBytes: Long = 0,
    /** The ABI the app's native code runs as (the package manager's primary CPU ABI); empty for none. */
    val primaryAbi: String = "",
    /** The native libraries of the app's APKs on the device, base and splits together: ABI to names. */
    val nativeLibraries: Map<String, List<String>> = emptyMap(),
) {
    /** Whether the host would show [other] exactly as this. */
    fun sameAs(other: LauncherApp): Boolean =
        packageName == other.packageName &&
            label == other.label &&
            iconPng.contentEquals(other.iconPng) &&
            versionName == other.versionName &&
            versionCode == other.versionCode &&
            firstInstallTimeMs == other.firstInstallTimeMs &&
            lastUpdateTimeMs == other.lastUpdateTimeMs &&
            systemApp == other.systemApp &&
            appBytes == other.appBytes &&
            dataBytes == other.dataBytes &&
            cacheBytes == other.cacheBytes &&
            primaryAbi == other.primaryAbi &&
            nativeLibraries == other.nativeLibraries
}

/** The installed apps a launcher lists, and word of every change to them. */
interface LauncherApps {
    /** The apps installed now, or null when they cannot be read, so nothing is reported. */
    fun list(): List<LauncherApp>?

    /** Calls [onChange], from any thread, whenever an app is installed, updated or removed. */
    fun listen(onChange: () -> Unit)

    companion object {
        /** Lists nothing, so the host is never told which apps are installed. */
        val NONE = object : LauncherApps {
            override fun list(): List<LauncherApp>? = null
            override fun listen(onChange: () -> Unit) = Unit
        }
    }
}

/**
 * Keeps the host's list of installed apps in step with the guest.
 *
 * Each [update] sends InstalledApp for every app the host has not been told about on this
 * connection, or whose name, icon, version or size changed, then InstalledApps naming every
 * app, when the apps or what the host knows of them changed. A forced update (the host asked
 * for a fresh list and waits for it) sends InstalledApps even when nothing changed. Each app
 * goes in a frame of its own, so no frame carries every icon. A frame that could not be sent
 * is not recorded as told, so the next update tries again. The agent's own package is never
 * listed: it has nothing to open.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class InstalledAppReporter(
    private val ownPackage: String,
    /** Sends one control frame; false when it could not be sent. */
    private val send: (ByteArray) -> Boolean,
) {
    private val told = HashMap<String, LauncherApp>()
    private var toldNames: List<String>? = null
    private var listOwed = false

    fun update(apps: List<LauncherApp>, force: Boolean = false) {
        val current = sortedMapOf<String, LauncherApp>()
        for (app in apps) {
            if (app.packageName.isNotEmpty() && app.packageName != ownPackage && app.packageName !in current) {
                current[app.packageName] = app
            }
        }
        for ((packageName, app) in current) {
            val before = told[packageName]
            if (before != null && before.sameAs(app)) continue
            if (!send(Messages.installedApp(app))) return
            told[packageName] = app
            // The host takes what changed only with the list that follows.
            listOwed = true
        }
        val names = current.keys.toList()
        if (!force && !listOwed && names == toldNames) return
        if (send(Messages.installedApps(names))) {
            toldNames = names
            listOwed = false
            told.keys.retainAll(current.keys)
        }
    }

    /** Treats what the host was told as unknown, for a new connection. */
    fun resend() {
        told.clear()
        toldNames = null
        listOwed = false
    }
}

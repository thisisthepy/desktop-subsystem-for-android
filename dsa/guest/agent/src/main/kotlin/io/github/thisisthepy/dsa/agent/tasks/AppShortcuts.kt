package io.github.thisisthepy.dsa.agent.tasks

/**
 * One of an app's launcher shortcuts, as Android's ShortcutManager keeps it: a static one from
 * the app's manifest, a dynamic one the app published, or one pinned at the app's request.
 */
class LauncherShortcut(
    /** Unique within its app. */
    val id: String,
    /** The name a launcher shows. */
    val shortLabel: String,
    /** The longer name a launcher shows when it has room; empty when the app gave none. */
    val longLabel: String = "",
    /** The icon as a square PNG image; empty when it has none the agent could draw. */
    val iconPng: ByteArray = ByteArray(0),
    /** Its place among the app's shortcuts, lowest first. */
    val rank: Int = 0,
    val manifest: Boolean = false,
    val dynamic: Boolean = false,
    val pinned: Boolean = false,
) {
    /** Whether the host would show [other] exactly as this. */
    fun sameAs(other: LauncherShortcut): Boolean =
        id == other.id &&
            shortLabel == other.shortLabel &&
            longLabel == other.longLabel &&
            iconPng.contentEquals(other.iconPng) &&
            rank == other.rank &&
            manifest == other.manifest &&
            dynamic == other.dynamic &&
            pinned == other.pinned

    /** Whether a launcher lists it with the app: a static or dynamic one, not one only pinned. */
    val listed: Boolean get() = manifest || dynamic
}

/** The launcher shortcuts of the installed apps, and word of every change to them. */
interface ShortcutSource {
    /**
     * The enabled shortcuts of [packageName], or null when they cannot be read, so nothing
     * is reported for the app.
     */
    fun shortcuts(packageName: String): List<LauncherShortcut>?

    /** Calls [onChange], from any thread, with the package whose shortcuts changed. */
    fun listen(onChange: (String) -> Unit)

    companion object {
        /** Reads nothing, so the host is never told of any shortcut. */
        val NONE = object : ShortcutSource {
            override fun shortcuts(packageName: String): List<LauncherShortcut>? = null
            override fun listen(onChange: (String) -> Unit) = Unit
        }
    }
}

/**
 * Keeps the host's copy of each app's shortcuts in step with the guest.
 *
 * Each [update] sends AppShortcuts for every app whose shortcuts the host has not been told on
 * this connection, or whose shortcuts changed. An app that has none is not reported until it
 * had some: then an empty AppShortcuts says they are gone. The shortcuts go in the order the
 * host shows them: the static and dynamic ones by rank, then the ones only pinned. A frame
 * that could not be sent is not recorded as told, so the next update tries again. The agent's
 * own package is never reported.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class ShortcutReporter(
    private val ownPackage: String,
    /** Sends one control frame; false when it could not be sent. */
    private val send: (ByteArray) -> Boolean,
) {
    private val told = HashMap<String, List<LauncherShortcut>>()

    /**
     * Reports the shortcuts of [packages], read with [read] (null for an app whose shortcuts
     * cannot be read, which is left as the host knows it). An app no longer in [packages] is
     * forgotten: the host drops its shortcuts with the app.
     */
    fun update(packages: Collection<String>, read: (String) -> List<LauncherShortcut>?) {
        val current = packages.filter { it.isNotEmpty() && it != ownPackage }.toSortedSet()
        told.keys.retainAll(current)
        report(current, read)
    }

    /**
     * Reports the shortcuts of [packages] alone, whose shortcuts Android said changed; what the
     * host knows of every other app stays as it is.
     */
    fun changed(packages: Collection<String>, read: (String) -> List<LauncherShortcut>?) {
        report(packages.filter { it.isNotEmpty() && it != ownPackage }.toSortedSet(), read)
    }

    private fun report(current: Set<String>, read: (String) -> List<LauncherShortcut>?) {
        for (packageName in current) {
            val shortcuts = read(packageName)?.let(::ordered) ?: continue
            val before = told[packageName]
            if (before == null && shortcuts.isEmpty()) continue
            if (before != null && same(before, shortcuts)) continue
            if (!send(Messages.appShortcuts(packageName, shortcuts))) continue
            if (shortcuts.isEmpty()) told.remove(packageName) else told[packageName] = shortcuts
        }
    }

    /** Treats what the host was told as unknown, for a new connection. */
    fun resend() {
        told.clear()
    }

    private fun same(a: List<LauncherShortcut>, b: List<LauncherShortcut>): Boolean =
        a.size == b.size && a.indices.all { a[it].sameAs(b[it]) }

    companion object {
        /** [shortcuts] each ID once, the listed ones by rank first, then the ones only pinned. */
        fun ordered(shortcuts: List<LauncherShortcut>): List<LauncherShortcut> {
            val seen = HashSet<String>()
            val unique = shortcuts.filter { it.id.isNotEmpty() && seen.add(it.id) }
            val listed = unique.filter { it.listed }.sortedWith(compareBy({ !it.manifest }, { it.rank }))
            return listed + unique.filterNot { it.listed }
        }
    }
}

package io.github.thisisthepy.dsa.agent.memory

/** An installed package as compiling it needs: when it was last installed or updated, and how large its APKs are. */
data class InstalledPackage(val updatedAt: Long, val apkBytes: Long)

/**
 * What the agent keeps about an app the user opens: how many times a task of it opened, the
 * install or update ([InstalledPackage.updatedAt]) last compiled with speed (0 for none), what
 * that compile came to, and when it ran (wall clock milliseconds, 0 for never).
 */
data class CompileRecord(
    val opens: Int = 0,
    val compiledUpdate: Long = 0,
    val result: String = "",
    val compiledAtMillis: Long = 0,
)

/**
 * The guest's side of trimming the apps the user cannot see and compiling the apps the user
 * opens. Both are carried out by scripts init runs as the shell when a request property
 * changes, which answer in a property of their own: the agent, a platform app, may not ask the
 * activity manager or the package manager for them itself.
 */
interface AppMemoryPort {
    /** Sets the trim request, "<number> <HIDDEN|BACKGROUND> <compact: 0 or 1> <process>". */
    fun requestTrim(value: String)

    /**
     * The answer to the last trim request carried out, "<number> <result>" (sent, refused
     * with the activity manager's reason, or invalid), the number alone from an older image's
     * script, or empty.
     */
    fun trimDone(): String

    /**
     * Whether the framework sends a memory trim to an app the host hides although Android
     * shows its task (the activity manager's dsa-hidden-trim, which DSA's framework adds).
     * A framework without that change answers false.
     */
    fun hiddenTrimSupported(): Boolean = false

    /**
     * Tells the framework which apps the host hides, replacing those it was told before, so
     * that it sends them their trims. Returns whether it took them.
     */
    fun setHiddenApps(packages: Set<String>): Boolean = false

    /** Sets the compile request, "<number> <package>". */
    fun requestCompile(value: String)

    /** The answer to the last compile request, "<number> <result>", or empty. */
    fun compileDone(): String

    /** The package as installed now, or null when it is not installed. */
    fun installed(packageName: String): InstalledPackage?

    /** What was kept of the apps the user opens, by package. */
    fun records(): Map<String, CompileRecord>

    /** Keeps [record] for [packageName]. */
    fun save(packageName: String, record: CompileRecord)

    /** The wall clock, in milliseconds, for the records. */
    fun wallMillis(): Long

    /**
     * Makes sure the activity manager compacts a process when asked, and calls
     * [onPackageChanged] with each package installed or updated, on any thread.
     */
    fun start(onPackageChanged: (String) -> Unit) = Unit

    companion object {
        /** Nothing to trim or compile with: requests go nowhere and nothing is installed. */
        val NONE = object : AppMemoryPort {
            override fun requestTrim(value: String) = Unit
            override fun trimDone() = ""
            override fun requestCompile(value: String) = Unit
            override fun compileDone() = ""
            override fun installed(packageName: String): InstalledPackage? = null
            override fun records(): Map<String, CompileRecord> = emptyMap()
            override fun save(packageName: String, record: CompileRecord) = Unit
            override fun wallMillis() = 0L
        }
    }
}

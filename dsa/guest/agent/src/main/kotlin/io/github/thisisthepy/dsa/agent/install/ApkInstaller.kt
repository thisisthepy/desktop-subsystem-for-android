package io.github.thisisthepy.dsa.agent.install

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.ApkChunk
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.InstallApk
import io.github.thisisthepy.dsa.control.v1.InstallProgress
import io.github.thisisthepy.dsa.control.v1.InstallResult
import io.github.thisisthepy.dsa.control.v1.InstallStatus

/** How the guest's package installer ended an install session, as its status broadcast says. */
data class InstallerResult(
    /** PackageInstaller's EXTRA_STATUS: one of the STATUS_ values in [PackageInstallerStatus]. */
    val status: Int,
    /** PackageInstaller's EXTRA_STATUS_MESSAGE; null when it gave none. */
    val message: String?,
    /** PackageInstaller's EXTRA_PACKAGE_NAME; null when it gave none. */
    val packageName: String?,
    /** The package manager's own INSTALL_FAILED_ code, which tells a downgrade apart; 0 when absent. */
    val legacyStatus: Int = 0,
)

/** One install session of the guest's package installer, which the APK is written into. */
interface InstallSession {
    /**
     * Begins the next APK of a session of several, [name] of [size] bytes: what is written
     * from here on goes into it. A session of one APK has it begun already.
     */
    fun startApk(name: String, size: Long) {
        throw IllegalStateException("this install session holds one APK")
    }

    /** Adds [data] to the end of the APK. */
    fun write(data: ByteArray)

    /** Commits the session; [onResult] is called once, on any thread, when the installer has decided. */
    fun commit(onResult: (InstallerResult) -> Unit)

    /** Drops the session and what was written into it. */
    fun abandon()
}

/** The guest's package installer, as the agent uses it. */
fun interface PackageInstallerPort {
    /**
     * Opens a session for a full install of an APK of [size] bytes, with its native code for
     * [abi] (the installer's ABI override: [Abis.X86_64] or [Abis.ARM64]); an empty [abi] lets
     * the package manager choose.
     */
    fun open(name: String, size: Long, abi: String): InstallSession

    /**
     * Opens a session for a full install of an app made of several APKs, [size] bytes together,
     * as [open] does; [InstallSession.startApk] begins each APK. [name] is the bundle's.
     */
    fun openApks(name: String, size: Long, abi: String): InstallSession {
        throw IllegalStateException("this agent installs no apps made of several APKs")
    }

    companion object {
        val NONE = PackageInstallerPort { _, _, _ -> throw IllegalStateException("this agent installs no APKs") }
    }
}

/**
 * Where the expansion (OBB) files of an app the host installs go. They cannot be part of an
 * install session, so each is kept aside while it arrives and moved into the app's folder
 * under Android/obb/ once the app is installed, where the app reads it as it would one a
 * store put there.
 */
interface ExpansionFiles {
    /** Begins keeping the expansion file [name] aside; its bytes go to the stream returned. */
    fun stage(name: String): java.io.OutputStream

    /** Moves the files [names] kept aside into [packageName]'s folder under Android/obb/. */
    fun place(packageName: String, names: List<String>)

    /** Drops the files [names] kept aside. */
    fun discard(names: List<String>)

    companion object {
        val NONE = object : ExpansionFiles {
            override fun stage(name: String): java.io.OutputStream =
                throw IllegalStateException("this agent keeps no expansion files")

            override fun place(packageName: String, names: List<String>): Unit =
                throw IllegalStateException("this agent keeps no expansion files")

            override fun discard(names: List<String>) {}
        }
    }
}

/** The folder an app's expansion files go in, and the names files of an install may have. */
object InstallFileNames {
    /** [packageName]'s folder under Android/obb/ of the shared storage at [storage]. */
    fun obbFolder(storage: java.io.File, packageName: String): java.io.File =
        java.io.File(java.io.File(java.io.File(storage, "Android"), "obb"), packageName)

    /** Whether [name] is a file name, without a folder, that ends in [ending] after something. */
    fun valid(name: String, ending: String): Boolean =
        name.length in (ending.length + 1)..255 &&
            name.endsWith(ending, ignoreCase = true) &&
            '/' !in name && '\\' !in name &&
            name.none { it.isISOControl() }
}

/** The ABIs the host may ask an app to be installed for. */
object Abis {
    const val X86_64 = "x86_64"
    const val ARM64 = "arm64-v8a"

    /** Whether [abi] is one the host may ask for: empty, which leaves it to Android, or one of the two above. */
    fun allowed(abi: String): Boolean = abi.isEmpty() || abi == X86_64 || abi == ARM64
}

/** The values of android.content.pm.PackageInstaller's STATUS_ constants and the legacy codes used here. */
object PackageInstallerStatus {
    const val PENDING_USER_ACTION = -1
    const val SUCCESS = 0
    const val FAILURE = 1
    const val FAILURE_BLOCKED = 2
    const val FAILURE_ABORTED = 3
    const val FAILURE_INVALID = 4
    const val FAILURE_CONFLICT = 5
    const val FAILURE_STORAGE = 6
    const val FAILURE_INCOMPATIBLE = 7

    /** PackageManager.INSTALL_FAILED_VERSION_DOWNGRADE. */
    const val LEGACY_VERSION_DOWNGRADE = -25

    /** The stable reason the host is told for what the installer said. */
    fun reason(result: InstallerResult): InstallStatus {
        val downgrade = result.legacyStatus == LEGACY_VERSION_DOWNGRADE ||
            result.message?.contains("INSTALL_FAILED_VERSION_DOWNGRADE") == true
        return when (result.status) {
            SUCCESS -> InstallStatus.INSTALL_STATUS_SUCCESS
            FAILURE_INCOMPATIBLE -> InstallStatus.INSTALL_STATUS_INCOMPATIBLE
            FAILURE_CONFLICT -> if (downgrade) InstallStatus.INSTALL_STATUS_DOWNGRADE else InstallStatus.INSTALL_STATUS_CONFLICTING_SIGNATURE
            FAILURE_INVALID -> if (downgrade) InstallStatus.INSTALL_STATUS_DOWNGRADE else InstallStatus.INSTALL_STATUS_INVALID_APK
            FAILURE_STORAGE -> InstallStatus.INSTALL_STATUS_INSUFFICIENT_STORAGE
            FAILURE_BLOCKED -> InstallStatus.INSTALL_STATUS_BLOCKED
            else -> if (downgrade) InstallStatus.INSTALL_STATUS_DOWNGRADE else InstallStatus.INSTALL_STATUS_OTHER
        }
    }
}

/**
 * Installs the APKs the host sends: InstallApk opens an install session, each ApkChunk is
 * written into it in order, and once all of the APK is there the session is committed. After
 * each chunk the host is told how far it got (InstallProgress), which is what lets it send the
 * next ones, and when the installer has decided the host is told how it went (InstallResult),
 * with a stable reason and the installer's own words. An install that goes wrong at any point
 * ends with InstallResult and its session abandoned; the host's CancelInstall does the same.
 *
 * The installed app reaches the host through the agent's report of launcher apps, which the
 * package manager's broadcast refreshes; [onInstalled] refreshes it once more, so the app is
 * reported even if that broadcast is late.
 *
 * Every method runs on the agent's scheduler thread; the installer's answer is moved there
 * with [post].
 */
class ApkInstaller(
    private val installer: PackageInstallerPort,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val post: (() -> Unit) -> Unit,
    private val onInstalled: () -> Unit = {},
    private val expansionFiles: ExpansionFiles = ExpansionFiles.NONE,
) {
    /** One file of an install of several, as InstallApk names it. */
    private data class Part(val name: String, val size: Long, val obb: Boolean)

    private class Transfer(
        val id: Int,
        val name: String,
        val size: Long,
        val session: InstallSession,
        val parts: List<Part> = emptyList(),
    ) {
        var received = 0L
        var committed = false

        /** The part being written, and how much of it is. */
        var part = 0
        var partWritten = 0L

        /** Where the expansion file being written goes, while one is. */
        var obb: java.io.OutputStream? = null

        /** The expansion files kept aside so far. */
        val staged = mutableListOf<String>()
    }

    private var transfer: Transfer? = null

    /** A new connection to the host: an install of the last one will never be finished. */
    fun connected() {
        transfer?.let { if (!it.committed) abandon(it) }
        transfer = null
    }

    /** Takes a frame about an install. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.INSTALL_APK -> begin(envelope.installApk)
            Envelope.BodyCase.APK_CHUNK -> chunk(envelope.apkChunk)
            Envelope.BodyCase.CANCEL_INSTALL -> cancel(envelope.cancelInstall.transferId)
            else -> return false
        }
        return true
    }

    private fun begin(install: InstallApk) {
        val id = install.transferId
        transfer?.let { running ->
            if (!running.committed) {
                log.warn("install ${running.id} of ${running.name} was replaced by install $id before it was whole")
                fail(running, InstallStatus.INSTALL_STATUS_OTHER, "another install began before this one was whole")
            }
        }
        val parts = install.partsList.map { Part(it.name, it.size, it.obb) }
        if (parts.isEmpty() && (install.size <= 0 || install.size > MAX_APK_BYTES)) {
            return result(id, InstallStatus.INSTALL_STATUS_INVALID_APK, "", "the APK is ${install.size} bytes; Android takes APKs of 1 byte to ${MAX_APK_BYTES / (1024 * 1024)} MiB")
        }
        partsProblem(install.size, parts)?.let { why ->
            return result(id, InstallStatus.INSTALL_STATUS_INVALID_APK, "", why)
        }
        if (!Abis.allowed(install.abi)) {
            return result(id, InstallStatus.INSTALL_STATUS_OTHER, "", "the host asked for the ABI ${install.abi.take(32)}, which is neither ${Abis.X86_64} nor ${Abis.ARM64}")
        }
        val session = try {
            if (parts.isEmpty()) {
                installer.open(install.name, install.size, install.abi)
            } else {
                installer.openApks(install.name, parts.filter { !it.obb }.sumOf { it.size }, install.abi)
            }
        } catch (e: Exception) {
            log.warn("could not open an install session for ${install.name}", e)
            val storage = e is java.io.IOException && e.message?.contains("space", ignoreCase = true) == true
            return result(
                id,
                if (storage) InstallStatus.INSTALL_STATUS_INSUFFICIENT_STORAGE else InstallStatus.INSTALL_STATUS_OTHER,
                "",
                "Android could not begin the install: ${e.message ?: e.javaClass.simpleName}",
            )
        }
        val running = Transfer(id, install.name, install.size, session, parts)
        transfer = running
        if (parts.isNotEmpty()) {
            try {
                beginPart(running)
            } catch (e: Exception) {
                log.warn("could not begin ${parts[0].name} of ${install.name}", e)
                return fail(running, InstallStatus.INSTALL_STATUS_OTHER, "Android could not begin the install: ${e.message ?: e.javaClass.simpleName}")
            }
        }
        val abi = if (install.abi.isEmpty()) "" else " for ${install.abi}"
        val files = if (parts.isEmpty()) "" else ", ${parts.size} files"
        log.info("installing ${install.name} (${install.size} bytes$files) from the host$abi")
    }

    /**
     * Why the files of an install of several cannot be installed, or null when they can: the
     * APKs come first, each file has a name of its kind and some bytes, no name is there
     * twice, and they add up to the [size] announced.
     */
    private fun partsProblem(size: Long, parts: List<Part>): String? {
        if (parts.isEmpty()) return null
        if (parts.size > MAX_PARTS) return "the install has ${parts.size} files; Android takes at most $MAX_PARTS together"
        if (parts.none { !it.obb }) return "the install has no APK"
        val firstObb = parts.indexOfFirst { it.obb }
        if (firstObb >= 0 && parts.drop(firstObb).any { !it.obb }) return "an APK of the install comes after an expansion file"
        for ((index, part) in parts.withIndex()) {
            val kind = if (part.obb) "an expansion file" else "an APK"
            if (!InstallFileNames.valid(part.name, if (part.obb) ".obb" else ".apk")) return "${part.name.take(64)} is not the name of $kind"
            if (part.size <= 0) return "${part.name} is empty"
            if (!part.obb && part.size > MAX_APK_BYTES) return "${part.name} is ${part.size} bytes; Android takes APKs up to ${MAX_APK_BYTES / (1024 * 1024)} MiB"
            if (parts.take(index).any { it.name.equals(part.name, ignoreCase = true) }) return "${part.name} is in the install twice"
        }
        val total = parts.sumOf { it.size }
        if (total > MAX_INSTALL_BYTES) return "the install is $total bytes; Android takes up to ${MAX_INSTALL_BYTES / (1024 * 1024)} MiB at once"
        if (total != size) return "the files of the install add up to $total bytes, not the $size announced"
        return null
    }

    /** Begins the part of the install the transfer is at: an APK of the session, or an expansion file kept aside. */
    private fun beginPart(running: Transfer) {
        val part = running.parts[running.part]
        if (part.obb) {
            running.staged += part.name
            running.obb = expansionFiles.stage(part.name)
        } else {
            running.session.startApk(part.name, part.size)
        }
    }

    /** Writes [data] into the install: the one APK, or across its files in order. */
    private fun store(running: Transfer, data: ByteArray) {
        if (running.parts.isEmpty()) return running.session.write(data)
        var at = 0
        while (at < data.size) {
            val part = running.parts[running.part]
            val count = minOf(part.size - running.partWritten, (data.size - at).toLong()).toInt()
            val piece = data.copyOfRange(at, at + count)
            val obb = running.obb
            if (obb != null) obb.write(piece) else running.session.write(piece)
            running.partWritten += count
            at += count
            if (running.partWritten == part.size) {
                running.obb?.close()
                running.obb = null
                running.part += 1
                running.partWritten = 0
                if (running.part < running.parts.size) beginPart(running)
            }
        }
    }

    private fun chunk(chunk: ApkChunk) {
        val running = transfer
        if (running == null || running.id != chunk.transferId || running.committed) {
            log.warn("dropped a piece of install ${chunk.transferId}, which is not under way")
            return
        }
        if (chunk.offset != running.received) {
            return fail(running, InstallStatus.INSTALL_STATUS_OTHER, "a piece of the APK arrived out of order (at ${chunk.offset}, expected ${running.received})")
        }
        if (running.received + chunk.data.size() > running.size) {
            return fail(running, InstallStatus.INSTALL_STATUS_OTHER, "more than the ${running.size} bytes announced arrived")
        }
        try {
            store(running, chunk.data.toByteArray())
        } catch (e: Exception) {
            log.warn("could not write a piece of ${running.name} into its install session", e)
            val storage = e is java.io.IOException
            return fail(
                running,
                if (storage) InstallStatus.INSTALL_STATUS_INSUFFICIENT_STORAGE else InstallStatus.INSTALL_STATUS_OTHER,
                "Android could not store the APK: ${e.message ?: e.javaClass.simpleName}",
            )
        }
        running.received += chunk.data.size()
        progress(running)
        if (running.received == running.size) commit(running)
    }

    private fun commit(running: Transfer) {
        running.committed = true
        log.info("committing ${running.name}")
        try {
            running.session.commit { answer -> post { committed(running, answer) } }
        } catch (e: Exception) {
            log.warn("could not commit the install of ${running.name}", e)
            running.committed = false
            fail(running, InstallStatus.INSTALL_STATUS_OTHER, "Android could not install the APK: ${e.message ?: e.javaClass.simpleName}")
        }
    }

    private fun committed(running: Transfer, answer: InstallerResult) {
        if (transfer === running) transfer = null
        val reason = PackageInstallerStatus.reason(answer)
        if (reason == InstallStatus.INSTALL_STATUS_SUCCESS) {
            log.info("installed ${answer.packageName ?: running.name}")
            onInstalled()
            placeExpansionFiles(running, answer.packageName ?: "")?.let { why ->
                return result(running.id, InstallStatus.INSTALL_STATUS_OTHER, answer.packageName ?: "", why)
            }
        } else {
            log.warn("did not install ${running.name}: $reason (${answer.message})")
            discardExpansionFiles(running)
        }
        result(running.id, reason, answer.packageName ?: "", answer.message ?: "")
    }

    /**
     * Moves the installed app's expansion files into its folder under Android/obb/. Returns
     * why they are not there, or null when they are or the app has none.
     */
    private fun placeExpansionFiles(running: Transfer, packageName: String): String? {
        if (running.staged.isEmpty()) return null
        if (packageName.isEmpty()) {
            discardExpansionFiles(running)
            return "the app is installed, but Android did not say its package, so its expansion files have no folder under Android/obb/"
        }
        return try {
            expansionFiles.place(packageName, running.staged.toList())
            log.info("put ${running.staged.size} expansion files in Android/obb/$packageName")
            null
        } catch (e: Exception) {
            log.warn("could not put the expansion files of $packageName in Android/obb/$packageName", e)
            discardExpansionFiles(running)
            "$packageName is installed, but its expansion files could not be put in Android/obb/$packageName: ${e.message ?: e.javaClass.simpleName}"
        }
    }

    private fun discardExpansionFiles(running: Transfer) {
        try {
            running.obb?.close()
        } catch (e: Exception) {
            log.warn("could not close an expansion file of ${running.name}", e)
        }
        running.obb = null
        if (running.staged.isEmpty()) return
        try {
            expansionFiles.discard(running.staged.toList())
        } catch (e: Exception) {
            log.warn("could not drop the expansion files of ${running.name}", e)
        }
    }

    private fun cancel(id: Int) {
        val running = transfer
        if (running == null || running.id != id) {
            log.warn("the host cancelled install $id, which is not under way")
            return result(id, InstallStatus.INSTALL_STATUS_OTHER, "", "the install was cancelled")
        }
        if (running.committed) {
            // The installer has the whole APK and will answer; the host takes that answer.
            log.warn("the host cancelled install $id after it was committed")
            return
        }
        log.info("the host cancelled the install of ${running.name}")
        fail(running, InstallStatus.INSTALL_STATUS_OTHER, "the install was cancelled")
    }

    private fun fail(running: Transfer, status: InstallStatus, message: String) {
        log.warn("install of ${running.name} failed: $message")
        abandon(running)
        if (transfer === running) transfer = null
        result(running.id, status, "", message)
    }

    private fun abandon(running: Transfer) {
        try {
            running.session.abandon()
        } catch (e: Exception) {
            log.warn("could not abandon the install session of ${running.name}", e)
        }
        discardExpansionFiles(running)
    }

    private fun progress(running: Transfer) {
        send(
            Envelope.newBuilder().setInstallProgress(
                InstallProgress.newBuilder().setTransferId(running.id).setReceived(running.received),
            ).build().toByteArray(),
        )
    }

    private fun result(id: Int, status: InstallStatus, packageName: String, message: String) {
        send(
            Envelope.newBuilder().setInstallResult(
                InstallResult.newBuilder().setTransferId(id).setStatus(status).setPackageName(packageName).setMessage(message),
            ).build().toByteArray(),
        )
    }

    companion object {
        /** The largest APK the host sends; it refuses larger ones itself. */
        const val MAX_APK_BYTES = 2L * 1024 * 1024 * 1024

        /** The most an install of several files holds, all of them together. */
        const val MAX_INSTALL_BYTES = 8L * 1024 * 1024 * 1024

        /** The most files one install holds. */
        const val MAX_PARTS = 256
    }
}

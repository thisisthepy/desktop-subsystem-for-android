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
    /** Adds [data] to the end of the APK. */
    fun write(data: ByteArray)

    /** Commits the session; [onResult] is called once, on any thread, when the installer has decided. */
    fun commit(onResult: (InstallerResult) -> Unit)

    /** Drops the session and what was written into it. */
    fun abandon()
}

/** The guest's package installer, as the agent uses it. */
fun interface PackageInstallerPort {
    /** Opens a session for a full install of an APK of [size] bytes. */
    fun open(name: String, size: Long): InstallSession

    companion object {
        val NONE = PackageInstallerPort { _, _ -> throw IllegalStateException("this agent installs no APKs") }
    }
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
) {
    private class Transfer(val id: Int, val name: String, val size: Long, val session: InstallSession) {
        var received = 0L
        var committed = false
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
        if (install.size <= 0 || install.size > MAX_APK_BYTES) {
            return result(id, InstallStatus.INSTALL_STATUS_INVALID_APK, "", "the APK is ${install.size} bytes; Android takes APKs of 1 byte to ${MAX_APK_BYTES / (1024 * 1024)} MiB")
        }
        val session = try {
            installer.open(install.name, install.size)
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
        transfer = Transfer(id, install.name, install.size, session)
        log.info("installing ${install.name} (${install.size} bytes) from the host")
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
            running.session.write(chunk.data.toByteArray())
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
        } else {
            log.warn("did not install ${running.name}: $reason (${answer.message})")
        }
        result(running.id, reason, answer.packageName ?: "", answer.message ?: "")
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
    }
}

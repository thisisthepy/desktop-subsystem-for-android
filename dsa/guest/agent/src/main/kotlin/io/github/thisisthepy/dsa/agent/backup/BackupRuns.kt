package io.github.thisisthepy.dsa.agent.backup

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.BackupRunResult
import io.github.thisisthepy.dsa.control.v1.BackupStatus
import io.github.thisisthepy.dsa.control.v1.Envelope

/** How a backup of every app ended. [error] is null when the backup manager finished it. */
data class BackupOutcome(val succeeded: Int, val failed: Int, val error: String?)

/**
 * Android's backup manager, as the agent drives it. The platform's calls it through
 * IBackupManager; a test's records the calls. Each call returns at once; the [done] callbacks
 * come later, on any thread.
 */
interface BackupManagerPort {
    /**
     * Turns backup on with the agent's transport as the one Android uses, and automatic
     * restore on. False while that cannot be done yet (the backup manager has not found the
     * transport), so the agent tries again.
     */
    fun useHostTransport(): Boolean

    /**
     * Keeps Android's backup manager from backing up to the host while the user has backup
     * off: backup off and automatic restore off, so nothing is backed up and nothing is
     * scheduled. A transport of another app the user chose (Google's) is left as it is. False
     * while that cannot be done yet, so the agent tries again.
     */
    fun keepOff(): Boolean

    /** Backs up every app Android may back up, now, as `bmgr backupnow --all` does. */
    fun backUpAll(done: (BackupOutcome) -> Unit)

    /** Restores [packages] from the restore set [token]; [done] gets null or why it failed. */
    fun restore(token: Long, packages: List<String>, done: (String?) -> Unit)

    companion object {
        val NONE = object : BackupManagerPort {
            override fun useHostTransport() = false
            override fun keepOff() = true
            override fun backUpAll(done: (BackupOutcome) -> Unit) =
                done(BackupOutcome(0, 0, "this Android has no backup manager"))
            override fun restore(token: Long, packages: List<String>, done: (String?) -> Unit) =
                done("this Android has no backup manager")
        }
    }
}

/**
 * Keeps Android's backup manager as the user chose on the host, and runs the backups and
 * restores the host asks for (BackupRun, BackupRestore), each answered with BackupRunResult
 * once Android's backup manager has finished.
 *
 * Backup is the user's choice and off until the host says it is on (BackupSetting, sent each
 * time the agent connects and whenever the user changes it). While it is off the agent keeps
 * Android's backup manager from backing up to the host, so nothing is backed up and nothing is
 * scheduled, and refuses the host's runs. While it is on it makes sure backup is on, with the
 * host's transport selected: another app (Google Play services, if the user installs it) may
 * select its own, and the user's app data then would no longer reach the host.
 *
 * It checks at start, at each change, and every [CHECK_MILLIS] after; until the backup
 * manager has done what is asked it tries again every [RETRY_MILLIS].
 *
 * One run at a time: a request while one is under way is answered as an error at once.
 * Every method runs on the agent's thread; the port's callbacks come back through [post].
 */
class BackupRuns(
    private val port: BackupManagerPort,
    private val backup: HostBackup,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val post: (() -> Unit) -> Unit,
    private val later: (Long, () -> Unit) -> Unit,
) {
    private var running = false

    /** Whether the user turned backup on, as the host last said. Off until it says. */
    var enabled = false
        private set

    /** What the last check left in place: null before one, else whether backup was on. */
    private var inPlace: Boolean? = null

    /** Each check belongs to one chain; a change of the setting starts a new one. */
    private var chain = 0

    fun start() {
        // Without a backup manager there is nothing to keep as the user chose.
        if (port === BackupManagerPort.NONE) return
        check(chain)
    }

    private fun check(mine: Int) {
        if (mine != chain) return
        val wanted = enabled
        val ok = try {
            if (wanted) port.useHostTransport() else port.keepOff()
        } catch (e: Exception) {
            log.warn(
                if (wanted) "could not make the host's backup Android's; app data is not backed up"
                else "could not keep Android's backup manager from backing up to the host",
                e,
            )
            false
        }
        val now = if (ok) wanted else null
        if (now != inPlace) {
            inPlace = now
            log.info(
                when (now) {
                    true -> "app data is backed up to the host"
                    false -> "backup to the host is off, as the user chose"
                    null -> if (wanted) "the host's backup transport is not in use yet" else "backup is not off yet"
                },
            )
        }
        later(if (ok) CHECK_MILLIS else RETRY_MILLIS) { check(mine) }
    }

    /** The host said whether the user has backup on: acts on it at once. */
    private fun setting(on: Boolean) {
        if (on == enabled && inPlace != null) return
        enabled = on
        log.info(if (on) "the user turned backup to the host on" else "backup to the host is off")
        if (port === BackupManagerPort.NONE) return
        chain++
        check(chain)
    }

    /** Acts on BackupSetting, BackupRun and BackupRestore. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.BACKUP_SETTING -> setting(envelope.backupSetting.enabled)
            Envelope.BodyCase.BACKUP_RUN -> run(envelope.backupRun.requestId)
            Envelope.BodyCase.BACKUP_RESTORE -> {
                val request = envelope.backupRestore
                restore(request.requestId, request.token, request.packagesList)
            }
            else -> return false
        }
        return true
    }

    private fun run(id: Long) {
        if (!enabled) {
            answer(id, BackupStatus.BACKUP_STATUS_NOT_AVAILABLE, OFF)
            return
        }
        if (running) {
            answer(id, BackupStatus.BACKUP_STATUS_ERROR, "a backup or restore is under way; try again when it ends")
            return
        }
        running = true
        log.info("backing up every app, as the host asked")
        try {
            port.backUpAll { outcome ->
                post {
                    running = false
                    log.info("backed up ${outcome.succeeded} apps; ${outcome.failed} failed${outcome.error?.let { " ($it)" } ?: ""}")
                    answer(
                        id,
                        if (outcome.error == null) BackupStatus.BACKUP_STATUS_OK else BackupStatus.BACKUP_STATUS_ERROR,
                        outcome.error.orEmpty(),
                        outcome.succeeded,
                        outcome.failed,
                    )
                }
            }
        } catch (e: Exception) {
            running = false
            log.warn("the backup manager did not start the backup", e)
            answer(id, BackupStatus.BACKUP_STATUS_ERROR, "Android's backup manager refused the backup: ${e.message}")
        }
    }

    private fun restore(id: Long, token: Long, packages: List<String>) {
        if (!enabled) {
            answer(id, BackupStatus.BACKUP_STATUS_NOT_AVAILABLE, OFF)
            return
        }
        if (running) {
            answer(id, BackupStatus.BACKUP_STATUS_ERROR, "a backup or restore is under way; try again when it ends")
            return
        }
        // Android restores the package manager's metadata first by itself; it is no app.
        val apps = packages.filter { it != HostBackup.PACKAGE_MANAGER }.distinct()
        if (apps.isEmpty()) {
            answer(id, BackupStatus.BACKUP_STATUS_OK, "")
            return
        }
        running = true
        log.info("restoring ${apps.size} apps' data from the host's backup")
        try {
            port.restore(token, apps) { error ->
                post {
                    running = false
                    val restored = backup.restored
                    val failed = (apps.size - restored).coerceAtLeast(backup.restoreFailed)
                    log.info("restored $restored apps' data; $failed were not${error?.let { " ($it)" } ?: ""}")
                    answer(
                        id,
                        if (error == null) BackupStatus.BACKUP_STATUS_OK else BackupStatus.BACKUP_STATUS_ERROR,
                        error.orEmpty(),
                        restored,
                        failed,
                    )
                }
            }
        } catch (e: Exception) {
            running = false
            log.warn("the backup manager did not start the restore", e)
            answer(id, BackupStatus.BACKUP_STATUS_ERROR, "Android's backup manager refused the restore: ${e.message}")
        }
    }

    private fun answer(id: Long, status: BackupStatus, message: String, succeeded: Int = 0, failed: Int = 0) {
        val result = BackupRunResult.newBuilder()
            .setRequestId(id)
            .setStatus(status)
            .setMessage(message)
            .setSucceeded(succeeded.coerceAtLeast(0))
            .setFailed(failed.coerceAtLeast(0))
        if (!send(Envelope.newBuilder().setBackupRunResult(result).build().toByteArray())) {
            log.warn("the result of backup run $id did not reach the host")
        }
    }

    companion object {
        const val RETRY_MILLIS = 10_000L
        const val CHECK_MILLIS = 5 * 60_000L
        const val OFF = "backup is off; turn it on in DSA's settings app or with dsa backup on"
    }
}

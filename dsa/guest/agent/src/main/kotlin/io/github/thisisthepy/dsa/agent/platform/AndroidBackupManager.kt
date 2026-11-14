package io.github.thisisthepy.dsa.agent.platform

import android.app.backup.BackupProgress
import android.app.backup.IBackupManager
import android.app.backup.IBackupObserver
import android.app.backup.IRestoreObserver
import android.app.backup.IRestoreSession
import android.app.backup.RestoreSet
import android.content.Context
import android.os.ServiceManager
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.backup.BackupManagerPort
import io.github.thisisthepy.dsa.agent.backup.BackupOutcome
import io.github.thisisthepy.dsa.agent.backup.HostBackup
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

/**
 * Android's backup manager, through its binder interface (IBackupManager), which a privileged
 * app holding android.permission.BACKUP may call: the SDK has none of these calls.
 */
class AndroidBackupManager(private val context: Context, private val log: Log) : BackupManagerPort {
    private val transport: String get() = HostBackupTransportService.name(context)

    /** The user the agent runs as: the app's uid divided by the uids each user has. */
    private val userId: Int get() = android.os.Process.myUid() / PER_USER_RANGE

    private fun manager(): IBackupManager? =
        ServiceManager.getService(BACKUP_SERVICE)?.let { IBackupManager.Stub.asInterface(it) }

    override fun useHostTransport(): Boolean {
        val manager = manager() ?: return false
        if (!manager.isBackupServiceActive(userId)) {
            try {
                manager.setBackupServiceActive(userId, true)
            } catch (e: SecurityException) {
                log.warn("the backup service is off for this user, and only the system may turn it on", e)
                return false
            }
        }
        // The backup manager registers the transport once it has bound its service.
        if (transport !in manager.listAllTransports().orEmpty()) return false
        if (manager.currentTransport != transport) {
            val previous = manager.selectBackupTransport(transport)
            log.info("backups go to the host now (instead of ${previous ?: "no transport"})")
        }
        if (!manager.isBackupEnabled) manager.setBackupEnabled(true)
        // An app installed later gets its data back from the host's backup at once.
        manager.setAutoRestore(true)
        return manager.currentTransport == transport && manager.isBackupEnabled
    }

    override fun keepOff(): Boolean {
        val manager = manager() ?: return false
        // A backup service that is not active backs nothing up and schedules nothing.
        if (!manager.isBackupServiceActive(userId)) return true
        val current = manager.currentTransport
        if (!offWith(current, transport)) {
            // Another app's transport (Google's, once the user installs Google Play) is the
            // user's choice in Android, not DSA's to turn off.
            return true
        }
        if (manager.isBackupEnabled) {
            manager.setBackupEnabled(false)
            log.info("Android's backup manager is off, as backup to the host is")
        }
        manager.setAutoRestore(false)
        return !manager.isBackupEnabled
    }

    override fun backUpAll(done: (BackupOutcome) -> Unit) {
        val manager = manager() ?: return done(BackupOutcome(0, 0, "Android's backup manager is not running"))
        val installed = context.packageManager.getInstalledPackages(0).map { it.packageName }.toTypedArray()
        val eligible: Array<String> = manager.filterAppsEligibleForBackupForUser(userId, installed) ?: emptyArray()
        if (eligible.isEmpty()) return done(BackupOutcome(0, 0, null))
        val succeeded = AtomicInteger()
        val failed = AtomicInteger()
        val finished = AtomicBoolean()
        val observer = object : IBackupObserver.Stub() {
            override fun onUpdate(currentPackage: String?, backupProgress: BackupProgress?) = Unit

            override fun onResult(target: String?, status: Int) {
                if (target == HostBackup.PACKAGE_MANAGER) return
                if (status == 0) succeeded.incrementAndGet() else failed.incrementAndGet()
            }

            override fun backupFinished(status: Int) {
                if (!finished.compareAndSet(false, true)) return
                done(
                    BackupOutcome(
                        succeeded.get(),
                        failed.get(),
                        if (status == 0) null else "Android's backup manager ended the backup with status $status",
                    ),
                )
            }
        }
        val started = manager.requestBackup(eligible, observer, null, 0)
        if (started != 0 && finished.compareAndSet(false, true)) {
            done(BackupOutcome(0, 0, "Android's backup manager refused the backup (status $started)"))
        }
    }

    override fun restore(token: Long, packages: List<String>, done: (String?) -> Unit) {
        val manager = manager() ?: return done("Android's backup manager is not running")
        val session: IRestoreSession = manager.beginRestoreSessionForUser(userId, null, transport)
            ?: return done("Android's backup manager gave no restore session for the host's backup")
        val finished = AtomicBoolean()
        fun end(error: String?) {
            if (!finished.compareAndSet(false, true)) return
            try {
                session.endRestoreSession()
            } catch (e: Exception) {
                log.warn("ending the restore session failed", e)
            }
            done(error)
        }
        // The session restores only from a set it has listed, so it lists them first.
        val observer = object : IRestoreObserver.Stub() {
            private var listed = false

            override fun restoreSetsAvailable(result: Array<RestoreSet>?) {
                if (listed) return
                listed = true
                if (result.orEmpty().none { it.token == token }) {
                    end("the host's backup is not among the restore sets Android sees")
                    return
                }
                val started = try {
                    session.restorePackages(token, this, packages.toTypedArray(), null)
                } catch (e: Exception) {
                    log.warn("starting the restore failed", e)
                    -1
                }
                if (started != 0) end("Android's backup manager refused the restore (status $started)")
            }

            override fun restoreStarting(numPackages: Int) = Unit

            override fun onUpdate(nowBeingRestored: Int, curentPackage: String?) = Unit

            override fun restoreFinished(error: Int) {
                end(if (error == 0) null else "Android's backup manager ended the restore with status $error")
            }
        }
        val listing = session.getAvailableRestoreSets(observer, null)
        if (listing != 0) end("Android's backup manager could not list the restore sets (status $listing)")
    }

    companion object {
        /** UserHandle.PER_USER_RANGE: each user's apps have uids in a range of this size. */
        private const val PER_USER_RANGE = 100_000

        /** Context.BACKUP_SERVICE, the backup manager's service name; the SDK hides it. */
        private const val BACKUP_SERVICE = "backup"

        /** The transport AOSP's backup manager starts with, which keeps backups on the guest. */
        const val LOCAL_TRANSPORT = "com.android.localtransport/.LocalTransport"

        /**
         * Whether backup is kept off while the user has backup to the host off, with [current]
         * the transport Android uses and [host] the agent's: the host's, AOSP's own local one
         * or none. Another app's transport is that app's and the user's.
         */
        fun offWith(current: String?, host: String): Boolean =
            current.isNullOrEmpty() || current == host || current == LOCAL_TRANSPORT
    }
}

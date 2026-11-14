package io.github.thisisthepy.dsa.agent.platform

import android.app.Service
import android.app.backup.BackupAgent
import android.app.backup.BackupDataInput
import android.app.backup.BackupDataOutput
import android.app.backup.BackupTransport
import android.app.backup.RestoreDescription
import android.app.backup.RestoreSet
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.IBinder
import android.os.ParcelFileDescriptor
import io.github.thisisthepy.dsa.agent.R
import io.github.thisisthepy.dsa.agent.backup.ApkFile
import io.github.thisisthepy.dsa.agent.backup.AppFacts
import io.github.thisisthepy.dsa.agent.backup.HostBackup
import io.github.thisisthepy.dsa.agent.backup.KeyValueRecord
import java.io.File
import java.io.FileDescriptor
import java.io.FileInputStream
import java.io.FileOutputStream

/**
 * The service Android's backup manager binds to reach the agent's backup transport. The
 * backup manager uses only a transport of a privileged app that the image's sysconfig lists
 * (sysconfig-dsa-agent.xml), and binds it with the android.backup.TRANSPORT_HOST action. It is
 * not exported: only the system, which may bind any service, reaches it.
 */
class HostBackupTransportService : Service() {
    override fun onBind(intent: Intent): IBinder? = transport(this).binder

    companion object {
        @Volatile
        private var instance: HostBackupTransport? = null

        fun transport(context: Context): HostBackupTransport =
            instance ?: synchronized(this) {
                instance ?: HostBackupTransport(context.applicationContext).also { instance = it }
            }

        /** The name the transport registers under, which the agent selects. */
        fun name(context: Context): String =
            ComponentName(context, HostBackupTransportService::class.java).flattenToShortString()
    }
}

/**
 * The guest's backup transport: every app's backup goes to the host, which keeps it encrypted
 * in the user's backup folder, outside the guest's disk ([HostBackup] does the talking). The
 * backup manager calls it on its binder threads, one call at a time, and each call returns
 * once the host has answered. Without the agent's connection every call fails, and the backup
 * manager tries again later.
 */
class HostBackupTransport(private val context: Context) : BackupTransport() {
    private fun backup(): HostBackup? = AgentApplication.agent?.hostBackup

    override fun name(): String = HostBackupTransportService.name(context)

    override fun transportDirName(): String = "dsa-host"

    override fun configurationIntent(): Intent? = null

    override fun currentDestinationString(): String = context.getString(R.string.backup_destination)

    override fun dataManagementIntent(): Intent? = null

    override fun dataManagementIntentLabel(): CharSequence? = null

    override fun dataManagementLabel(): String? = null

    override fun initializeDevice(): Int = TRANSPORT_OK

    override fun requestBackupTime(): Long = 0

    override fun requestFullBackupTime(): Long = 0

    /**
     * The data leaves the guest only for the host, which encrypts it with a key the guest never
     * holds: apps that back up only with client-side encryption back up here too.
     */
    override fun getTransportFlags(): Int = BackupAgent.FLAG_CLIENT_SIDE_ENCRYPTION_ENABLED

    override fun isAppEligibleForBackup(targetPackage: PackageInfo, isFullBackup: Boolean): Boolean = true

    override fun getBackupQuota(packageName: String, isFullBackup: Boolean): Long =
        if (isFullBackup) HostBackup.MAX_FULL_BYTES else HostBackup.MAX_KEY_VALUE_BYTES

    override fun clearBackupData(packageInfo: PackageInfo): Int =
        backup()?.clear(packageInfo.packageName) ?: TRANSPORT_ERROR

    override fun performBackup(packageInfo: PackageInfo, inFd: ParcelFileDescriptor, flags: Int): Int {
        val backup = backup() ?: return TRANSPORT_ERROR
        if ((flags and FLAG_DATA_NOT_CHANGED) != 0) return TRANSPORT_OK
        val records = try {
            readRecords(inFd.fileDescriptor)
        } catch (e: Exception) {
            AndroidLog.warn("reading ${packageInfo.packageName}'s key-value backup failed", e)
            return TRANSPORT_ERROR
        }
        val incremental = (flags and FLAG_NON_INCREMENTAL) == 0
        return backup.keyValue(facts(packageInfo), records, incremental)
    }

    override fun performBackup(packageInfo: PackageInfo, inFd: ParcelFileDescriptor): Int =
        performBackup(packageInfo, inFd, 0)

    override fun finishBackup(): Int = backup()?.finish() ?: TRANSPORT_ERROR

    override fun performFullBackup(targetPackage: PackageInfo, socket: ParcelFileDescriptor, flags: Int): Int {
        val backup = backup() ?: return TRANSPORT_ERROR
        // The backup manager owns the pipe and closes it; the stream is only read.
        return backup.startFull(facts(targetPackage), FileInputStream(socket.fileDescriptor))
    }

    override fun performFullBackup(targetPackage: PackageInfo, socket: ParcelFileDescriptor): Int =
        performFullBackup(targetPackage, socket, 0)

    override fun checkFullBackupSize(size: Long): Int = backup()?.checkFullSize(size) ?: TRANSPORT_ERROR

    override fun sendBackupData(numBytes: Int): Int = backup()?.sendFull(numBytes) ?: TRANSPORT_ERROR

    override fun cancelFullBackup() {
        backup()?.cancelFull()
    }

    override fun getAvailableRestoreSets(): Array<RestoreSet>? =
        backup()?.restoreSets()?.map { RestoreSet(it.name, it.name, it.token) }?.toTypedArray()

    override fun getCurrentRestoreSet(): Long = backup()?.currentToken() ?: 0

    override fun startRestore(token: Long, packages: Array<PackageInfo>): Int =
        backup()?.startRestore(token, packages.map { it.packageName }) ?: TRANSPORT_ERROR

    override fun nextRestorePackage(): RestoreDescription? {
        val backup = backup() ?: return null
        val next = backup.nextPackage() ?: return RestoreDescription.NO_MORE_PACKAGES
        return RestoreDescription(
            next.packageName,
            if (next.full) RestoreDescription.TYPE_FULL_STREAM else RestoreDescription.TYPE_KEY_VALUE,
        )
    }

    override fun getRestoreData(outFd: ParcelFileDescriptor): Int {
        val backup = backup() ?: return TRANSPORT_ERROR
        val records = backup.keyValueRecords() ?: return TRANSPORT_ERROR
        return try {
            val output = BACKUP_DATA_OUTPUT.newInstance(outFd.fileDescriptor)
            for (record in records) {
                val value = record.value ?: continue
                output.writeEntityHeader(record.key, value.size)
                output.writeEntityData(value, value.size)
            }
            TRANSPORT_OK
        } catch (e: Exception) {
            AndroidLog.warn("handing restored key-value data to Android failed", e)
            TRANSPORT_ERROR
        }
    }

    override fun getNextFullRestoreDataChunk(socket: ParcelFileDescriptor): Int {
        val backup = backup() ?: return TRANSPORT_ERROR
        // The backup manager owns the pipe and closes it; the stream is only written.
        val out = FileOutputStream(socket.fileDescriptor)
        return backup.nextFullChunk { bytes, offset, length -> out.write(bytes, offset, length) }
    }

    override fun abortFullRestore(): Int = backup()?.abortFull() ?: TRANSPORT_OK

    override fun finishRestore() {
        backup()?.finishRestore()
    }

    /** The app's version and, unless the image carries it, its APKs. */
    private fun facts(info: PackageInfo): AppFacts {
        val name = info.packageName
        if (name == HostBackup.PACKAGE_MANAGER) return AppFacts(name, 0, emptyList())
        val app: ApplicationInfo? = info.applicationInfo ?: try {
            context.packageManager.getApplicationInfo(name, 0)
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
        val fromImage = app != null && (app.flags and ApplicationInfo.FLAG_SYSTEM) != 0 &&
            (app.flags and ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) == 0
        val apks = if (app == null || fromImage) {
            emptyList()
        } else {
            listOfNotNull(app.sourceDir, *(app.splitSourceDirs ?: emptyArray()))
                .map(::File)
                .filter { it.isFile }
                .map { file -> ApkFile(file.name, file.length()) { FileInputStream(file) } }
        }
        return AppFacts(name, info.longVersionCode, apks)
    }

    private companion object {
        /** BackupDataInput(FileDescriptor) and BackupDataOutput(FileDescriptor) are system APIs. */
        val BACKUP_DATA_INPUT: java.lang.reflect.Constructor<BackupDataInput> =
            BackupDataInput::class.java.getConstructor(FileDescriptor::class.java)
        val BACKUP_DATA_OUTPUT: java.lang.reflect.Constructor<BackupDataOutput> =
            BackupDataOutput::class.java.getConstructor(FileDescriptor::class.java)

        /** The entities an app's backup agent wrote, a negative size marking a deleted key. */
        fun readRecords(fd: FileDescriptor): List<KeyValueRecord> {
            val input = BACKUP_DATA_INPUT.newInstance(fd)
            val records = mutableListOf<KeyValueRecord>()
            var total = 0L
            while (input.readNextHeader()) {
                val key = input.key
                val size = input.dataSize
                if (size < 0) {
                    records += KeyValueRecord(key, null)
                    continue
                }
                total += size
                if (total > HostBackup.MAX_KEY_VALUE_BYTES) throw java.io.IOException("the app's data is larger than the host keeps")
                val value = ByteArray(size)
                var read = 0
                while (read < size) {
                    val n = input.readEntityData(value, read, size - read)
                    if (n <= 0) throw java.io.IOException("the app's data ended early")
                    read += n
                }
                records += KeyValueRecord(key, value)
            }
            return records
        }
    }
}

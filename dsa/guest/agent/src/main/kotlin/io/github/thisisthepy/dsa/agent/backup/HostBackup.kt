package io.github.thisisthepy.dsa.agent.backup

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.channel.HostCalls
import io.github.thisisthepy.dsa.control.v1.BackupClear
import io.github.thisisthepy.dsa.control.v1.BackupCommit
import io.github.thisisthepy.dsa.control.v1.BackupDataKind
import io.github.thisisthepy.dsa.control.v1.BackupDiscard
import io.github.thisisthepy.dsa.control.v1.BackupFetch
import io.github.thisisthepy.dsa.control.v1.BackupFetchData
import io.github.thisisthepy.dsa.control.v1.BackupPackage
import io.github.thisisthepy.dsa.control.v1.BackupQuery
import io.github.thisisthepy.dsa.control.v1.BackupStatus
import io.github.thisisthepy.dsa.control.v1.BackupUpload
import io.github.thisisthepy.dsa.control.v1.Envelope
import java.io.InputStream

/**
 * The result codes of Android's backup transport (android.app.backup.BackupTransport), which
 * the platform's transport returns as they are.
 */
object TransportCodes {
    const val OK = 0
    const val NO_MORE_DATA = -1
    const val ERROR = -1000
    const val PACKAGE_REJECTED = -1002
    const val QUOTA_EXCEEDED = -1005

    /** BackupTransport.FLAG_INCREMENTAL and FLAG_NON_INCREMENTAL. */
    const val FLAG_INCREMENTAL = 1 shl 1
    const val FLAG_NON_INCREMENTAL = 1 shl 2

    /** BackupTransport.FLAG_DATA_NOT_CHANGED. */
    const val FLAG_DATA_NOT_CHANGED = 1 shl 3
}

/** One APK file of an app, to send so a restore after a data reset can install the app. */
class ApkFile(val name: String, val size: Long, val open: () -> InputStream)

/**
 * What the backup needs to know of an app, from the package manager. [apks] is empty for an
 * app the image itself carries (a system app that was not updated), which a new disk has
 * already, and for the package manager's own metadata (@pm@).
 */
class AppFacts(val packageName: String, val versionCode: Long, val apks: List<ApkFile>)

/** What a restore hands Android next: a package and whether its data is a full backup. */
data class RestorePackage(val packageName: String, val full: Boolean)

/** The restore set the host keeps: one, the user's backup folder. */
data class RestoreSetFacts(val token: Long, val name: String)

/**
 * The agent's backup transport, apart from Android: it keeps every app's backup on the host,
 * which stores it encrypted in the user's backup folder outside the guest's disk, and reads it
 * back for a restore after a data reset or on another computer.
 *
 * Every piece of data goes to the host in a message of at most [PIECE_BYTES], and the next
 * piece goes only once the host has answered the last ([HostCalls] blocks the backup manager's
 * thread meanwhile), so the host never holds more than one piece in flight. An upload is kept
 * only when its commit says what it is (the app, the kind, the size). After an app's data, the
 * host may ask for the app's APKs, which it keeps per app version.
 *
 * A restore reads the backup's list first, then hands Android the packages it asked for in
 * the order Android asked (the package manager's metadata, @pm@, first), each as key-value
 * records or as a full backup's tar stream, read from the host piece by piece as Android
 * consumes it.
 *
 * The backup manager calls one thing at a time, on its own threads; the methods are
 * synchronized so the state stays whole if it ever did not.
 */
class HostBackup(
    private val calls: HostCalls,
    private val log: Log,
    private val pieceWaitMillis: Long = PIECE_WAIT_MILLIS,
    private val commitWaitMillis: Long = COMMIT_WAIT_MILLIS,
) {
    /** A full backup under way: the stream Android writes into, and how far it came. */
    private class FullUpload(val id: Long, val app: AppFacts, val input: InputStream) {
        var offset = 0L
        var failed = false
    }

    private var full: FullUpload? = null

    private var restoreContents: Map<String, BackupPackage> = emptyMap()
    private var restoreToken = 0L
    private val restoreQueue = ArrayDeque<RestorePackage>()
    private var current: RestorePackage? = null
    private var fullRead: FullRead? = null
    private var lastToken = 0L

    /** How far a restore got: the apps whose data Android was given whole, and those it was not. */
    @Volatile
    var restored = 0
        private set

    @Volatile
    var restoreFailed = 0
        private set

    private class FullRead(val id: Long, val packageName: String) {
        var offset = 0L
        var done = false
    }

    // Backup.

    /**
     * Keeps the key-value [records] of [app] on the host: all of them, or with [incremental]
     * only what changed since the last backup, which the host merges into what it holds.
     */
    @Synchronized
    fun keyValue(app: AppFacts, records: List<KeyValueRecord>, incremental: Boolean): Int {
        val data = KeyValueRecords.encode(records)
        if (data.size > MAX_KEY_VALUE_BYTES) return TransportCodes.QUOTA_EXCEEDED
        val id = calls.nextId()
        var offset = 0
        while (offset < data.size) {
            val end = minOf(data.size, offset + PIECE_BYTES)
            val code = sendPiece(id, offset.toLong(), data, offset, end - offset)
            if (code != TransportCodes.OK) return code
            offset = end
        }
        val commit = BackupCommit.newBuilder()
            .setRequestId(id)
            .setPackage(app.packageName)
            .setKind(BackupDataKind.BACKUP_DATA_KIND_KEY_VALUE)
            .setSize(data.size.toLong())
            .setIncremental(incremental)
            .setVersionCode(app.versionCode)
        return commitThenApks(app, commit, "key-value data")
    }

    /** Android starts a full backup of [app], whose tar stream it writes into [input]. */
    @Synchronized
    fun startFull(app: AppFacts, input: InputStream): Int {
        full?.let { discard(it.id) }
        full = FullUpload(calls.nextId(), app, input)
        return TransportCodes.OK
    }

    /** Whether a full backup of [size] bytes may go to the host. */
    fun checkFullSize(size: Long): Int = when {
        size <= 0 -> TransportCodes.PACKAGE_REJECTED
        size > MAX_FULL_BYTES -> TransportCodes.QUOTA_EXCEEDED
        else -> TransportCodes.OK
    }

    /** The next [numBytes] of the full backup are in its stream: they go to the host. */
    @Synchronized
    fun sendFull(numBytes: Int): Int {
        val upload = full ?: return TransportCodes.ERROR
        if (upload.failed) return TransportCodes.ERROR
        if (upload.offset + numBytes > MAX_FULL_BYTES) {
            upload.failed = true
            return TransportCodes.QUOTA_EXCEEDED
        }
        val buffer = ByteArray(minOf(numBytes, PIECE_BYTES).coerceAtLeast(1))
        var left = numBytes
        while (left > 0) {
            val wanted = minOf(left, buffer.size)
            var filled = 0
            while (filled < wanted) {
                val read = try {
                    upload.input.read(buffer, filled, wanted - filled)
                } catch (e: java.io.IOException) {
                    log.warn("reading ${upload.app.packageName}'s full backup failed", e)
                    -1
                }
                if (read < 0) {
                    upload.failed = true
                    return TransportCodes.ERROR
                }
                filled += read
            }
            val code = sendPiece(upload.id, upload.offset, buffer, 0, filled)
            if (code != TransportCodes.OK) {
                upload.failed = true
                return code
            }
            upload.offset += filled
            left -= filled
        }
        return TransportCodes.OK
    }

    /** Android gave up on the full backup under way: the host drops what it has of it. */
    @Synchronized
    fun cancelFull() {
        val upload = full ?: return
        full = null
        discard(upload.id)
    }

    /** The backup of the app under way is whole: a full backup is committed now. */
    @Synchronized
    fun finish(): Int {
        val upload = full ?: return TransportCodes.OK
        full = null
        if (upload.failed) {
            discard(upload.id)
            return TransportCodes.ERROR
        }
        val commit = BackupCommit.newBuilder()
            .setRequestId(upload.id)
            .setPackage(upload.app.packageName)
            .setKind(BackupDataKind.BACKUP_DATA_KIND_FULL)
            .setSize(upload.offset)
            .setVersionCode(upload.app.versionCode)
        return commitThenApks(upload.app, commit, "full backup")
    }

    /** Removes everything the host keeps of [packageName]. */
    @Synchronized
    fun clear(packageName: String): Int {
        val id = calls.nextId()
        val request = Envelope.newBuilder()
            .setBackupClear(BackupClear.newBuilder().setRequestId(id).setPackage(packageName))
            .build()
        val answer = calls.call(id, request, commitWaitMillis)?.takeIf { it.hasBackupAnswer() }?.backupAnswer
        return code(answer?.status, answer?.message, "clearing $packageName's backup")
    }

    private fun commitThenApks(app: AppFacts, commit: BackupCommit.Builder, what: String): Int {
        val id = commit.requestId
        val request = Envelope.newBuilder().setBackupCommit(commit).build()
        val answer = calls.call(id, request, commitWaitMillis)?.takeIf { it.hasBackupAnswer() }?.backupAnswer
        if (answer == null) discard(id)
        val result = code(answer?.status, answer?.message, "keeping ${app.packageName}'s $what")
        if (result != TransportCodes.OK || answer == null) return result
        if (answer.apksWanted && app.apks.isNotEmpty()) sendApks(app)
        return TransportCodes.OK
    }

    /**
     * The app's APKs, so a restore after a data reset can install it. A failure here leaves
     * the app's data backed up; the host asks again at the next backup.
     */
    private fun sendApks(app: AppFacts) {
        val names = app.apks.map { it.name }
        for (apk in app.apks) {
            val id = calls.nextId()
            var offset = 0L
            val ok = try {
                apk.open().use { input ->
                    val buffer = ByteArray(PIECE_BYTES)
                    while (true) {
                        val read = readFully(input, buffer)
                        if (read <= 0) break
                        if (sendPiece(id, offset, buffer, 0, read) != TransportCodes.OK) return@use false
                        offset += read
                    }
                    true
                }
            } catch (e: Exception) {
                log.warn("reading ${app.packageName}'s ${apk.name} for the backup failed", e)
                false
            }
            if (!ok) {
                discard(id)
                log.warn("${app.packageName}'s APKs did not reach the backup; the next backup sends them")
                return
            }
            val commit = BackupCommit.newBuilder()
                .setRequestId(id)
                .setPackage(app.packageName)
                .setKind(BackupDataKind.BACKUP_DATA_KIND_APK)
                .setSize(offset)
                .setVersionCode(app.versionCode)
                .setApkName(apk.name)
                .addAllApkNames(names)
            val answer = calls.call(id, Envelope.newBuilder().setBackupCommit(commit).build(), commitWaitMillis)
                ?.takeIf { it.hasBackupAnswer() }?.backupAnswer
            if (code(answer?.status, answer?.message, "keeping ${app.packageName}'s ${apk.name}") != TransportCodes.OK) {
                return
            }
        }
    }

    private fun readFully(input: InputStream, buffer: ByteArray): Int {
        var filled = 0
        while (filled < buffer.size) {
            val read = input.read(buffer, filled, buffer.size - filled)
            if (read < 0) break
            filled += read
        }
        return filled
    }

    private fun sendPiece(id: Long, offset: Long, data: ByteArray, start: Int, length: Int): Int {
        val piece = BackupUpload.newBuilder()
            .setRequestId(id)
            .setOffset(offset)
            .setData(ByteString.copyFrom(data, start, length))
        val answer = calls.call(id, Envelope.newBuilder().setBackupUpload(piece).build(), pieceWaitMillis)
            ?.takeIf { it.hasBackupAnswer() }?.backupAnswer
        if (answer == null) discard(id)
        return code(answer?.status, answer?.message, "sending a backup to the host")
    }

    private fun discard(id: Long) {
        calls.tell(Envelope.newBuilder().setBackupDiscard(BackupDiscard.newBuilder().setRequestId(id)).build())
    }

    /** The transport's code for what the host said; null is no answer at all. */
    private fun code(status: BackupStatus?, message: String?, what: String): Int = when (status) {
        BackupStatus.BACKUP_STATUS_OK -> TransportCodes.OK
        BackupStatus.BACKUP_STATUS_QUOTA_EXCEEDED -> {
            log.warn("$what: the host keeps no more of it (${message.orEmpty()})")
            TransportCodes.QUOTA_EXCEEDED
        }
        null -> {
            log.warn("$what: the host did not answer")
            TransportCodes.ERROR
        }
        else -> {
            log.warn("$what failed: ${message.orEmpty().ifEmpty { status.name }}")
            TransportCodes.ERROR
        }
    }

    // Restore.

    /** What the host's backup holds; null when it cannot be read. */
    private fun query(): Pair<RestoreSetFacts, List<BackupPackage>>? {
        val id = calls.nextId()
        val request = Envelope.newBuilder().setBackupQuery(BackupQuery.newBuilder().setRequestId(id)).build()
        val answer = calls.call(id, request, commitWaitMillis)?.takeIf { it.hasBackupContents() }?.backupContents
        if (answer == null) {
            log.warn("the host did not say what its backup holds")
            return null
        }
        if (answer.status != BackupStatus.BACKUP_STATUS_OK) {
            log.warn("the host's backup cannot be read: ${answer.message.ifEmpty { answer.status.name }}")
            return null
        }
        lastToken = answer.token
        return RestoreSetFacts(answer.token, answer.name) to answer.packagesList
    }

    /** The restore sets Android may restore from: the host's one; null when it cannot be read. */
    @Synchronized
    fun restoreSets(): List<RestoreSetFacts>? {
        val (set, packages) = query() ?: return null
        return if (packages.isEmpty()) emptyList() else listOf(set)
    }

    /** The token of the restore set this device backs up to; 0 when there is none yet. */
    @Synchronized
    fun currentToken(): Long {
        if (lastToken != 0L) return lastToken
        return query()?.first?.token ?: 0L
    }

    /** Android restores [packages] from the set [token], in that order. */
    @Synchronized
    fun startRestore(token: Long, packages: List<String>): Int {
        endRestore()
        restored = 0
        restoreFailed = 0
        val (set, contents) = query() ?: return TransportCodes.ERROR
        if (set.token != token) {
            log.warn("Android asked to restore from set $token; the host has only ${set.token}")
            return TransportCodes.ERROR
        }
        restoreToken = token
        restoreContents = contents.associateBy { it.getPackage() }
        for (name in packages) {
            val held = restoreContents[name] ?: continue
            when {
                held.full -> restoreQueue.addLast(RestorePackage(name, full = true))
                held.keyValue -> restoreQueue.addLast(RestorePackage(name, full = false))
            }
        }
        log.info("restoring ${restoreQueue.count { it.packageName != PACKAGE_MANAGER }} apps from the host's backup")
        return TransportCodes.OK
    }

    /** The next package to restore; null when there is none left. */
    @Synchronized
    fun nextPackage(): RestorePackage? {
        finishCurrent()
        val next = restoreQueue.removeFirstOrNull()
        current = next
        if (next != null && next.full) fullRead = FullRead(calls.nextId(), next.packageName)
        return next
    }

    /** The key-value records of the package being restored; null when they cannot be had. */
    @Synchronized
    fun keyValueRecords(): List<KeyValueRecord>? {
        val now = current ?: return null
        if (now.full) return null
        val id = calls.nextId()
        val data = java.io.ByteArrayOutputStream()
        var offset = 0L
        while (true) {
            val piece = fetch(id, now.packageName, BackupDataKind.BACKUP_DATA_KIND_KEY_VALUE, offset) ?: run {
                countFailed(now)
                return null
            }
            data.write(piece.data.toByteArray())
            offset += piece.data.size()
            if (piece.last || piece.data.isEmpty) break
            if (data.size() > MAX_KEY_VALUE_BYTES) {
                discard(id)
                countFailed(now)
                return null
            }
        }
        val records = KeyValueRecords.decode(data.toByteArray())
        if (records == null) {
            log.warn("the host's key-value data of ${now.packageName} is not in the form it is kept in")
            countFailed(now)
            return null
        }
        countRestored(now)
        current = null
        return records.filter { it.value != null }
    }

    /**
     * Hands the next piece of the full backup being restored to [write]; the number of bytes
     * written, [TransportCodes.NO_MORE_DATA] once it is all written, or an error.
     */
    @Synchronized
    fun nextFullChunk(write: (ByteArray, Int, Int) -> Unit): Int {
        val read = fullRead ?: return TransportCodes.ERROR
        if (read.done) return TransportCodes.NO_MORE_DATA
        val piece = fetch(read.id, read.packageName, BackupDataKind.BACKUP_DATA_KIND_FULL, read.offset)
        if (piece == null) {
            fullRead = null
            current?.let { countFailed(it) }
            current = null
            return TransportCodes.ERROR
        }
        val bytes = piece.data.toByteArray()
        read.offset += bytes.size
        if (piece.last || bytes.isEmpty()) read.done = true
        if (bytes.isEmpty()) return TransportCodes.NO_MORE_DATA
        try {
            write(bytes, 0, bytes.size)
        } catch (e: java.io.IOException) {
            log.warn("handing ${read.packageName}'s full backup to Android failed", e)
            discard(read.id)
            fullRead = null
            current?.let { countFailed(it) }
            current = null
            return TransportCodes.ERROR
        }
        return bytes.size
    }

    /** Android stopped reading the full backup being restored. */
    @Synchronized
    fun abortFull(): Int {
        val read = fullRead ?: return TransportCodes.OK
        if (!read.done) discard(read.id)
        fullRead = null
        current?.let { countFailed(it) }
        current = null
        return TransportCodes.OK
    }

    /** The restore is over. */
    @Synchronized
    fun finishRestore() {
        finishCurrent()
        for (left in restoreQueue) countFailed(left)
        endRestore()
    }

    private fun endRestore() {
        fullRead?.let { if (!it.done) discard(it.id) }
        fullRead = null
        current = null
        restoreQueue.clear()
        restoreContents = emptyMap()
    }

    /** A package whose full data Android read to the end counts as restored. */
    private fun finishCurrent() {
        val now = current ?: return
        val read = fullRead
        if (now.full && read != null && read.done) countRestored(now) else countFailed(now)
        fullRead = null
        current = null
    }

    private fun countRestored(app: RestorePackage) {
        if (app.packageName != PACKAGE_MANAGER) restored++
    }

    private fun countFailed(app: RestorePackage) {
        if (app.packageName != PACKAGE_MANAGER) restoreFailed++
    }

    private fun fetch(id: Long, packageName: String, kind: BackupDataKind, offset: Long): BackupFetchData? {
        val request = BackupFetch.newBuilder()
            .setRequestId(id)
            .setToken(restoreToken)
            .setPackage(packageName)
            .setKind(kind)
            .setOffset(offset)
        val answer = calls.call(id, Envelope.newBuilder().setBackupFetch(request).build(), pieceWaitMillis)
            ?.takeIf { it.hasBackupFetchData() }?.backupFetchData
        if (answer == null) {
            discard(id)
            log.warn("the host did not send $packageName's backup")
            return null
        }
        if (answer.status != BackupStatus.BACKUP_STATUS_OK) {
            log.warn("the host could not send $packageName's backup: ${answer.message.ifEmpty { answer.status.name }}")
            return null
        }
        if (answer.offset != offset) {
            discard(id)
            log.warn("the host sent $packageName's backup from ${answer.offset}, not $offset")
            return null
        }
        return answer
    }

    companion object {
        /** The largest piece of data in one message, either way. */
        const val PIECE_BYTES = 256 * 1024

        /** The most the host keeps of one app: its key-value data, and its full backup. */
        const val MAX_KEY_VALUE_BYTES = 64L * 1024 * 1024
        const val MAX_FULL_BYTES = 4L * 1024 * 1024 * 1024

        /** The package Android's backup manager keeps the package manager's metadata under. */
        const val PACKAGE_MANAGER = "@pm@"

        const val PIECE_WAIT_MILLIS = 60_000L
        const val COMMIT_WAIT_MILLIS = 120_000L
    }
}

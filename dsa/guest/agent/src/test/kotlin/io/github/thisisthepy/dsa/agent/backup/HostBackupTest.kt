package io.github.thisisthepy.dsa.agent.backup

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.channel.HostCalls
import io.github.thisisthepy.dsa.control.v1.BackupAnswer
import io.github.thisisthepy.dsa.control.v1.BackupContents
import io.github.thisisthepy.dsa.control.v1.BackupDataKind
import io.github.thisisthepy.dsa.control.v1.BackupFetchData
import io.github.thisisthepy.dsa.control.v1.BackupPackage
import io.github.thisisthepy.dsa.control.v1.BackupStatus
import io.github.thisisthepy.dsa.control.v1.Envelope
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The host's backup service, faked in memory as the host's BackupService behaves: uploads are
 * kept per request until committed, key-value records are merged, each commit says whether the
 * host wants the app's APKs, and a restore reads the data back in pieces.
 */
class FakeBackupHost(val token: Long = 0x5eed) {
    class Kept(val kind: BackupDataKind, val data: ByteArray, val versionCode: Long, val apkName: String = "")

    val uploads = mutableMapOf<Long, ByteArrayOutputStream>()
    val pieces = mutableListOf<Pair<Long, Int>>()
    val keyValue = mutableMapOf<String, MutableMap<String, ByteArray>>()
    val full = mutableMapOf<String, ByteArray>()
    val apks = mutableMapOf<String, MutableMap<String, ByteArray>>()
    val versions = mutableMapOf<String, Long>()
    val discarded = mutableListOf<Long>()
    val cleared = mutableListOf<String>()

    /** What the host answers instead of OK, when set. */
    var refuse: BackupStatus? = null
    var answers = true

    lateinit var calls: HostCalls

    fun send(frame: ByteArray): Boolean {
        val envelope = Envelope.parseFrom(frame)
        val answer = answer(envelope) ?: return true
        if (answers) calls.handle(answer)
        return true
    }

    private fun ok(id: Long, apksWanted: Boolean = false): Envelope {
        val status = refuse ?: BackupStatus.BACKUP_STATUS_OK
        return Envelope.newBuilder().setBackupAnswer(
            BackupAnswer.newBuilder().setRequestId(id).setStatus(status).setMessage(if (refuse != null) "refused" else "")
                .setApksWanted(apksWanted && refuse == null),
        ).build()
    }

    private fun answer(envelope: Envelope): Envelope? = when (envelope.bodyCase) {
        Envelope.BodyCase.BACKUP_UPLOAD -> {
            val upload = envelope.backupUpload
            assertTrue("a piece is at most 256 KiB", upload.data.size() <= 256 * 1024)
            val buffer = uploads.getOrPut(upload.requestId) { ByteArrayOutputStream() }
            assertEquals("pieces come in order", buffer.size().toLong(), upload.offset)
            buffer.write(upload.data.toByteArray())
            pieces += upload.requestId to upload.data.size()
            ok(upload.requestId)
        }
        Envelope.BodyCase.BACKUP_COMMIT -> {
            val commit = envelope.backupCommit
            val data = uploads.remove(commit.requestId)?.toByteArray() ?: ByteArray(0)
            assertEquals(commit.size, data.size.toLong())
            val name = commit.getPackage()
            if (refuse != null) {
                ok(commit.requestId)
            } else {
                when (commit.kind) {
                    BackupDataKind.BACKUP_DATA_KIND_KEY_VALUE -> {
                        val map = if (commit.incremental) keyValue.getOrPut(name) { mutableMapOf() } else mutableMapOf()
                        for (record in KeyValueRecords.decode(data)!!) {
                            if (record.value == null) map.remove(record.key) else map[record.key] = record.value!!
                        }
                        keyValue[name] = map
                    }
                    BackupDataKind.BACKUP_DATA_KIND_FULL -> full[name] = data
                    BackupDataKind.BACKUP_DATA_KIND_APK -> {
                        assertTrue(commit.apkName in commit.apkNamesList)
                        apks.getOrPut(name) { mutableMapOf() }[commit.apkName] = data
                    }
                    else -> error("unknown kind")
                }
                val wanted = commit.kind != BackupDataKind.BACKUP_DATA_KIND_APK && name != "@pm@" &&
                    versions[name] != commit.versionCode
                if (commit.kind == BackupDataKind.BACKUP_DATA_KIND_APK) versions[name] = commit.versionCode
                ok(commit.requestId, apksWanted = wanted)
            }
        }
        Envelope.BodyCase.BACKUP_DISCARD -> {
            discarded += envelope.backupDiscard.requestId
            uploads.remove(envelope.backupDiscard.requestId)
            null
        }
        Envelope.BodyCase.BACKUP_CLEAR -> {
            cleared += envelope.backupClear.getPackage()
            keyValue.remove(envelope.backupClear.getPackage())
            ok(envelope.backupClear.requestId)
        }
        Envelope.BodyCase.BACKUP_QUERY -> {
            val names = (keyValue.keys + full.keys).toSortedSet()
            val contents = BackupContents.newBuilder()
                .setRequestId(envelope.backupQuery.requestId)
                .setStatus(refuse ?: BackupStatus.BACKUP_STATUS_OK)
                .setToken(token)
                .setName("DSA backup of TEST-PC")
            for (name in names) {
                contents.addPackages(
                    BackupPackage.newBuilder().setPackage(name)
                        .setKeyValue(name in keyValue).setFull(name in full),
                )
            }
            Envelope.newBuilder().setBackupContents(contents).build()
        }
        Envelope.BodyCase.BACKUP_FETCH -> {
            val fetch = envelope.backupFetch
            val name = fetch.getPackage()
            val data = when (fetch.kind) {
                BackupDataKind.BACKUP_DATA_KIND_KEY_VALUE -> keyValue[name]?.let { map ->
                    KeyValueRecords.encode(map.map { KeyValueRecord(it.key, it.value) })
                }
                BackupDataKind.BACKUP_DATA_KIND_FULL -> full[name]
                else -> null
            }
            val reply = BackupFetchData.newBuilder().setRequestId(fetch.requestId)
            if (fetch.token != token || data == null) {
                reply.setStatus(BackupStatus.BACKUP_STATUS_NOT_FOUND)
            } else {
                val end = minOf(data.size.toLong(), fetch.offset + 256 * 1024).toInt()
                reply.setStatus(BackupStatus.BACKUP_STATUS_OK)
                    .setSize(data.size.toLong())
                    .setOffset(fetch.offset)
                    .setData(ByteString.copyFrom(data, fetch.offset.toInt(), end - fetch.offset.toInt()))
                    .setLast(end >= data.size)
            }
            Envelope.newBuilder().setBackupFetchData(reply).build()
        }
        else -> null
    }
}

class HostBackupTest {
    private val log = RecordingLog()

    private fun transport(host: FakeBackupHost): HostBackup {
        val calls = HostCalls(host::send, log)
        host.calls = calls
        return HostBackup(calls, log, pieceWaitMillis = 1000, commitWaitMillis = 1000)
    }

    private fun bytes(size: Int, seed: Int): ByteArray = ByteArray(size) { ((it * 31 + seed) and 0xff).toByte() }

    private fun app(name: String, version: Long = 7, apks: List<ApkFile> = emptyList()) = AppFacts(name, version, apks)

    @Test
    fun fr36_backup_transport_sends_key_value_records_in_pieces_and_commits_them() {
        val host = FakeBackupHost()
        val backup = transport(host)
        val records = listOf(
            KeyValueRecord("settings", bytes(600 * 1024, 1)),
            KeyValueRecord("name", "Ada".toByteArray()),
        )
        assertEquals(TransportCodes.OK, backup.keyValue(app("org.example.notes"), records, incremental = false))
        assertTrue("the records went in several pieces", host.pieces.size >= 3)
        assertArrayEquals(records[0].value, host.keyValue["org.example.notes"]!!["settings"])
        assertArrayEquals("Ada".toByteArray(), host.keyValue["org.example.notes"]!!["name"])
        // An incremental backup deletes and changes only what it names.
        val change = listOf(KeyValueRecord("name", null), KeyValueRecord("theme", "dark".toByteArray()))
        assertEquals(TransportCodes.OK, backup.keyValue(app("org.example.notes"), change, incremental = true))
        assertEquals(setOf("settings", "theme"), host.keyValue["org.example.notes"]!!.keys)
    }

    @Test
    fun fr36_backup_transport_sends_an_apps_apks_when_the_host_has_none_of_its_version() {
        val host = FakeBackupHost()
        val backup = transport(host)
        val base = bytes(300 * 1024, 2)
        val split = bytes(10, 3)
        val apks = listOf(
            ApkFile("base.apk", base.size.toLong()) { ByteArrayInputStream(base) },
            ApkFile("split_config.en.apk", split.size.toLong()) { ByteArrayInputStream(split) },
        )
        backup.keyValue(app("org.example.notes", 7, apks), listOf(KeyValueRecord("k", byteArrayOf(1))), false)
        assertArrayEquals(base, host.apks["org.example.notes"]!!["base.apk"])
        assertArrayEquals(split, host.apks["org.example.notes"]!!["split_config.en.apk"])
        // The host holds them now, so the next backup of the same version sends none.
        host.apks.clear()
        backup.keyValue(app("org.example.notes", 7, apks), listOf(KeyValueRecord("k", byteArrayOf(2))), false)
        assertTrue(host.apks.isEmpty())
        // An app the image carries has no APKs to send.
        backup.keyValue(app("com.android.settings"), listOf(KeyValueRecord("k", byteArrayOf(2))), false)
        assertNull(host.apks["com.android.settings"])
    }

    @Test
    fun fr36_backup_transport_streams_a_full_backup_and_commits_it_when_finished() {
        val host = FakeBackupHost()
        val backup = transport(host)
        val tar = bytes(700 * 1024 + 5, 4)
        val stream = ByteArrayInputStream(tar)
        assertEquals(TransportCodes.OK, backup.checkFullSize(tar.size.toLong()))
        assertEquals(TransportCodes.OK, backup.startFull(app("org.example.game"), stream))
        assertEquals(TransportCodes.OK, backup.sendFull(400 * 1024))
        assertEquals(TransportCodes.OK, backup.sendFull(tar.size - 400 * 1024))
        assertNull("nothing is kept before the backup is whole", host.full["org.example.game"])
        assertEquals(TransportCodes.OK, backup.finish())
        assertArrayEquals(tar, host.full["org.example.game"])
    }

    @Test
    fun fr36_backup_transport_drops_a_canceled_full_backup() {
        val host = FakeBackupHost()
        val backup = transport(host)
        backup.startFull(app("org.example.game"), ByteArrayInputStream(bytes(1000, 5)))
        backup.sendFull(500)
        backup.cancelFull()
        assertEquals(TransportCodes.OK, backup.finish())
        assertNull(host.full["org.example.game"])
        assertTrue(host.discarded.isNotEmpty())
        assertEquals(TransportCodes.PACKAGE_REJECTED, backup.checkFullSize(0))
        assertEquals(TransportCodes.QUOTA_EXCEEDED, backup.checkFullSize(HostBackup.MAX_FULL_BYTES + 1))
    }

    @Test
    fun fr36_backup_transport_reports_what_the_host_refuses() {
        val host = FakeBackupHost()
        val backup = transport(host)
        host.refuse = BackupStatus.BACKUP_STATUS_QUOTA_EXCEEDED
        assertEquals(TransportCodes.QUOTA_EXCEEDED, backup.keyValue(app("a.b"), listOf(KeyValueRecord("k", byteArrayOf(1))), false))
        host.refuse = BackupStatus.BACKUP_STATUS_NOT_AVAILABLE
        assertEquals(TransportCodes.ERROR, backup.keyValue(app("a.b"), listOf(KeyValueRecord("k", byteArrayOf(1))), false))
        host.refuse = null
        host.answers = false
        assertEquals("a host that does not answer fails the call", TransportCodes.ERROR, backup.clear("a.b"))
    }

    @Test
    fun fr36_backup_transport_restores_in_the_order_android_asks() {
        val host = FakeBackupHost()
        val backup = transport(host)
        host.keyValue["@pm@"] = mutableMapOf("meta" to byteArrayOf(9))
        host.keyValue["org.example.notes"] = mutableMapOf("name" to "Ada".toByteArray())
        val tar = bytes(300 * 1024, 6)
        host.full["org.example.game"] = tar
        val sets = backup.restoreSets()!!
        assertEquals(listOf(RestoreSetFacts(host.token, "DSA backup of TEST-PC")), sets)
        assertEquals(
            TransportCodes.OK,
            backup.startRestore(host.token, listOf("@pm@", "org.example.notes", "org.example.missing", "org.example.game")),
        )
        assertEquals(RestorePackage("@pm@", full = false), backup.nextPackage())
        assertEquals(listOf(KeyValueRecord("meta", byteArrayOf(9))), backup.keyValueRecords())
        assertEquals(RestorePackage("org.example.notes", full = false), backup.nextPackage())
        assertEquals(listOf(KeyValueRecord("name", "Ada".toByteArray())), backup.keyValueRecords())
        assertEquals(RestorePackage("org.example.game", full = true), backup.nextPackage())
        val read = ByteArrayOutputStream()
        while (true) {
            val n = backup.nextFullChunk { b, off, len -> read.write(b, off, len) }
            if (n == TransportCodes.NO_MORE_DATA) break
            assertTrue(n > 0)
        }
        assertArrayEquals(tar, read.toByteArray())
        assertNull(backup.nextPackage())
        backup.finishRestore()
        assertEquals(2, backup.restored)
        assertEquals(0, backup.restoreFailed)
    }

    @Test
    fun fr36_backup_transport_refuses_a_restore_set_the_host_does_not_have() {
        val host = FakeBackupHost()
        val backup = transport(host)
        host.keyValue["org.example.notes"] = mutableMapOf("name" to byteArrayOf(1))
        assertEquals(TransportCodes.ERROR, backup.startRestore(host.token + 1, listOf("org.example.notes")))
        assertNull(backup.nextPackage())
    }

    @Test
    fun fr36_backup_survives_a_data_reset() {
        val host = FakeBackupHost()
        val before = transport(host)
        val records = listOf(KeyValueRecord("score", "42".toByteArray()), KeyValueRecord("level", bytes(1000, 7)))
        val tar = bytes(512 * 1024 + 1, 8)
        before.keyValue(app("org.example.game"), records, incremental = false)
        before.startFull(app("org.example.photos"), ByteArrayInputStream(tar))
        before.sendFull(tar.size)
        before.finish()
        // A data reset gives a new disk and a new agent; the host's folder stays.
        val after = transport(host)
        val token = after.currentToken()
        assertEquals(TransportCodes.OK, after.startRestore(token, listOf("@pm@", "org.example.game", "org.example.photos")))
        assertEquals("org.example.game", after.nextPackage()!!.packageName)
        assertEquals(records.toSet(), after.keyValueRecords()!!.toSet())
        assertEquals("org.example.photos", after.nextPackage()!!.packageName)
        val read = ByteArrayOutputStream()
        while (after.nextFullChunk { b, off, len -> read.write(b, off, len) } > 0) Unit
        assertArrayEquals(tar, read.toByteArray())
        assertNull(after.nextPackage())
        after.finishRestore()
        assertEquals(2, after.restored)
    }

    @Test
    fun fr36_backup_transport_counts_a_restore_android_left_unread_as_failed() {
        val host = FakeBackupHost()
        val backup = transport(host)
        host.full["org.example.game"] = bytes(400 * 1024, 9)
        backup.startRestore(host.token, listOf("org.example.game"))
        backup.nextPackage()
        backup.nextFullChunk { _, _, _ -> }
        assertEquals(TransportCodes.OK, backup.abortFull())
        backup.finishRestore()
        assertEquals(0, backup.restored)
        assertEquals(1, backup.restoreFailed)
    }

    @Test
    fun fr36_backup_records_round_trip_and_malformed_ones_are_refused() {
        val records = listOf(KeyValueRecord("ключ", byteArrayOf(1, 2)), KeyValueRecord("gone", null), KeyValueRecord("empty", ByteArray(0)))
        assertEquals(records, KeyValueRecords.decode(KeyValueRecords.encode(records)))
        val data = KeyValueRecords.encode(records)
        assertNull(KeyValueRecords.decode(data.copyOf(data.size - 1)))
        assertNull(KeyValueRecords.decode(byteArrayOf(0x7f, 0, 0, 0)))
    }
}

package io.github.thisisthepy.dsa.agent.backup

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.channel.HostCalls
import io.github.thisisthepy.dsa.control.v1.BackupRestore
import io.github.thisisthepy.dsa.control.v1.BackupRun
import io.github.thisisthepy.dsa.control.v1.BackupRunResult
import io.github.thisisthepy.dsa.control.v1.BackupSetting
import io.github.thisisthepy.dsa.control.v1.BackupStatus
import io.github.thisisthepy.dsa.control.v1.Envelope
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Android's backup manager, faked: it records the calls and finishes when the test says. */
class FakeBackupManager : BackupManagerPort {
    var transportReady = false
    var selections = 0
    var turnedOff = 0

    /** Whether Android's backup manager backs up and schedules now. */
    var backingUp = false
    val backups = mutableListOf<(BackupOutcome) -> Unit>()
    val restores = mutableListOf<Triple<Long, List<String>, (String?) -> Unit>>()

    override fun useHostTransport(): Boolean {
        selections++
        if (transportReady) backingUp = true
        return transportReady
    }

    override fun keepOff(): Boolean {
        turnedOff++
        backingUp = false
        return true
    }

    override fun backUpAll(done: (BackupOutcome) -> Unit) {
        backups += done
    }

    override fun restore(token: Long, packages: List<String>, done: (String?) -> Unit) {
        restores += Triple(token, packages, done)
    }
}

class BackupRunsTest {
    private val log = RecordingLog()
    private val manager = FakeBackupManager()
    private val sent = mutableListOf<BackupRunResult>()
    private val posted = ArrayDeque<() -> Unit>()
    private val later = mutableListOf<Pair<Long, () -> Unit>>()
    private val backup = HostBackup(HostCalls({ false }, log), log)
    private val runs = BackupRuns(
        manager,
        backup,
        { frame -> sent += Envelope.parseFrom(frame).backupRunResult; true },
        log,
        post = { posted.addLast(it) },
        later = { delay, work -> later += delay to work },
    )

    private fun runPosted() {
        while (posted.isNotEmpty()) posted.removeFirst()()
    }

    private fun run(id: Long) = Envelope.newBuilder().setBackupRun(BackupRun.newBuilder().setRequestId(id)).build()

    private fun setting(on: Boolean) =
        Envelope.newBuilder().setBackupSetting(BackupSetting.newBuilder().setEnabled(on)).build()

    @Test
    fun fr36_backup_is_kept_off_until_the_host_says_the_user_turned_it_on() {
        runs.start()
        assertEquals("nothing is selected while backup is off", 0, manager.selections)
        assertEquals(1, manager.turnedOff)
        assertFalse(manager.backingUp)
        assertFalse(runs.enabled)
        // Kept off, in case something turns it on meanwhile.
        assertEquals(BackupRuns.CHECK_MILLIS, later.single().first)
        later.removeAt(0).second()
        assertEquals(2, manager.turnedOff)
        // The host's runs are refused while it is off, and Android is not asked.
        runs.handle(run(5))
        assertEquals(BackupStatus.BACKUP_STATUS_NOT_AVAILABLE, sent.single().status)
        assertTrue(sent.single().message.contains("backup is off"))
        assertTrue(manager.backups.isEmpty())
        // The host saying it is still off changes nothing.
        assertTrue(runs.handle(setting(false)))
        assertEquals(0, manager.selections)
    }

    @Test
    fun fr36_backup_transport_is_selected_once_on_and_kept_selected() {
        runs.start()
        later.clear()
        assertTrue(runs.handle(setting(true)))
        assertTrue(runs.enabled)
        assertEquals(1, manager.selections)
        assertEquals(BackupRuns.RETRY_MILLIS, later.single().first)
        manager.transportReady = true
        later.removeAt(0).second()
        assertEquals(2, manager.selections)
        assertTrue(manager.backingUp)
        // Checked again later, in case another app selected its own transport meanwhile.
        assertEquals(BackupRuns.CHECK_MILLIS, later.single().first)
        assertTrue(log.lines.any { it.contains("backed up to the host") })
    }

    @Test
    fun fr36_turning_backup_off_stops_android_backing_up_at_once() {
        manager.transportReady = true
        runs.start()
        runs.handle(setting(true))
        assertTrue(manager.backingUp)
        val turnedOffBefore = manager.turnedOff
        runs.handle(setting(false))
        assertFalse(manager.backingUp)
        assertEquals(turnedOffBefore + 1, manager.turnedOff)
        // The checks of the earlier setting end: only the new chain goes on.
        val selections = manager.selections
        later.toList().forEach { it.second() }
        assertEquals(selections, manager.selections)
        assertFalse(manager.backingUp)
    }

    @Test
    fun fr36_backup_run_is_answered_once_the_backup_manager_finishes() {
        runs.handle(setting(true))
        sent.clear()
        assertTrue(runs.handle(run(7)))
        assertTrue("no answer before Android finishes", sent.isEmpty())
        // A second request while the first runs is refused at once.
        runs.handle(run(8))
        assertEquals(8L, sent.single().requestId)
        assertEquals(BackupStatus.BACKUP_STATUS_ERROR, sent.single().status)
        manager.backups.single()(BackupOutcome(succeeded = 5, failed = 1, error = null))
        runPosted()
        val result = sent.last()
        assertEquals(7L, result.requestId)
        assertEquals(BackupStatus.BACKUP_STATUS_OK, result.status)
        assertEquals(5, result.succeeded)
        assertEquals(1, result.failed)
        // Free again.
        runs.handle(run(9))
        assertEquals(2, manager.backups.size)
    }

    @Test
    fun fr36_backup_restore_asks_android_for_the_apps_without_the_package_manager() {
        val restore = Envelope.newBuilder().setBackupRestore(
            BackupRestore.newBuilder().setRequestId(3).setToken(0x5eed)
                .addAllPackages(listOf("@pm@", "org.example.notes", "org.example.game")),
        ).build()
        runs.handle(setting(true))
        assertTrue(runs.handle(restore))
        val (token, packages, done) = manager.restores.single()
        assertEquals(0x5eedL, token)
        assertEquals(listOf("org.example.notes", "org.example.game"), packages)
        done("Android's backup manager ended the restore with status 1")
        runPosted()
        val result = sent.single()
        assertEquals(3L, result.requestId)
        assertEquals(BackupStatus.BACKUP_STATUS_ERROR, result.status)
        assertEquals(0, result.succeeded)
        assertEquals(2, result.failed)
        assertTrue(result.message.contains("status 1"))
    }

    @Test
    fun fr36_backup_restore_of_nothing_is_answered_at_once() {
        val restore = Envelope.newBuilder().setBackupRestore(
            BackupRestore.newBuilder().setRequestId(4).setToken(1).addPackages("@pm@"),
        ).build()
        runs.handle(setting(true))
        runs.handle(restore)
        assertTrue(manager.restores.isEmpty())
        assertEquals(BackupStatus.BACKUP_STATUS_OK, sent.single().status)
    }

    @Test
    fun fr36_backup_runs_ignore_other_frames() {
        assertFalse(runs.handle(Envelope.getDefaultInstance()))
    }
}

class HostCallsTest {
    private val log = RecordingLog()

    @Test
    fun fr36_a_host_call_gets_the_answer_with_its_number() {
        val frames = ArrayBlockingQueue<Envelope>(4)
        val calls = HostCalls({ frame -> frames.offer(Envelope.parseFrom(frame)) }, log)
        val id = calls.nextId()
        val answer = ArrayBlockingQueue<Envelope?>(1)
        val caller = Thread {
            val request = Envelope.newBuilder().setBackupRun(BackupRun.newBuilder().setRequestId(id)).build()
            answer.offer(calls.call(id, request, 5000) ?: Envelope.getDefaultInstance())
        }.apply { start() }
        frames.poll(5, TimeUnit.SECONDS)
        // Another request's answer does not end this one's wait.
        assertTrue(calls.handle(contents(id + 100)))
        assertTrue(calls.handle(contents(id)))
        caller.join(5000)
        assertEquals(id, answer.poll()!!.backupContents.requestId)
        assertEquals(0, calls.pending)
    }

    @Test
    fun fr36_a_host_call_ends_when_the_host_connects_again_or_does_not_answer() {
        val calls = HostCalls({ true }, log)
        val ended = ArrayBlockingQueue<String>(1)
        val caller = Thread {
            ended.offer(if (calls.call(1, Envelope.getDefaultInstance(), 30_000) == null) "ended" else "answered")
        }.apply { start() }
        while (calls.pending == 0) Thread.sleep(5)
        calls.connected()
        caller.join(5000)
        assertEquals("ended", ended.poll())
        assertNull(calls.call(2, Envelope.getDefaultInstance(), 10))
        assertNull("a request that cannot be sent is not waited for", HostCalls({ false }, log).call(3, Envelope.getDefaultInstance(), 30_000))
    }

    private fun contents(id: Long) = Envelope.newBuilder()
        .setBackupContents(io.github.thisisthepy.dsa.control.v1.BackupContents.newBuilder().setRequestId(id))
        .build()
}

package io.github.thisisthepy.dsa.agent.install

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLauncherApps
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.tasks.LauncherApp
import io.github.thisisthepy.dsa.control.v1.ApkChunk
import io.github.thisisthepy.dsa.control.v1.CancelInstall
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.InstallApk
import io.github.thisisthepy.dsa.control.v1.InstallStatus
import java.io.ByteArrayOutputStream
import java.io.IOException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's package installer: keeps what each session was given, and answers a commit when the test says. */
class FakeInstaller : PackageInstallerPort {
    inner class Session(val name: String, val size: Long) : InstallSession {
        val written = ByteArrayOutputStream()
        var abandoned = false
        var onResult: ((InstallerResult) -> Unit)? = null

        override fun write(data: ByteArray) {
            if (failWrites) throw IOException("No space left on device")
            written.write(data)
        }

        override fun commit(onResult: (InstallerResult) -> Unit) {
            this.onResult = onResult
        }

        override fun abandon() {
            abandoned = true
        }

        /** The installer decides, on a thread of its own as the framework's broadcast does. */
        fun answer(result: InstallerResult) {
            val callback = onResult ?: error("$name was not committed")
            val thread = Thread { callback(result) }
            thread.start()
            thread.join()
        }
    }

    val sessions = mutableListOf<Session>()
    var refuseOpen: Exception? = null
    var failWrites = false

    override fun open(name: String, size: Long): InstallSession {
        refuseOpen?.let { throw it }
        return Session(name, size).also { sessions += it }
    }
}

class ApkInstallerTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val installer = FakeInstaller()
    private val launcherApps = FakeLauncherApps()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            launcherApps = launcherApps,
            packageInstaller = installer,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun deliver(envelope: Envelope.Builder) {
        daemon.deliver(envelope.build().toByteArray())
        scheduler.runDue()
    }

    private fun install(id: Int, name: String, size: Long) =
        deliver(Envelope.newBuilder().setInstallApk(InstallApk.newBuilder().setTransferId(id).setName(name).setSize(size)))

    private fun chunk(id: Int, offset: Long, data: ByteArray) =
        deliver(Envelope.newBuilder().setApkChunk(ApkChunk.newBuilder().setTransferId(id).setOffset(offset).setData(ByteString.copyFrom(data))))

    private fun progress() = daemon.sentMessages().filter { it.hasInstallProgress() }.map { it.installProgress }

    private fun results() = daemon.sentMessages().filter { it.hasInstallResult() }.map { it.installResult }

    private val apk = ByteArray(300) { (it % 251).toByte() }

    /** Sends [apk] in three pieces as install [id]. */
    private fun sendApk(id: Int = 7, name: String = "notes.apk") {
        install(id, name, apk.size.toLong())
        chunk(id, 0, apk.copyOfRange(0, 100))
        chunk(id, 100, apk.copyOfRange(100, 200))
        chunk(id, 200, apk.copyOfRange(200, 300))
    }

    private fun answer(result: InstallerResult) {
        installer.sessions.last().answer(result)
        scheduler.runDue()
    }

    @Test
    fun fr11_an_apk_is_written_into_an_install_session_and_committed() {
        started()
        sendApk()
        val session = installer.sessions.single()
        assertEquals("notes.apk", session.name)
        assertEquals(300L, session.size)
        assertArrayEquals(apk, session.written.toByteArray())
        // The host hears after each piece how far it got.
        assertEquals(listOf(100L, 200L, 300L), progress().map { it.received })
        assertTrue(progress().all { it.transferId == 7 })
        assertTrue("committed once whole", session.onResult != null)
        assertTrue("no result before the installer decides", results().isEmpty())

        answer(InstallerResult(PackageInstallerStatus.SUCCESS, "INSTALL_SUCCEEDED", "com.example.notes"))
        val result = results().single()
        assertEquals(7, result.transferId)
        assertEquals(InstallStatus.INSTALL_STATUS_SUCCESS, result.status)
        assertEquals("com.example.notes", result.packageName)
    }

    @Test
    fun fr11_an_installed_app_is_reported_to_the_host_for_the_start_menu() {
        started()
        sendApk()
        // The package manager's broadcast has not reached the launcher apps listener yet; the
        // install itself makes the agent read the apps again.
        launcherApps.installed = listOf(LauncherApp("com.example.notes", "Notes", ByteArray(0)))
        answer(InstallerResult(PackageInstallerStatus.SUCCESS, null, "com.example.notes"))
        val listed = daemon.sentMessages().filter { it.hasInstalledApps() }.last().installedApps
        assertEquals(listOf("com.example.notes"), listed.packageNamesList)
    }

    @Test
    fun fr11_each_installer_failure_maps_to_a_stable_reason() {
        val cases = mapOf(
            InstallerResult(PackageInstallerStatus.FAILURE_INCOMPATIBLE, "INSTALL_FAILED_NO_MATCHING_ABIS", null) to InstallStatus.INSTALL_STATUS_INCOMPATIBLE,
            InstallerResult(PackageInstallerStatus.FAILURE_CONFLICT, "INSTALL_FAILED_UPDATE_INCOMPATIBLE", "a.b", -7) to InstallStatus.INSTALL_STATUS_CONFLICTING_SIGNATURE,
            InstallerResult(PackageInstallerStatus.FAILURE_INVALID, "INSTALL_PARSE_FAILED_NOT_APK", null) to InstallStatus.INSTALL_STATUS_INVALID_APK,
            InstallerResult(PackageInstallerStatus.FAILURE_STORAGE, null, null) to InstallStatus.INSTALL_STATUS_INSUFFICIENT_STORAGE,
            InstallerResult(PackageInstallerStatus.FAILURE_CONFLICT, "Downgrade detected", "a.b", PackageInstallerStatus.LEGACY_VERSION_DOWNGRADE) to InstallStatus.INSTALL_STATUS_DOWNGRADE,
            InstallerResult(PackageInstallerStatus.FAILURE_INVALID, "INSTALL_FAILED_VERSION_DOWNGRADE: older", "a.b") to InstallStatus.INSTALL_STATUS_DOWNGRADE,
            InstallerResult(PackageInstallerStatus.FAILURE_BLOCKED, null, null) to InstallStatus.INSTALL_STATUS_BLOCKED,
            InstallerResult(PackageInstallerStatus.FAILURE_ABORTED, null, null) to InstallStatus.INSTALL_STATUS_OTHER,
            InstallerResult(PackageInstallerStatus.FAILURE, "INSTALL_FAILED_INTERNAL_ERROR", null) to InstallStatus.INSTALL_STATUS_OTHER,
            InstallerResult(42, null, null) to InstallStatus.INSTALL_STATUS_OTHER,
        )
        for ((result, reason) in cases) {
            assertEquals("$result", reason, PackageInstallerStatus.reason(result))
        }
    }

    @Test
    fun fr11_a_refusal_carries_the_installers_own_words() {
        started()
        sendApk()
        answer(InstallerResult(PackageInstallerStatus.FAILURE_CONFLICT, "INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match", "com.example.notes"))
        val result = results().single()
        assertEquals(InstallStatus.INSTALL_STATUS_CONFLICTING_SIGNATURE, result.status)
        assertEquals("INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match", result.message)
        assertEquals("com.example.notes", result.packageName)
    }

    @Test
    fun fr11_a_piece_out_of_order_ends_the_install_and_abandons_its_session() {
        started()
        install(7, "notes.apk", 300)
        chunk(7, 0, apk.copyOfRange(0, 100))
        chunk(7, 150, apk.copyOfRange(150, 250))
        val session = installer.sessions.single()
        assertTrue(session.abandoned)
        val result = results().single()
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, result.status)
        assertTrue(result.message, result.message.contains("out of order"))
        // What comes after is dropped and logged, and nothing crashes.
        chunk(7, 100, apk.copyOfRange(100, 200))
        assertEquals(1, results().size)
        assertTrue(log.lines.any { it.contains("not under way") })
    }

    @Test
    fun fr11_more_than_announced_ends_the_install() {
        started()
        install(7, "notes.apk", 50)
        chunk(7, 0, apk.copyOfRange(0, 100))
        assertTrue(installer.sessions.single().abandoned)
        assertTrue(results().single().message.contains("more than the 50 bytes"))
    }

    @Test
    fun fr11_a_full_disk_is_reported_as_insufficient_storage() {
        started()
        installer.failWrites = true
        install(7, "big.apk", 300)
        chunk(7, 0, apk.copyOfRange(0, 100))
        assertEquals(InstallStatus.INSTALL_STATUS_INSUFFICIENT_STORAGE, results().single().status)
        assertTrue(installer.sessions.single().abandoned)
    }

    @Test
    fun fr11_a_session_that_cannot_be_opened_is_reported() {
        started()
        installer.refuseOpen = SecurityException("not allowed to install")
        install(7, "notes.apk", 300)
        val result = results().single()
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, result.status)
        assertTrue(result.message, result.message.contains("not allowed to install"))
    }

    @Test
    fun fr11_a_cancelled_install_is_abandoned_and_answered() {
        started()
        install(7, "notes.apk", 300)
        chunk(7, 0, apk.copyOfRange(0, 100))
        deliver(Envelope.newBuilder().setCancelInstall(CancelInstall.newBuilder().setTransferId(7)))
        assertTrue(installer.sessions.single().abandoned)
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, results().single().status)
    }

    @Test
    fun fr11_an_install_of_a_lost_connection_is_abandoned_when_the_agent_connects_again() {
        started()
        install(7, "notes.apk", 300)
        chunk(7, 0, apk.copyOfRange(0, 100))
        daemon.die()
        scheduler.runDue()
        locator.up = FakeEndpoint()
        scheduler.advance(1000)
        assertTrue(installer.sessions.single().abandoned)
    }

    @Test
    fun fr11_without_an_installer_the_host_is_told_why() {
        locator.up = daemon
        Agent(AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log).start()
        scheduler.runDue()
        install(7, "notes.apk", 300)
        val result = results().single()
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, result.status)
        assertTrue(result.message, result.message.contains("installs no APKs"))
    }

    @Test
    fun fr11_an_apk_of_no_bytes_is_refused_without_a_session() {
        started()
        install(7, "empty.apk", 0)
        assertTrue(installer.sessions.isEmpty())
        assertEquals(InstallStatus.INSTALL_STATUS_INVALID_APK, results().single().status)
    }
}

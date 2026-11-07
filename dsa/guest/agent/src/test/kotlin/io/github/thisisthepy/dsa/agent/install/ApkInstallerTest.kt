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
import io.github.thisisthepy.dsa.control.v1.InstallPart
import io.github.thisisthepy.dsa.control.v1.InstallStatus
import java.io.ByteArrayOutputStream
import java.io.IOException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's package installer: keeps what each session was given, and answers a commit when the test says. */
class FakeInstaller : PackageInstallerPort {
    inner class Session(val name: String, val size: Long, val abi: String = "") : InstallSession {
        val written = ByteArrayOutputStream()
        var abandoned = false
        var onResult: ((InstallerResult) -> Unit)? = null

        /** The APKs of a session of several, by name, in the order they were begun. */
        val apks = linkedMapOf<String, ByteArrayOutputStream>()
        private var current: ByteArrayOutputStream? = null

        override fun startApk(name: String, size: Long) {
            current = ByteArrayOutputStream().also { apks[name] = it }
        }

        override fun write(data: ByteArray) {
            if (failWrites) throw IOException("No space left on device")
            (current ?: written).write(data)
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

    override fun open(name: String, size: Long, abi: String): InstallSession {
        refuseOpen?.let { throw it }
        return Session(name, size, abi).also { sessions += it }
    }

    override fun openApks(name: String, size: Long, abi: String): InstallSession {
        refuseOpen?.let { throw it }
        return Session(name, size, abi).also { sessions += it }
    }
}

/** Where the agent keeps expansion files: keeps what each was given, and what became of it. */
class FakeExpansionFiles : ExpansionFiles {
    val staged = linkedMapOf<String, ByteArrayOutputStream>()
    val placed = mutableListOf<Pair<String, List<String>>>()
    val discarded = mutableListOf<String>()
    var refusePlace: Exception? = null

    override fun stage(name: String): java.io.OutputStream = ByteArrayOutputStream().also { staged[name] = it }

    override fun place(packageName: String, names: List<String>) {
        refusePlace?.let { throw it }
        placed += packageName to names
    }

    override fun discard(names: List<String>) {
        discarded += names
    }
}

class ApkInstallerTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val installer = FakeInstaller()
    private val expansions = FakeExpansionFiles()
    private val launcherApps = FakeLauncherApps()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            launcherApps = launcherApps,
            packageInstaller = installer,
            expansionFiles = expansions,
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

    private val base = ByteArray(5) { (it + 1).toByte() }
    private val abiSplit = ByteArray(3) { (it + 20).toByte() }
    private val obb = ByteArray(4) { (it + 40).toByte() }

    private fun part(name: String, size: Int, obb: Boolean = false) =
        InstallPart.newBuilder().setName(name).setSize(size.toLong()).setObb(obb)

    /** Sends the notes app as a base, an ABI split and an expansion file, in two pieces that each cross a file's end. */
    private fun sendSplitApp(id: Int = 9) {
        deliver(
            Envelope.newBuilder().setInstallApk(
                InstallApk.newBuilder().setTransferId(id).setName("notes.xapk").setSize(12).setAbi("x86_64")
                    .addParts(part("base.apk", base.size))
                    .addParts(part("split_config.x86_64.apk", abiSplit.size))
                    .addParts(part("main.42.com.example.notes.obb", obb.size, obb = true)),
            ),
        )
        val content = base + abiSplit + obb
        chunk(id, 0, content.copyOfRange(0, 7))
        chunk(id, 7, content.copyOfRange(7, 12))
    }

    @Test
    fun fr11_the_apks_of_a_split_app_go_into_one_session_and_its_expansion_file_to_android_obb() {
        started()
        sendSplitApp()
        val session = installer.sessions.single()
        assertEquals("x86_64", session.abi)
        // The session is as large as the APKs, without the expansion file.
        assertEquals(8L, session.size)
        assertEquals(listOf("base.apk", "split_config.x86_64.apk"), session.apks.keys.toList())
        assertArrayEquals(base, session.apks.getValue("base.apk").toByteArray())
        assertArrayEquals(abiSplit, session.apks.getValue("split_config.x86_64.apk").toByteArray())
        assertArrayEquals(obb, expansions.staged.getValue("main.42.com.example.notes.obb").toByteArray())
        assertEquals(listOf(7L, 12L), progress().map { it.received })
        assertTrue("committed once whole", session.onResult != null)
        assertTrue("not in place before the app is installed", expansions.placed.isEmpty())

        answer(InstallerResult(PackageInstallerStatus.SUCCESS, "INSTALL_SUCCEEDED", "com.example.notes"))
        assertEquals(listOf("com.example.notes" to listOf("main.42.com.example.notes.obb")), expansions.placed)
        val result = results().single()
        assertEquals(InstallStatus.INSTALL_STATUS_SUCCESS, result.status)
        assertEquals("com.example.notes", result.packageName)
    }

    @Test
    fun fr11_the_expansion_files_of_an_app_android_refused_are_dropped() {
        started()
        sendSplitApp()
        answer(InstallerResult(PackageInstallerStatus.FAILURE_INCOMPATIBLE, "INSTALL_FAILED_NO_MATCHING_ABIS", null))
        assertTrue(expansions.placed.isEmpty())
        assertEquals(listOf("main.42.com.example.notes.obb"), expansions.discarded)
        assertEquals(InstallStatus.INSTALL_STATUS_INCOMPATIBLE, results().single().status)
    }

    @Test
    fun fr11_expansion_files_that_cannot_be_put_in_place_say_why() {
        started()
        expansions.refusePlace = IOException("Permission denied")
        sendSplitApp()
        answer(InstallerResult(PackageInstallerStatus.SUCCESS, null, "com.example.notes"))
        val result = results().single()
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, result.status)
        assertEquals("com.example.notes", result.packageName)
        assertTrue(result.message, result.message.contains("Android/obb/com.example.notes"))
        assertTrue(result.message, result.message.contains("Permission denied"))
        assertEquals(listOf("main.42.com.example.notes.obb"), expansions.discarded)
    }

    @Test
    fun fr11_a_cancelled_split_install_drops_its_session_and_expansion_files() {
        started()
        deliver(
            Envelope.newBuilder().setInstallApk(
                InstallApk.newBuilder().setTransferId(4).setName("notes.xapk").setSize(9)
                    .addParts(part("base.apk", 5))
                    .addParts(part("main.1.com.example.notes.obb", 4, obb = true)),
            ),
        )
        chunk(4, 0, ByteArray(7))
        deliver(Envelope.newBuilder().setCancelInstall(CancelInstall.newBuilder().setTransferId(4)))
        assertTrue(installer.sessions.single().abandoned)
        assertEquals(listOf("main.1.com.example.notes.obb"), expansions.discarded)
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, results().single().status)
    }

    @Test
    fun fr11_parts_android_cannot_take_are_refused_before_a_session_opens() {
        started()
        val bad = listOf(
            listOf(part("main.1.a.obb", 3, obb = true)) to 3L,
            listOf(part("main.1.a.obb", 3, obb = true), part("base.apk", 3)) to 6L,
            listOf(part("base.apk", 3), part("../base.apk", 3)) to 6L,
            listOf(part("base.apk", 3), part("BASE.APK", 3)) to 6L,
            listOf(part("base.apk", 3), part("main.1.a.apk", 3, obb = true)) to 6L,
            listOf(part("base.apk", 3)) to 4L,
        )
        for ((index, case) in bad.withIndex()) {
            val (parts, size) = case
            val install = InstallApk.newBuilder().setTransferId(20 + index).setName("a.xapk").setSize(size)
            parts.forEach { install.addParts(it) }
            deliver(Envelope.newBuilder().setInstallApk(install))
        }
        assertTrue(installer.sessions.isEmpty())
        assertEquals(bad.size, results().size)
        assertTrue(results().all { it.status == InstallStatus.INSTALL_STATUS_INVALID_APK })
    }

    @Test
    fun fr11_an_expansion_file_goes_to_the_obb_folder_of_its_package() {
        val folder = InstallFileNames.obbFolder(java.io.File("/storage/emulated/0"), "com.example.notes")
        assertEquals("/storage/emulated/0/Android/obb/com.example.notes", folder.invariantSeparatorsPath)
        assertTrue(InstallFileNames.valid("main.42.com.example.notes.obb", ".obb"))
        assertTrue(InstallFileNames.valid("patch.42.com.example.notes.OBB", ".obb"))
        for (name in listOf(".obb", "a/b.obb", "a\\b.obb", "main.obb.apk", "a\nb.obb")) {
            assertTrue(name, !InstallFileNames.valid(name, ".obb"))
        }
    }

    @Test
    fun fr25_the_abi_the_host_chose_is_the_install_sessions_abi_override() {
        started()
        deliver(Envelope.newBuilder().setInstallApk(InstallApk.newBuilder().setTransferId(3).setName("game.apk").setSize(10).setAbi("arm64-v8a")))
        deliver(Envelope.newBuilder().setInstallApk(InstallApk.newBuilder().setTransferId(4).setName("notes.apk").setSize(10)))
        assertEquals(listOf("arm64-v8a", ""), installer.sessions.map { it.abi })
    }

    @Test
    fun fr25_an_abi_the_guest_cannot_run_is_refused_before_a_session_opens() {
        started()
        deliver(Envelope.newBuilder().setInstallApk(InstallApk.newBuilder().setTransferId(5).setName("old.apk").setSize(10).setAbi("armeabi-v7a")))
        assertTrue(installer.sessions.isEmpty())
        val result = results().single()
        assertEquals(5, result.transferId)
        assertEquals(InstallStatus.INSTALL_STATUS_OTHER, result.status)
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

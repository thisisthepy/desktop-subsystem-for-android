package io.github.thisisthepy.dsa.agent.install

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
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ReinstallApp
import io.github.thisisthepy.dsa.control.v1.ReinstallResult
import io.github.thisisthepy.dsa.control.v1.ReinstallStatus
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppReinstallerTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()

    /** The guest's installer: the apps it holds, and what it answers. */
    private class FakeReinstaller : PackageReinstallerPort {
        val apps = mutableMapOf(
            // x86_64 code that leaves out a library its ARM code has.
            "com.example.game" to InstalledApks(
                AppKind.USER,
                listOf(File("/data/app/game/base.apk"), File("/data/app/game/split_config.arm64_v8a.apk")),
                mapOf("arm64-v8a" to listOf("libmain.so", "libunity.so"), "x86_64" to listOf("libmain.so")),
            ),
            "com.example.notes" to InstalledApks(AppKind.USER, listOf(File("/data/app/notes/base.apk")), emptyMap()),
            "com.android.settings" to InstalledApks(
                AppKind.SYSTEM,
                listOf(File("/system/app/Settings.apk")),
                mapOf("arm64-v8a" to listOf("libsettings.so")),
            ),
        )
        val abis = mutableMapOf("com.example.game" to "x86_64")
        val steps = mutableListOf<String>()
        var replaceAnswer = InstallerResult(PackageInstallerStatus.SUCCESS, "INSTALL_SUCCEEDED", "com.example.game")
        var dataKept = true

        override fun apks(packageName: String): InstalledApks? = apps[packageName]

        override fun install(packageName: String, files: List<File>, abi: String, onResult: (InstallerResult) -> Unit) {
            steps += "install $packageName $abi ${files.joinToString(",") { it.path.replace('\\', '/') }}"
            val answer = if (packageName in apps) replaceAnswer else InstallerResult(PackageInstallerStatus.SUCCESS, "INSTALL_SUCCEEDED", packageName)
            if (answer.status == PackageInstallerStatus.SUCCESS) abis[packageName] = abi
            onResult(answer)
        }

        override fun uninstall(packageName: String, onResult: (InstallerResult) -> Unit) {
            steps += "uninstall $packageName"
            apps.remove(packageName)
            dataKept = false
            onResult(InstallerResult(PackageInstallerStatus.SUCCESS, "Success", packageName))
        }

        override fun keepCopies(packageName: String, files: List<File>): List<File> {
            steps += "copy $packageName"
            return files.map { File("/cache/reinstall/$packageName", it.name) }
        }

        override fun dropCopies(files: List<File>) {
            steps += "drop ${files.size}"
        }

        override fun primaryAbi(packageName: String): String = abis[packageName].orEmpty()
    }

    private val reinstaller = FakeReinstaller()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            launcherApps = FakeLauncherApps(),
            packageReinstaller = reinstaller,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun reinstall(id: Int, name: String, abi: String, eraseData: Boolean = false) {
        daemon.deliver(
            Envelope.newBuilder().setReinstallApp(
                ReinstallApp.newBuilder().setRequestId(id).setPackageName(name).setAbi(abi).setEraseData(eraseData),
            ).build().toByteArray(),
        )
        scheduler.runDue()
    }

    private fun results(): List<ReinstallResult> = daemon.sentMessages().filter { it.hasReinstallResult() }.map { it.reinstallResult }

    @Test
    fun fr25_switching_an_app_to_arm_replaces_it_from_its_apks_and_keeps_its_data() {
        started()

        reinstall(7, "com.example.game", "arm64-v8a")

        assertEquals(
            listOf("install com.example.game arm64-v8a /data/app/game/base.apk,/data/app/game/split_config.arm64_v8a.apk"),
            reinstaller.steps,
        )
        assertTrue(reinstaller.dataKept)
        val result = results().single()
        assertEquals(7, result.requestId)
        assertEquals(ReinstallStatus.REINSTALL_STATUS_SUCCESS, result.status)
        assertEquals("arm64-v8a", result.primaryAbi)
    }

    @Test
    fun fr25_a_replace_android_refuses_asks_before_the_data_goes_and_erasing_it_reinstalls_from_copies() {
        started()
        reinstaller.replaceAnswer = InstallerResult(
            PackageInstallerStatus.FAILURE_INCOMPATIBLE,
            "INSTALL_FAILED_UPDATE_INCOMPATIBLE: the existing package cannot be replaced",
            "com.example.game",
            legacyStatus = AppReinstaller.LEGACY_UPDATE_INCOMPATIBLE,
        )

        reinstall(1, "com.example.game", "arm64-v8a")
        assertEquals(ReinstallStatus.REINSTALL_STATUS_DATA_WOULD_BE_ERASED, results().last().status)
        assertTrue("nothing was uninstalled", reinstaller.dataKept)
        assertEquals("x86_64", reinstaller.primaryAbi("com.example.game"))

        reinstaller.steps.clear()
        reinstall(2, "com.example.game", "arm64-v8a", eraseData = true)
        assertEquals(
            listOf(
                "copy com.example.game",
                "uninstall com.example.game",
                "install com.example.game arm64-v8a /cache/reinstall/com.example.game/base.apk," +
                    "/cache/reinstall/com.example.game/split_config.arm64_v8a.apk",
                "drop 2",
            ),
            reinstaller.steps,
        )
        assertFalse(reinstaller.dataKept)
        assertEquals(ReinstallStatus.REINSTALL_STATUS_SUCCESS, results().last().status)
    }

    @Test
    fun fr25_an_app_without_code_for_the_abi_a_system_app_or_a_missing_one_is_refused_with_its_reason() {
        started()

        reinstall(1, "com.example.notes", "arm64-v8a")
        reinstall(2, "com.android.settings", "arm64-v8a")
        reinstall(3, "com.example.missing", "arm64-v8a")
        reinstall(4, AGENT, "arm64-v8a")
        reinstall(5, "com.example.game", "armeabi-v7a")

        assertTrue(reinstaller.steps.isEmpty())
        assertEquals(
            listOf(
                ReinstallStatus.REINSTALL_STATUS_NO_LIBRARIES,
                ReinstallStatus.REINSTALL_STATUS_SYSTEM_APP,
                ReinstallStatus.REINSTALL_STATUS_NOT_INSTALLED,
                ReinstallStatus.REINSTALL_STATUS_SYSTEM_APP,
                ReinstallStatus.REINSTALL_STATUS_OTHER,
            ),
            results().map { it.status },
        )
    }

    @Test
    fun fr25_an_installer_that_keeps_the_old_abi_is_reported_rather_than_taken_for_success() {
        started()
        val keeps = object : PackageReinstallerPort by reinstaller {
            override fun primaryAbi(packageName: String): String = "x86_64"
        }
        val sent = mutableListOf<ByteArray>()
        val direct = AppReinstaller(AGENT, keeps, { sent += it; true }, log, post = { it() })

        direct.handle(
            Envelope.newBuilder().setReinstallApp(
                ReinstallApp.newBuilder().setRequestId(9).setPackageName("com.example.game").setAbi("arm64-v8a"),
            ).build(),
        )

        val result = Envelope.parseFrom(sent.single()).reinstallResult
        assertEquals(ReinstallStatus.REINSTALL_STATUS_OTHER, result.status)
        assertEquals("x86_64", result.primaryAbi)
    }
}

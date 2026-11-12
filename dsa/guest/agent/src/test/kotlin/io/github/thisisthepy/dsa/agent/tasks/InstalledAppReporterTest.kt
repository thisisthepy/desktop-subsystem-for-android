package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class InstalledAppReporterTest {
    private val sent = mutableListOf<Envelope>()
    private var channelUp = true
    private val reporter = InstalledAppReporter(AGENT) { frame ->
        if (channelUp) sent += decode(frame)
        channelUp
    }

    private fun app(packageName: String, label: String = "Name of $packageName", icon: Byte = 1) =
        LauncherApp(packageName, label, byteArrayOf(icon, 2, 3))

    private fun described() = sent.filter { it.hasInstalledApp() }.map { it.installedApp.packageName }

    private fun lists() = sent.filter { it.hasInstalledApps() }.map { it.installedApps.packageNamesList }

    @Test
    fun fr7_every_app_is_described_then_listed_in_package_order() {
        reporter.update(listOf(app("org.example.maps"), app("org.example.clock"), app(AGENT)))

        assertEquals(
            listOf(Envelope.BodyCase.INSTALLED_APP, Envelope.BodyCase.INSTALLED_APP, Envelope.BodyCase.INSTALLED_APPS),
            sent.map { it.bodyCase },
        )
        assertEquals(listOf("org.example.clock", "org.example.maps"), described())
        assertEquals(listOf(listOf("org.example.clock", "org.example.maps")), lists())
        val clock = sent[0].installedApp
        assertEquals("Name of org.example.clock", clock.label)
        assertArrayEquals(byteArrayOf(1, 2, 3), clock.iconPng.toByteArray())
    }

    @Test
    fun fr7_nothing_is_sent_while_nothing_changes() {
        reporter.update(listOf(app("org.example.clock")))
        sent.clear()

        reporter.update(listOf(app("org.example.clock")))

        assertTrue(sent.isEmpty())
    }

    @Test
    fun fr7_an_installed_app_is_described_and_the_list_sent_again() {
        reporter.update(listOf(app("org.example.clock")))
        sent.clear()

        reporter.update(listOf(app("org.example.clock"), app("org.example.maps")))

        assertEquals(listOf("org.example.maps"), described())
        assertEquals(listOf(listOf("org.example.clock", "org.example.maps")), lists())
    }

    @Test
    fun fr7_a_removed_app_leaves_the_list() {
        reporter.update(listOf(app("org.example.clock"), app("org.example.maps")))
        sent.clear()

        reporter.update(listOf(app("org.example.clock")))

        assertEquals(emptyList<String>(), described())
        assertEquals(listOf(listOf("org.example.clock")), lists())
    }

    @Test
    fun fr7_an_updated_app_with_a_new_name_or_icon_is_described_again() {
        reporter.update(listOf(app("org.example.clock")))
        sent.clear()

        reporter.update(listOf(app("org.example.clock", icon = 9)))

        assertEquals(listOf("org.example.clock"), described())
        assertEquals(listOf(listOf("org.example.clock")), lists())
    }

    @Test
    fun fr7_a_new_connection_describes_every_app_again() {
        reporter.update(listOf(app("org.example.clock")))
        sent.clear()

        reporter.resend()
        reporter.update(listOf(app("org.example.clock")))

        assertEquals(listOf("org.example.clock"), described())
        assertEquals(listOf(listOf("org.example.clock")), lists())
    }

    @Test
    fun fr7_what_could_not_be_sent_is_sent_on_the_next_update() {
        channelUp = false
        reporter.update(listOf(app("org.example.clock")))
        channelUp = true

        reporter.update(listOf(app("org.example.clock")))

        assertEquals(listOf("org.example.clock"), described())
        assertEquals(listOf(listOf("org.example.clock")), lists())
    }

    private fun detailed(versionCode: Long = 3, dataBytes: Long = 4096) = LauncherApp(
        "org.example.notes", "Notes", byteArrayOf(1),
        versionName = "1.$versionCode", versionCode = versionCode,
        firstInstallTimeMs = 1_700_000_000_000, lastUpdateTimeMs = 1_710_000_000_000,
        systemApp = false, appBytes = 1_000_000, dataBytes = dataBytes, cacheBytes = 512,
    )

    @Test
    fun fr23_each_app_carries_its_version_dates_sizes_and_kind() {
        reporter.update(listOf(detailed(), app("org.example.clock")))

        val notes = sent.first { it.hasInstalledApp() && it.installedApp.packageName == "org.example.notes" }.installedApp
        assertEquals("1.3", notes.versionName)
        assertEquals(3L, notes.versionCode)
        assertEquals(1_700_000_000_000, notes.firstInstallTimeMs)
        assertEquals(1_710_000_000_000, notes.lastUpdateTimeMs)
        assertEquals(false, notes.systemApp)
        assertEquals(1_000_000L, notes.appBytes)
        assertEquals(4096L, notes.dataBytes)
        assertEquals(512L, notes.cacheBytes)
    }

    @Test
    fun fr35_apps_sharing_a_user_id_are_approved_as_one_group() {
        val one = LauncherApp("org.example.one", "One", byteArrayOf(1), sharedUserId = "org.example.shared")
        val two = LauncherApp("org.example.two", "Two", byteArrayOf(2), sharedUserId = "org.example.shared")
        reporter.update(listOf(one, two, app("org.example.alone")))

        val byPackage = sent.filter { it.hasInstalledApp() }.associate { it.installedApp.packageName to it.installedApp }
        assertEquals("org.example.shared", byPackage.getValue("org.example.one").sharedUserId)
        assertEquals("org.example.shared", byPackage.getValue("org.example.two").sharedUserId)
        assertEquals("", byPackage.getValue("org.example.alone").sharedUserId)

        // An app that joins a group is described again, so the host groups it at once.
        sent.clear()
        reporter.update(listOf(one, two, LauncherApp("org.example.alone", "Name of org.example.alone", byteArrayOf(1, 2, 3), sharedUserId = "org.example.shared")))
        assertEquals(listOf("org.example.alone"), described())
    }

    @Test
    fun fr23_a_new_version_or_size_describes_the_app_again() {
        reporter.update(listOf(detailed()))
        sent.clear()

        reporter.update(listOf(detailed(versionCode = 4)))
        reporter.update(listOf(detailed(versionCode = 4, dataBytes = 8192)))

        assertEquals(listOf("org.example.notes", "org.example.notes"), described())
        assertEquals(2, lists().size)
    }

    @Test
    fun fr23_a_forced_update_sends_the_list_even_when_nothing_changed() {
        reporter.update(listOf(detailed()))
        sent.clear()

        reporter.update(listOf(detailed()), force = true)

        assertEquals(emptyList<String>(), described())
        assertEquals(listOf(listOf("org.example.notes")), lists())
    }
}

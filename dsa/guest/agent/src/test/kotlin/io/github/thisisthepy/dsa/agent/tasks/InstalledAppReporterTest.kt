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
}

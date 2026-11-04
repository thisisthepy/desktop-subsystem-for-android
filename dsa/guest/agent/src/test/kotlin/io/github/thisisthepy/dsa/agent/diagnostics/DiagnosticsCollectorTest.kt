package io.github.thisisthepy.dsa.agent.diagnostics

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.CollectDiagnostics
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ScreenState
import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

private class FakeDiagnostics(
    val entries: List<DropboxEntry> = emptyList(),
    val dumps: Map<String, String> = emptyMap(),
    val failing: Set<String> = emptySet(),
) : DiagnosticsSource {
    val asked = mutableListOf<String>()

    override fun dropbox(): List<DropboxEntry> = entries

    override fun dump(service: String, args: List<String>): ByteArray? {
        asked += (listOf(service) + args).joinToString(" ")
        if (service in failing) throw SecurityException("no DUMP")
        return dumps[service]?.toByteArray()
    }
}

class DiagnosticsCollectorTest {
    private fun collect(source: DiagnosticsSource, requestId: Int = 7): List<Envelope> {
        val sent = mutableListOf<ByteArray>()
        val collector = DiagnosticsCollector(source, { sent += it }, RecordingLog(), run = { it.run() })
        val frame = Envelope.newBuilder()
            .setCollectDiagnostics(CollectDiagnostics.newBuilder().setRequestId(requestId))
            .build()
            .toByteArray()
        assertTrue(collector.handle(frame))
        return sent.map { Envelope.parseFrom(it) }
    }

    @Test
    fun fr21_the_agent_sends_the_services_state_and_the_dropbox_then_says_it_is_done() {
        val source = FakeDiagnostics(
            entries = listOf(
                DropboxEntry("SYSTEM_TOMBSTONE", 100, "signal 11".toByteArray()),
                DropboxEntry("data_app_anr", 200, "Input dispatching timed out".toByteArray()),
                DropboxEntry("SYSTEM_LAST_KMSG", 300, "Kernel panic".toByteArray()),
                DropboxEntry("SYSTEM_LAST_KMSG", 300, "again".toByteArray()),
            ),
            dumps = mapOf("activity" to "ACTIVITY MANAGER", "meminfo" to "Total PSS"),
            failing = setOf("cpuinfo"),
        )
        val sent = collect(source)
        assertTrue(source.asked.containsAll(listOf("activity activities", "window windows", "meminfo")))
        val files = sent.filter { it.bodyCase == Envelope.BodyCase.DIAGNOSTICS_FILE }.map { it.diagnosticsFile }
        assertTrue(files.all { it.requestId == 7 && it.last })
        assertEquals(
            listOf(
                "android/dumpsys.txt",
                "android/tombstones/SYSTEM_TOMBSTONE@100.txt",
                "android/anr/data_app_anr@200.txt",
                "android/dropbox/SYSTEM_LAST_KMSG@300.txt",
                "android/dropbox/SYSTEM_LAST_KMSG@300.txt.2",
            ),
            files.map { it.name },
        )
        val summary = files[0].data.toStringUtf8()
        assertTrue(summary, summary.contains("-------- dumpsys activity activities\nACTIVITY MANAGER"))
        val done = sent.last()
        assertEquals(Envelope.BodyCase.DIAGNOSTICS_DONE, done.bodyCase)
        assertEquals(7, done.diagnosticsDone.requestId)
        assertTrue(done.diagnosticsDone.error, done.diagnosticsDone.error.contains("dumpsys cpuinfo: no DUMP"))
        assertTrue(done.diagnosticsDone.error, done.diagnosticsDone.error.contains("dumpsys window windows: no such service"))
    }

    @Test
    fun fr21_a_large_file_goes_in_pieces_the_last_marked() {
        val big = ByteArray(DiagnosticsCollector.PIECE * 2 + 10) { 'x'.code.toByte() }
        val sent = collect(FakeDiagnostics(entries = listOf(DropboxEntry("big", 1, big))))
        val pieces = sent.filter { it.hasDiagnosticsFile() && it.diagnosticsFile.name.startsWith("android/dropbox/") }
            .map { it.diagnosticsFile }
        assertEquals(3, pieces.size)
        assertEquals(listOf(false, false, true), pieces.map { it.last })
        assertEquals(big.size, pieces.sumOf { it.data.size() })
    }

    @Test
    fun fr21_a_dropbox_tag_cannot_name_a_path_outside_the_archive() {
        val name = DiagnosticsCollector.nameOf(DropboxEntry("../../etc/passwd", 5, ByteArray(0)))
        assertFalse(name, name.contains(".."+ "/"))
        assertTrue(name, name.startsWith("android/dropbox/"))
        assertEquals(2, name.count { it == '/' })
    }

    @Test
    fun fr21_other_frames_are_not_the_collectors() {
        val collector = DiagnosticsCollector(DiagnosticsSource.NONE, { }, RecordingLog(), run = { it.run() })
        val other = Envelope.newBuilder().setScreenState(ScreenState.newBuilder().setOn(true)).build()
        assertFalse(collector.handle(other.toByteArray()))
        assertFalse(collector.handle(byteArrayOf(0x7f, 0x7f)))
    }

    @Test
    fun fr21_the_agent_may_read_the_dropbox_in_its_manifest_and_its_privileged_list() {
        fun names(path: String, tag: String): Set<String> {
            val document = DocumentBuilderFactory.newInstance()
                .apply { isNamespaceAware = true }
                .newDocumentBuilder()
                .parse(File(path))
            val nodes = document.getElementsByTagName(tag)
            return (0 until nodes.length).map { nodes.item(it) as Element }
                .map { it.getAttributeNS("http://schemas.android.com/apk/res/android", "name").ifEmpty { it.getAttribute("name") } }
                .toSet()
        }
        for (permission in listOf("android.permission.READ_LOGS", "android.permission.DUMP")) {
            assertTrue(permission in names("src/main/AndroidManifest.xml", "uses-permission"))
            assertTrue(permission in names("privapp-permissions-dsa-agent.xml", "permission"))
        }
    }
}

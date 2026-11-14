package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ShortcutReporterTest {
    private val sent = mutableListOf<Envelope>()
    private var channelUp = true
    private val reporter = ShortcutReporter(AGENT) { frame ->
        if (channelUp) sent += decode(frame)
        channelUp
    }
    private var shortcuts = mapOf<String, List<LauncherShortcut>>()

    private fun shortcut(id: String, rank: Int = 0, manifest: Boolean = false, dynamic: Boolean = true, pinned: Boolean = false) =
        LauncherShortcut(id, "Label $id", "Long $id", byteArrayOf(rank.toByte(), 2), rank, manifest, dynamic, pinned)

    private fun update(vararg packages: String) = reporter.update(packages.toList()) { shortcuts[it].orEmpty() }

    private fun reported() = sent.map { it.appShortcuts.packageName to it.appShortcuts.shortcutsList.map { s -> s.id } }

    @Test
    fun fr7_each_app_with_shortcuts_is_reported_with_their_names_and_icons() {
        shortcuts = mapOf(
            "org.example.chat" to listOf(shortcut("new-chat"), shortcut("ana", rank = 1)),
            AGENT to listOf(shortcut("own")),
        )

        update("org.example.clock", "org.example.chat", AGENT)

        assertEquals(listOf("org.example.chat" to listOf("new-chat", "ana")), reported())
        val first = sent[0].appShortcuts.getShortcuts(0)
        assertEquals("Label new-chat", first.shortLabel)
        assertEquals("Long new-chat", first.longLabel)
        assertTrue(first.dynamic)
        assertArrayEquals(byteArrayOf(0, 2), first.iconPng.toByteArray())
    }

    @Test
    fun fr7_static_and_dynamic_shortcuts_go_by_rank_before_the_pinned_ones() {
        shortcuts = mapOf(
            "org.example.chat" to listOf(
                shortcut("pinned", dynamic = false, pinned = true),
                shortcut("dynamic-1", rank = 1),
                shortcut("static", rank = 3, manifest = true, dynamic = false),
                shortcut("dynamic-0", rank = 0),
            ),
        )

        update("org.example.chat")

        assertEquals(listOf("org.example.chat" to listOf("static", "dynamic-0", "dynamic-1", "pinned")), reported())
    }

    @Test
    fun fr7_unchanged_shortcuts_are_not_sent_again_and_changed_ones_are() {
        shortcuts = mapOf("org.example.chat" to listOf(shortcut("new-chat")))
        update("org.example.chat")
        sent.clear()

        update("org.example.chat")
        assertTrue(sent.isEmpty())

        shortcuts = mapOf("org.example.chat" to listOf(shortcut("new-chat"), shortcut("ana", rank = 1)))
        reporter.changed(listOf("org.example.chat")) { shortcuts[it].orEmpty() }
        assertEquals(listOf("org.example.chat" to listOf("new-chat", "ana")), reported())
    }

    @Test
    fun fr7_an_app_whose_shortcuts_are_all_gone_is_reported_with_none_once() {
        shortcuts = mapOf("org.example.chat" to listOf(shortcut("new-chat")))
        update("org.example.chat")
        sent.clear()

        shortcuts = emptyMap()
        update("org.example.chat")
        update("org.example.chat")

        assertEquals(listOf("org.example.chat" to emptyList<String>()), reported())
    }

    @Test
    fun fr7_a_report_that_could_not_be_sent_is_sent_with_the_next_update() {
        shortcuts = mapOf("org.example.chat" to listOf(shortcut("new-chat")))
        channelUp = false
        update("org.example.chat")
        channelUp = true

        update("org.example.chat")

        assertEquals(listOf("org.example.chat" to listOf("new-chat")), reported())
    }

    @Test
    fun fr7_a_new_connection_hears_every_apps_shortcuts_again() {
        shortcuts = mapOf("org.example.chat" to listOf(shortcut("new-chat")))
        update("org.example.chat")
        sent.clear()

        reporter.resend()
        update("org.example.chat")

        assertEquals(listOf("org.example.chat" to listOf("new-chat")), reported())
    }

    @Test
    fun fr7_an_app_whose_shortcuts_cannot_be_read_keeps_what_the_host_has() {
        shortcuts = mapOf("org.example.chat" to listOf(shortcut("new-chat")))
        update("org.example.chat")
        sent.clear()

        reporter.update(listOf("org.example.chat")) { null }

        assertTrue(sent.isEmpty())
    }
}

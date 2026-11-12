package io.github.thisisthepy.dsa.agent.settings

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ScreenState
import io.github.thisisthepy.dsa.control.v1.SystemSettings
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SystemSettingsSyncTest {
    private val calls = mutableListOf<String>()
    private var accentPalette: Map<String, Int> = emptyMap()
    private val port = object : SystemSettingsPort {
        override fun setTimeZone(id: String) {
            calls += "time zone $id"
        }

        override fun setLocales(tags: List<String>) {
            calls += "locales $tags"
        }

        override fun setTime24Hour(on: Boolean) {
            calls += "24-hour $on"
        }

        override fun setDarkMode(on: Boolean) {
            calls += "dark $on"
        }

        override fun setAccentColor(argb: Int, palette: Map<String, Int>) {
            calls += "accent %08X".format(argb)
            accentPalette = palette
        }

        override fun fontScale(): Float = fontScale

        override fun setFontScale(scale: Float) {
            calls += String.format(java.util.Locale.ROOT, "font scale %.3f", scale)
            fontScale = scale
        }
    }

    /** The guest's font scale, as Android starts with it. */
    private var fontScale = 1f
    private val log = RecordingLog()
    private val sync = SystemSettingsSync(port, log)

    private fun seoul(dark: Boolean = true, accent: Int = 0xFF0078D4.toInt()): ByteArray =
        Envelope.newBuilder()
            .setSystemSettings(
                SystemSettings.newBuilder()
                    .setTimeZone("Asia/Seoul")
                    .addLocales("ko-KR")
                    .addLocales("en-US")
                    .setRegionalFormat("ko-KR")
                    .setTime24Hour(false)
                    .setFirstDayOfWeek(7)
                    .setDarkMode(dark)
                    .setAccentColor(accent),
            )
            .build()
            .toByteArray()

    @Test
    fun fr30_the_hosts_settings_become_the_guests() {
        assertTrue(sync.handle(seoul()))
        assertEquals(
            listOf(
                "time zone Asia/Seoul",
                "locales [ko-KR-u-fw-sun-hc-h12, en-US-u-fw-sun-hc-h12-rg-krzzzz]",
                "24-hour false",
                "dark true",
                "accent FF0078D4",
            ),
            calls,
        )
        assertEquals(5 * 13, accentPalette.size)
    }

    @Test
    fun fr30_only_what_changed_is_applied_again() {
        sync.handle(seoul())
        calls.clear()
        sync.handle(seoul(dark = false))
        assertEquals(listOf("dark false"), calls)
        calls.clear()
        sync.handle(seoul(dark = false, accent = 0xFFE81123.toInt()))
        assertEquals(listOf("accent FFE81123"), calls)
        // A new connection applies everything again.
        calls.clear()
        sync.connected()
        sync.handle(seoul(dark = false, accent = 0xFFE81123.toInt()))
        assertEquals(5, calls.size)
    }

    @Test
    fun fr30_a_setting_the_guest_refuses_does_not_keep_the_others_from_applying() {
        val refusing = object : SystemSettingsPort by SystemSettingsPort.NONE {
            override fun setTimeZone(id: String) = throw SecurityException("no")

            override fun setDarkMode(on: Boolean) {
                calls += "dark $on"
            }
        }
        val sync = SystemSettingsSync(refusing, log)
        sync.handle(seoul())
        assertEquals(listOf("dark true"), calls)
        assertTrue(log.lines.any { it.startsWith("warn: could not follow the host's time zone") })
    }

    /** The host's settings with a font scale for a Windows text size of [percent]. */
    private fun textSize(percent: Int): ByteArray {
        val envelope = Envelope.parseFrom(seoul())
        val settings = envelope.systemSettings.toBuilder().setFontScale(percent / 100f)
        return envelope.toBuilder().setSystemSettings(settings).build().toByteArray()
    }

    @Test
    fun fr4_text_follows_the_windows_text_size() {
        // Android starts: the tasks are read, and none is open yet. A text size of 100% is a
        // font scale of 1, which Android starts at, so nothing is set.
        sync.tasksRunning(emptyList())
        sync.handle(textSize(100))
        assertFalse(calls.any { it.startsWith("font scale") })
        assertEquals(1f, fontScale, 0.001f)

        // An app that handles a change of font scale itself is open: the new size applies at
        // once, and Android redraws it without a restart.
        calls.clear()
        sync.tasksRunning(emptyList())
        sync.handle(textSize(150))
        assertEquals(listOf("font scale 1.500"), calls)
        assertFalse(sync.holdsFontScale)

        // An app that would be restarted for it is open: the size waits, and nothing restarts.
        calls.clear()
        sync.tasksRunning(listOf("com.example.notes"))
        sync.handle(textSize(125))
        assertTrue(calls.isEmpty())
        assertTrue(sync.holdsFontScale)
        assertEquals(1.5f, fontScale, 0.001f)
        assertTrue(log.lines.any { it.contains("while com.example.notes is open") })
        // Other settings still apply meanwhile.
        sync.handle(
            Envelope.parseFrom(textSize(125)).toBuilder()
                .setSystemSettings(Envelope.parseFrom(textSize(125)).systemSettings.toBuilder().setDarkMode(false))
                .build().toByteArray(),
        )
        assertEquals(listOf("dark false"), calls)
        // The app closes: the size the host asked for last applies.
        calls.clear()
        sync.tasksRunning(emptyList())
        assertEquals(listOf("font scale 1.250"), calls)
        assertFalse(sync.holdsFontScale)

        // The same size again changes nothing.
        calls.clear()
        sync.connected()
        sync.handle(textSize(125))
        assertFalse(calls.any { it.startsWith("font scale") })

        // Back to 100%: the guest's font scale is 1 again. The new connection applied the
        // other settings in full above, so only what this step sets is looked at.
        calls.clear()
        sync.handle(textSize(100))
        assertEquals(listOf("font scale 1.000"), calls)
    }

    @Test
    fun fr4_a_font_scale_waits_until_the_tasks_are_read() {
        sync.handle(textSize(150))
        assertTrue(sync.holdsFontScale)
        assertFalse(calls.any { it.startsWith("font scale") })
        sync.tasksRunning(emptyList())
        assertTrue(calls.contains("font scale 1.500"))
        // An older host sends no font scale: the guest's stays as it is.
        calls.clear()
        val older = SystemSettingsSync(port, log)
        older.tasksRunning(emptyList())
        older.handle(seoul())
        assertFalse(calls.any { it.startsWith("font scale") })
    }

    @Test
    fun fr4_an_app_that_handles_a_change_of_font_scale_is_not_restarted_by_one() {
        val notes = io.github.thisisthepy.dsa.agent.tasks.TaskSnapshot(
            taskId = 3,
            kind = io.github.thisisthepy.dsa.agent.tasks.TaskKind.APP,
            packageName = "com.example.notes",
            label = "Notes",
            bounds = io.github.thisisthepy.dsa.agent.tasks.TaskBounds.EMPTY,
            focused = true,
        )
        val handles = notes.copy(taskId = 4, packageName = "com.example.reader", handlesFontScaleChange = true)
        val home = notes.copy(taskId = 5, kind = io.github.thisisthepy.dsa.agent.tasks.TaskKind.OTHER)
        assertEquals(
            listOf("com.example.notes"),
            io.github.thisisthepy.dsa.agent.tasks.restartedByFontScale(listOf(notes, handles, home)),
        )
        assertTrue(io.github.thisisthepy.dsa.agent.tasks.handlesFontScaleChange(0x40000000 or 0x1000))
        assertFalse(io.github.thisisthepy.dsa.agent.tasks.handlesFontScaleChange(0x1000))
    }

    @Test
    fun fr30_other_frames_are_not_settings() {
        val screen = Envelope.newBuilder().setScreenState(ScreenState.newBuilder().setOn(true)).build()
        assertFalse(sync.handle(screen.toByteArray()))
        assertFalse(sync.handle(byteArrayOf(1, 2, 3)))
        assertTrue(calls.isEmpty())
    }

    @Test
    fun fr30_the_accent_color_seeds_a_palette_from_white_to_black() {
        val palette = TonalPalette.of(0xFF0078D4.toInt())
        assertEquals(0xFFFFFFFF.toInt(), palette["system_accent1_0"])
        assertEquals(0xFF000000.toInt(), palette["system_accent1_1000"])
        val (hue, _, _) = TonalPalette.hsl(palette.getValue("system_accent1_500"))
        val (seedHue, _, _) = TonalPalette.hsl(0xFF0078D4.toInt())
        assertEquals(seedHue, hue, 3.0)
        // The neutrals are nearly grey.
        val (_, saturation, _) = TonalPalette.hsl(palette.getValue("system_neutral1_500"))
        assertTrue(saturation < 0.1)
    }
}

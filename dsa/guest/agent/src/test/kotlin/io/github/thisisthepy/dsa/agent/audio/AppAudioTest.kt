package io.github.thisisthepy.dsa.agent.audio

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.AppAudio as AppAudioMessage
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.SetAppVolume
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The host is the one place the guest's sound is turned down: each app with a window plays on
 * an output of its own, the host is told which app each output plays and the volume Android
 * gives it, Android's media volume is the focused app's, and a volume the user sets on the
 * host comes back as the app's media volume.
 */
class AppAudioTest {
    /** Android's media volume (0 to 15) and its routing, as a test sets them. */
    private class FakePort(private val outputs: Int = 2) : AppAudioPort {
        var volume = MediaVolume(index = 10, min = 0, max = 15, muted = false)
        val routes = mutableListOf<Pair<String, Int?>>()
        val sets = mutableListOf<Pair<Int, Boolean>>()
        var playing = emptySet<String>()
        var changed: (() -> Unit)? = null

        override fun start(onMediaVolumeChanged: () -> Unit): Int {
            changed = onMediaVolumeChanged
            return outputs
        }

        override fun route(packageName: String, slot: Int?): Boolean {
            routes += packageName to slot
            return true
        }

        override fun label(packageName: String) = packageName.substringAfterLast('.').replaceFirstChar(Char::uppercase)

        override fun mediaVolume() = volume

        override fun setMediaVolume(index: Int, muted: Boolean) {
            sets += index to muted
            volume = volume.copy(index = index, muted = muted)
            // Android tells of the change, as it does of every change.
            changed?.invoke()
        }

        override fun playing() = playing

        /** An app or a volume key sets the media volume. */
        fun press(index: Int, muted: Boolean = false) {
            volume = volume.copy(index = index, muted = muted)
            changed?.invoke()
        }
    }

    private val sent = mutableListOf<AppAudioMessage>()
    private val port = FakePort()
    private val store = AppVolumeStore.memory()
    private val audio = AppAudio(port, store, { frame ->
        sent += Envelope.parseFrom(frame).appAudio
        true
    }, RecordingLog())

    init {
        audio.start { work -> work() }
    }

    private fun last(): AppAudioMessage = sent.last()

    private fun stream(name: String) = last().streamsList.first { it.stream == name }

    private fun host(packageName: String, volume: Float, muted: Boolean = false): Boolean =
        audio.handle(
            Envelope.newBuilder()
                .setSetAppVolume(SetAppVolume.newBuilder().setPackageName(packageName).setVolume(volume).setMuted(muted))
                .build(),
        )

    @Test
    fun fr26_volume_is_controlled_on_the_host_only() {
        // An app opens a window: it plays on an output of its own, at full volume, and Android's
        // media volume becomes its own.
        audio.tasks(setOf("com.example.music"), "com.example.music")
        assertEquals(listOf<Pair<String, Int?>>("com.example.music" to 0), port.routes)
        assertEquals("com.example.music", stream("app 0").packageName)
        assertEquals("Music", stream("app 0").appLabel)
        assertEquals(1f, stream("app 0").volume, 0.001f)
        assertEquals(15, port.volume.index)
        // The agent's own change of the media volume is not the app's.
        assertEquals(1f, stream("app 0").volume, 0.001f)

        // The app sets its volume: the host is told the app's new volume.
        port.press(6)
        assertEquals(0.4f, stream("app 0").volume, 0.001f)
        // The shared output keeps its own.
        assertEquals(1f, stream(AppAudio.PRIMARY_STREAM).volume, 0.001f)

        // The user sets the app's volume on the host: Android's media volume becomes it, and
        // the app reads it.
        val before = sent.size
        assertTrue(host("com.example.music", 0.2f))
        assertEquals(3, port.volume.index)
        assertEquals(0.2f, stream("app 0").volume, 0.001f)
        assertTrue(sent.size > before)
        // And the mute.
        host("com.example.music", 0.2f, muted = true)
        assertTrue(port.volume.muted)
        assertTrue(stream("app 0").muted)
    }

    @Test
    fun fr26_the_focus_makes_the_media_volume_the_focused_apps() {
        audio.tasks(setOf("com.example.music", "com.example.video"), "com.example.music")
        port.press(9)
        audio.tasks(setOf("com.example.music", "com.example.video"), "com.example.video")
        // The video app has its own volume, full.
        assertEquals(15, port.volume.index)
        port.press(3)
        assertEquals(0.6f, stream("app 0").volume, 0.001f)
        assertEquals(0.2f, stream("app 1").volume, 0.001f)
        // Back to the music app: its volume again.
        audio.tasks(setOf("com.example.music", "com.example.video"), "com.example.music")
        assertEquals(9, port.volume.index)
    }

    @Test
    fun fr26_a_volume_set_on_the_host_for_another_app_leaves_the_media_volume() {
        audio.tasks(setOf("com.example.music", "com.example.video"), "com.example.music")
        val index = port.volume.index
        host("com.example.video", 0.3f)
        assertEquals(index, port.volume.index)
        assertEquals(0.3f, stream("app 1").volume, 0.001f)
        // It is the video app's when it has the focus.
        audio.tasks(setOf("com.example.music", "com.example.video"), "com.example.video")
        assertEquals(5, port.volume.index)
    }

    @Test
    fun fr26_an_app_keeps_its_output_and_the_oldest_quiet_one_gives_it_up() {
        audio.tasks(setOf("com.example.a", "com.example.b"), "com.example.a")
        // A's window closes; its music plays on.
        port.playing = setOf("com.example.a")
        audio.tasks(setOf("com.example.b"), "com.example.b")
        assertEquals("com.example.a", stream("app 0").packageName)
        // A third app: A still plays, B has a window, so there is no output for it.
        audio.tasks(setOf("com.example.b", "com.example.c"), "com.example.c")
        assertTrue(last().streamsList.none { it.packageName == "com.example.c" })
        // A stops playing: C takes its output.
        port.playing = emptySet()
        audio.tasks(setOf("com.example.b", "com.example.c"), "com.example.c")
        assertTrue(("com.example.a" to null) in port.routes)
        assertEquals("com.example.c", stream("app 0").packageName)
    }

    @Test
    fun fr26_an_app_without_an_output_has_the_shared_outputs_volume() {
        val none = FakePort(outputs = 0)
        val shared = mutableListOf<AppAudioMessage>()
        val audio = AppAudio(none, AppVolumeStore.memory(), { frame ->
            shared += Envelope.parseFrom(frame).appAudio
            true
        }, RecordingLog())
        audio.start { work -> work() }
        audio.tasks(setOf("com.example.music"), "com.example.music")
        assertTrue(none.routes.isEmpty())
        none.press(3)
        val list = shared.last().streamsList
        assertEquals(1, list.size)
        assertEquals(AppAudio.PRIMARY_STREAM, list[0].stream)
        assertEquals("", list[0].packageName)
        assertEquals(0.2f, list[0].volume, 0.001f)
    }

    @Test
    fun fr26_each_apps_volume_is_kept() {
        audio.tasks(setOf("com.example.music"), "com.example.music")
        host("com.example.music", 0.4f)
        val again = AppAudio(FakePort(), store, { true }, RecordingLog())
        val port = FakePort()
        val list = mutableListOf<AppAudioMessage>()
        val restarted = AppAudio(port, store, { frame ->
            list += Envelope.parseFrom(frame).appAudio
            true
        }, RecordingLog())
        restarted.start { work -> work() }
        restarted.tasks(setOf("com.example.music"), "com.example.music")
        assertEquals(6, port.volume.index)
        assertEquals(0.4f, list.last().streamsList.first { it.stream == "app 0" }.volume, 0.001f)
        assertFalse(again.handle(Envelope.getDefaultInstance()))
    }

    @Test
    fun fr26_nothing_is_sent_while_the_host_is_away_and_all_of_it_on_connecting() {
        var connected = false
        val list = mutableListOf<AppAudioMessage>()
        val audio = AppAudio(FakePort(), AppVolumeStore.memory(), { frame ->
            list += Envelope.parseFrom(frame).appAudio
            true
        }, RecordingLog(), isConnected = { connected })
        audio.start { work -> work() }
        audio.tasks(setOf("com.example.music"), "com.example.music")
        assertTrue(list.isEmpty())
        connected = true
        audio.connected()
        assertEquals(2, list.single().streamsCount)
        // The same list is not sent twice.
        audio.tasks(setOf("com.example.music"), "com.example.music")
        assertEquals(1, list.size)
        assertNull(list.single().streamsList.firstOrNull { it.stream == "app 1" })
    }

    @Test
    fun fr26_a_media_volume_index_is_a_fraction_and_back() {
        val volume = MediaVolume(index = 5, min = 0, max = 15, muted = false)
        assertEquals(1f / 3f, volume.fraction, 0.001f)
        assertEquals(3, volume.indexOf(0.2f))
        assertEquals(15, volume.indexOf(2f))
        assertEquals(1f, MediaVolume(0, 0, 0, false).fraction, 0.001f)
    }
}

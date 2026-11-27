package io.github.thisisthepy.dsa.agent.overlays

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.agent.tasks.AppDetails
import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class OverlayReporterTest {
    private val sent = mutableListOf<Envelope>()
    private var channelUp = true
    private val apps = AppInfoSource { packageName ->
        if (packageName == FLO) AppDetails("FLO", byteArrayOf(1), byteArrayOf(), false) else null
    }
    private val reporter = OverlayReporter(AGENT, apps) { frame ->
        if (channelUp) sent += decode(frame)
        channelUp
    }
    private val display = Size(3840, 3840)

    /** FLO's floating lyrics, as SurfaceFlinger lists them. */
    private fun lyrics(left: Int = 158, top: Int = 82, identity: Any = "faa9c03") = GuestWindow(
        identity = identity,
        packageName = FLO,
        ownerUid = 10157,
        type = OverlayRules.TYPE_APPLICATION_OVERLAY,
        surface = TaskBounds(left, top, left + 508, top + 138),
        title = FLO,
        visible = true,
        focusable = false,
        touchable = true,
        windowName = identity.toString(),
    )

    /** An app's own window: a task's surface. */
    private fun appWindow() = lyrics(identity = "task").copy(type = 1, surface = TaskBounds(0, 0, 900, 700))

    private fun updates() = sent.filter { it.bodyCase == Envelope.BodyCase.TASK_UPDATED }.map { it.taskUpdated }

    @Test
    fun fr41_an_app_overlay_window_is_reported_as_a_window_of_its_own() {
        reporter.update(listOf(lyrics(), appWindow()), display, 262)

        // The app's name and icon first, since the overlay's host window is the app's program's.
        assertEquals(listOf(Envelope.BodyCase.APP_INFO, Envelope.BodyCase.TASK_UPDATED), sent.map { it.bodyCase })
        assertEquals(FLO, sent[0].appInfo.packageName)
        val update = sent[1].taskUpdated
        assertEquals(OverlayRules.FIRST_ID, update.taskId)
        assertEquals(FLO, update.packageName)
        // A window whose title is only its package is named as its app.
        assertEquals("FLO", update.label)
        assertEquals(
            listOf(158, 82, 666, 220),
            listOf(update.bounds.left, update.bounds.top, update.bounds.right, update.bounds.bottom),
        )
        assertEquals(262, update.densityDpi)
        assertEquals(3840, update.maxWidth)
        assertEquals(3840, update.maxHeight)
        assertTrue(update.hasOverlay())
        assertEquals(2038, update.overlay.windowType)
        assertFalse(update.overlay.focusable)
        assertTrue(update.overlay.touchable)
        assertEquals(setOf(OverlayRules.FIRST_ID), reporter.reported)
        assertEquals(TaskBounds(158, 82, 666, 220), reporter.bounds[OverlayRules.FIRST_ID])
        assertEquals(setOf(FLO), reporter.packages)
    }

    @Test
    fun fr41_an_overlay_its_app_moves_is_reported_at_its_new_place_with_the_same_id() {
        reporter.update(listOf(lyrics()), display, 262)
        sent.clear()
        // Nothing changed: nothing is sent.
        reporter.update(listOf(lyrics()), display, 262)
        assertTrue(sent.isEmpty())

        reporter.update(listOf(lyrics(left = 258, top = 122)), display, 262)
        val update = updates().single()
        assertEquals(OverlayRules.FIRST_ID, update.taskId)
        assertEquals(listOf(258, 122), listOf(update.bounds.left, update.bounds.top))
        // Its app was told once.
        assertTrue(sent.none { it.bodyCase == Envelope.BodyCase.APP_INFO })
    }

    @Test
    fun fr41_an_overlay_that_is_removed_or_hidden_is_ended() {
        reporter.update(listOf(lyrics()), display, 262)
        sent.clear()
        // Hidden: its window is still listed, but shows nothing.
        reporter.update(listOf(lyrics().copy(visible = false)), display, 262)
        assertEquals(Envelope.BodyCase.TASK_REMOVED, sent.single().bodyCase)
        assertEquals(OverlayRules.FIRST_ID, sent.single().taskRemoved.taskId)
        assertTrue(reporter.reported.isEmpty())

        // Shown again, the same window is the same overlay.
        sent.clear()
        reporter.update(listOf(lyrics()), display, 262)
        assertEquals(OverlayRules.FIRST_ID, updates().single().taskId)

        // Removed (or its app's process died): gone, and a new window is a new overlay.
        sent.clear()
        reporter.update(emptyList(), display, 262)
        assertEquals(OverlayRules.FIRST_ID, sent.single().taskRemoved.taskId)
        sent.clear()
        reporter.update(listOf(lyrics(identity = "b01dface")), display, 262)
        assertEquals(OverlayRules.FIRST_ID + 1, updates().single().taskId)
    }

    @Test
    fun fr41_only_an_apps_overlay_types_are_overlays() {
        val own = AGENT
        // The older types an app built for Android 7.1 or older may still add.
        for (type in listOf(2038, 2002, 2003, 2006, 2007, 2010)) {
            assertTrue("type $type", OverlayRules.isOverlay(lyrics().copy(type = type), own))
        }
        // An app's own windows, its dialogs and popups, a toast, the input method.
        for (type in listOf(1, 2, 3, 1000, 1002, 2005, 2011)) {
            assertFalse("type $type", OverlayRules.isOverlay(lyrics().copy(type = type), own))
        }
        // The system's own windows of these types, the agent's, and one with no owner named.
        assertFalse(OverlayRules.isOverlay(lyrics().copy(ownerUid = 1000), own))
        assertFalse(OverlayRules.isOverlay(lyrics().copy(packageName = own), own))
        assertFalse(OverlayRules.isOverlay(lyrics().copy(packageName = ""), own))
        // One that shows nothing.
        assertFalse(OverlayRules.isOverlay(lyrics().copy(visible = false), own))
        assertFalse(OverlayRules.isOverlay(lyrics().copy(surface = TaskBounds.EMPTY), own))
        // An app of another user of the device is an app.
        assertTrue(OverlayRules.isOverlay(lyrics().copy(ownerUid = 1010157), own))
    }

    @Test
    fun fr41_several_overlays_each_get_an_id_and_a_report() {
        val chat = lyrics(left = 3000, top = 500, identity = "c4a7").copy(
            packageName = "com.example.chat",
            title = "Chat head",
            focusable = true,
        )
        reporter.update(listOf(chat, lyrics()), display, 262)
        val told = updates()
        assertEquals(2, told.size)
        assertEquals(setOf(OverlayRules.FIRST_ID, OverlayRules.FIRST_ID + 1), told.map { it.taskId }.toSet())
        val head = told.single { it.packageName == "com.example.chat" }
        // A window with a title of its own carries it, and says it may take the focus.
        assertEquals("Chat head", head.label)
        assertTrue(head.overlay.focusable)
    }

    @Test
    fun fr41_a_full_display_overlay_is_reported_for_the_hwcomposer_to_draw_over_the_apps() {
        val filter = lyrics(identity = "f117e2").copy(surface = TaskBounds(0, 0, 3840, 3840), touchable = false)
        reporter.update(listOf(filter), display, 262)
        val update = updates().single()
        assertEquals(listOf(0, 0, 3840, 3840), listOf(update.bounds.left, update.bounds.top, update.bounds.right, update.bounds.bottom))
        assertFalse(update.overlay.touchable)
    }

    @Test
    fun fr41_a_new_connection_hears_of_every_overlay_again() {
        reporter.update(listOf(lyrics()), display, 262)
        sent.clear()
        reporter.resend()
        reporter.update(listOf(lyrics()), display, 262)
        assertEquals(listOf(Envelope.BodyCase.APP_INFO, Envelope.BodyCase.TASK_UPDATED), sent.map { it.bodyCase })
    }

    @Test
    fun fr41_an_overlay_that_could_not_be_sent_is_sent_with_the_next_update() {
        channelUp = false
        reporter.update(listOf(lyrics()), display, 262)
        assertTrue(reporter.reported.isEmpty())
        channelUp = true
        reporter.update(listOf(lyrics()), display, 262)
        assertEquals(OverlayRules.FIRST_ID, updates().single().taskId)
    }

    /** The lyrics `width` by `height`, as FLO lays them out at another density. */
    private fun lyricsSized(width: Int, height: Int) = lyrics().copy(surface = TaskBounds(158, 82, 158 + width, 82 + height))

    @Test
    fun fr41_an_overlay_is_reported_at_the_density_given_once_its_app_has_laid_it_out_again() {
        reporter.update(listOf(lyrics()), display, 262)
        assertEquals("faa9c03", reporter.windowName(OverlayRules.FIRST_ID))
        assertEquals(0, updates().single().overlay.densityDpi)
        sent.clear()
        // The host gives the lyrics' window 262 dpi: until FLO lays it out again, it is still
        // drawn as the rest of FLO, and moving it changes nothing of that.
        reporter.densityGiven(OverlayRules.FIRST_ID, 262)
        reporter.update(listOf(lyrics(left = 258)), display, 262)
        assertEquals(0, updates().single().overlay.densityDpi)
        sent.clear()
        // FLO has laid it out at 262: 1.6 times larger, and reported with its density.
        reporter.update(listOf(lyricsSized(813, 221)), display, 262)
        assertEquals(262, updates().single().overlay.densityDpi)
        sent.clear()
        // A change of size of its own afterwards keeps the density.
        reporter.update(listOf(lyricsSized(900, 221)), display, 262)
        assertEquals(262, updates().single().overlay.densityDpi)
        sent.clear()
        // Back to FLO's own density: reported so once it is laid out again.
        reporter.densityGiven(OverlayRules.FIRST_ID, 0)
        reporter.update(listOf(lyricsSized(900, 221)), display, 262)
        assertTrue(sent.isEmpty())
        reporter.update(listOf(lyrics()), display, 262)
        assertEquals(0, updates().single().overlay.densityDpi)
    }

    @Test
    fun fr41_an_overlay_given_the_density_it_has_waits_for_nothing() {
        reporter.update(listOf(lyrics()), display, 262)
        reporter.densityGiven(OverlayRules.FIRST_ID, 0)
        sent.clear()
        reporter.update(listOf(lyricsSized(600, 138)), display, 262)
        assertEquals(0, updates().single().overlay.densityDpi)
    }

    @Test
    fun fr41_an_overlay_there_before_the_agent_is_reported_at_the_density_the_framework_gave_it() {
        // The agent started again while FLO still draws its lyrics at 262: the framework says
        // so, for the windows of the first list only.
        val asked = mutableListOf<String>()
        val restarted = OverlayReporter(AGENT, apps, givenDensity = { window ->
            asked += window
            if (window == "faa9c03") 262 else 0
        }) { frame ->
            sent += decode(frame)
            true
        }
        restarted.update(listOf(lyrics()), display, 262)
        assertEquals(262, updates().single().overlay.densityDpi)
        // A window that appears later was given nothing, and the framework is not asked.
        restarted.update(listOf(lyrics(), lyrics(identity = "c0ffee1")), display, 262)
        assertEquals(listOf("faa9c03"), asked)
        assertEquals(0, updates().last().overlay.densityDpi)
    }

    private companion object {
        const val FLO = "skplanet.musicmate"
    }
}

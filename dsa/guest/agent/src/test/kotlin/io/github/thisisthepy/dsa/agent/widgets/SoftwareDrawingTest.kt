package io.github.thisisthepy.dsa.agent.widgets

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The pictures of the widgets on the host's desktop are drawn in software, so the agent holds
 * no GPU device for them; the GPU draws a widget only while software cannot.
 */
class SoftwareDrawingTest {
    @Test
    fun nfr1_widget_pictures_are_drawn_without_a_gpu_device() {
        val mode = WidgetDrawMode()
        assertEquals(WidgetDraw.SOFTWARE, mode.next())
        assertFalse(mode.holdsGpu)
        // A clock draws a picture each minute, each in software.
        repeat(3) {
            mode.drewInSoftware()
            assertEquals(WidgetDraw.SOFTWARE, mode.next())
        }
        mode.viewsReplaced()
        assertEquals(WidgetDraw.SOFTWARE, mode.next())
        assertFalse(mode.holdsGpu)
    }

    @Test
    fun nfr1_a_widget_draws_with_the_gpu_only_while_its_views_hold_a_hardware_bitmap() {
        val mode = WidgetDrawMode()
        // Software met a bitmap that lives on the GPU.
        mode.needsGpu()
        assertTrue(mode.holdsGpu)
        // Its next pictures, as an animation's, go straight to the GPU, not through a failing
        // software drawing each time.
        assertEquals(WidgetDraw.GPU, mode.next())
        assertEquals(WidgetDraw.GPU, mode.next())
        // New views from the app may have no such bitmap: software is tried again.
        mode.viewsReplaced()
        assertEquals(WidgetDraw.SOFTWARE, mode.next())
        // They still hold one: back to the GPU until the next views.
        mode.needsGpu()
        assertEquals(WidgetDraw.GPU, mode.next())
        mode.viewsReplaced()
        assertEquals(WidgetDraw.SOFTWARE, mode.next())
        // Software drew them: the GPU renderer goes.
        mode.drewInSoftware()
        assertFalse(mode.holdsGpu)
        assertEquals(WidgetDraw.SOFTWARE, mode.next())
    }

    @Test
    fun fr12_a_widget_drawn_in_software_clips_a_view_to_its_rounded_outline_as_the_gpu_did() {
        // A rounded background that clips its content, as Android 12 widgets are made.
        assertTrue(OutlineClip.needed(true, true, 0, 0, 400, 200, 24f, 400, 200))
        // An outline inset from the view's bounds cuts off more than the bounds do.
        assertTrue(OutlineClip.needed(true, true, 8, 8, 392, 192, 0f, 400, 200))
        // A plain rectangle the size of the view: software's own clip to the bounds is the same.
        assertFalse(OutlineClip.needed(true, true, 0, 0, 400, 200, 0f, 400, 200))
        assertFalse(OutlineClip.needed(true, true, -2, -2, 402, 202, 0f, 400, 200))
        // A view that does not clip to its outline is drawn as it is, rounded outline or not.
        assertFalse(OutlineClip.needed(false, true, 0, 0, 400, 200, 24f, 400, 200))
        // An outline that is a path, an empty outline and a view with no size clip nothing.
        assertFalse(OutlineClip.needed(true, false, 0, 0, 0, 0, -1f, 400, 200))
        assertFalse(OutlineClip.needed(true, true, 10, 10, 10, 10, 4f, 400, 200))
        assertFalse(OutlineClip.needed(true, true, 0, 0, 400, 200, 24f, 0, 200))
    }
}

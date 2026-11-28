package io.github.thisisthepy.dsa.agent.widgets

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The pictures of the widgets on the host's desktop are drawn in software, so the agent holds
 * no GPU device for them, but only when software draws exactly the picture the GPU would. Any
 * view that uses something only the GPU renderer draws sends its widget to the GPU.
 */
class SoftwareDrawingTest {
    /** A clock widget as Android 12 widgets are made: a rounded root that clips, and text. */
    private val clock = listOf(
        ViewFacts(depth = 0),
        ViewFacts(depth = 1, clipToOutline = true, outline = OutlineShape.RoundRect(0, 0, 400, 200, 24f)),
        ViewFacts(depth = 2),
        ViewFacts(depth = 2),
    )

    private fun choose(views: List<ViewFacts>, mode: WidgetDrawMode = WidgetDrawMode(), touching: Boolean = false) =
        mode.choose(WidgetDrawRule.needs(views, touching))

    @Test
    fun nfr1_widget_pictures_are_drawn_without_a_gpu_device() {
        val mode = WidgetDrawMode()
        assertEquals(WidgetDraw.SOFTWARE, choose(clock, mode))
        // A clock draws a picture each minute, each in software.
        repeat(3) { assertEquals(WidgetDraw.SOFTWARE, choose(clock, mode)) }
        // Plain views, a see-through view that fades as a layer on the GPU too, a view that is
        // only moved or scaled, and a ripple at rest all draw the same in software.
        val plain = listOf(
            ViewFacts(depth = 1),
            ViewFacts(depth = 2, alpha = 0.5f, overlappingRendering = true),
            ViewFacts(depth = 2, ripple = true),
            ViewFacts(depth = 2, scrolls = true),
            ViewFacts(depth = 3, clipToOutline = true, outline = OutlineShape.None),
        )
        assertEquals(WidgetDraw.SOFTWARE, choose(plain, mode))
    }

    @Test
    fun fr12_a_widget_that_software_cannot_draw_exactly_is_drawn_with_the_gpu() {
        val cases = mapOf(
            GpuNeed.DEPTH to ViewFacts(depth = 2, z = 4f),
            GpuNeed.HARDWARE_BITMAP to ViewFacts(depth = 2, hardwareBitmap = true),
            GpuNeed.GAINMAP to ViewFacts(depth = 2, gainmap = true),
            GpuNeed.LAYER to ViewFacts(depth = 2, hasLayer = true),
            GpuNeed.ROTATION to ViewFacts(depth = 2, rotated = true),
            GpuNeed.ANIMATION to ViewFacts(depth = 2, animating = true),
            GpuNeed.FADE_WITHOUT_LAYER to ViewFacts(depth = 2, alpha = 0.5f, overlappingRendering = false),
            GpuNeed.ANIMATED_DRAWABLE to ViewFacts(depth = 2, animatedDrawable = true),
            GpuNeed.TEXT_SHADOW to ViewFacts(depth = 2, textShadow = true),
            GpuNeed.TOUCH_FEEDBACK to ViewFacts(depth = 2, ripple = true, feedbackActive = true),
        )
        for ((need, view) in cases) {
            assertEquals(need.name, setOf(need), WidgetDrawRule.needs(clock + view, touching = false))
            assertEquals(need.name, WidgetDraw.GPU, choose(clock + view))
        }
        // A lowered view is ordered by depth on the GPU too.
        assertEquals(WidgetDraw.GPU, choose(clock + ViewFacts(depth = 2, z = -1f)))
        // Outline clips software cannot match: a path, one nested in the app's views (a rounded
        // photo inside a card), and one on a see-through view.
        val outlines = listOf(
            ViewFacts(depth = 1, clipToOutline = true, outline = OutlineShape.Other),
            ViewFacts(depth = 2, clipToOutline = true, outline = OutlineShape.RoundRect(0, 0, 100, 100, 16f)),
            ViewFacts(depth = 2, clipToOutline = true, outline = OutlineShape.RoundRect(0, 0, 100, 100, 0f)),
            ViewFacts(depth = 1, clipToOutline = true, outline = OutlineShape.RoundRect(0, 0, 100, 100, 16f), alpha = 0.9f),
            ViewFacts(depth = 1, clipToOutline = true, outline = OutlineShape.RoundRect(0, 0, 100, 100, 16f), hasLayer = true),
        )
        for (view in outlines) {
            assertTrue(view.toString(), GpuNeed.OUTLINE_CLIP in WidgetDrawRule.needs(listOf(view), touching = false))
        }
        // A view that keeps its outline but does not clip to it is drawn as it is.
        assertEquals(
            WidgetDraw.SOFTWARE,
            choose(listOf(ViewFacts(depth = 2, outline = OutlineShape.Other))),
        )
    }

    @Test
    fun fr12_a_software_widget_picture_matches_the_gpu_picture() {
        // The GPU renderer clips a rounded outline with an antialiased round rectangle and a
        // square one with a plain rectangle; software clips with the same shape the same way.
        assertEquals(
            OutlineClipOp(0, 0, 400, 200, 24f, antiAlias = true),
            WidgetDrawRule.clipOp(true, OutlineShape.RoundRect(0, 0, 400, 200, 24f)),
        )
        assertEquals(
            OutlineClipOp(8, 8, 392, 192, 0f, antiAlias = false),
            WidgetDrawRule.clipOp(true, OutlineShape.RoundRect(8, 8, 392, 192, 0f)),
        )
        // Nothing is clipped where the GPU clips nothing.
        assertNull(WidgetDrawRule.clipOp(false, OutlineShape.RoundRect(0, 0, 400, 200, 24f)))
        assertNull(WidgetDrawRule.clipOp(true, OutlineShape.None))
        // And no view is changed for it: the rule reads views, it gives them nothing to set.
        assertEquals(emptySet<GpuNeed>(), WidgetDrawRule.needs(clock, touching = false))
    }

    @Test
    fun nfr1_a_widget_draws_with_the_gpu_only_while_its_views_need_it() {
        val mode = WidgetDrawMode()
        // Software met something it could not draw (a bitmap that lives on the GPU, found
        // only while drawing): the GPU draws these views from then on.
        mode.softwareFailed()
        assertEquals(WidgetDraw.GPU, choose(clock, mode))
        assertEquals(WidgetDraw.GPU, choose(clock, mode))
        // New views from the app are judged again.
        mode.viewsReplaced()
        assertEquals(WidgetDraw.SOFTWARE, choose(clock, mode))
        // A raised view sends the widget to the GPU only while it is there.
        assertEquals(WidgetDraw.GPU, choose(clock + ViewFacts(depth = 2, z = 2f), mode))
        assertEquals(WidgetDraw.SOFTWARE, choose(clock, mode))
    }

    @Test
    fun fr12_a_touch_on_a_widget_with_touch_feedback_is_drawn_with_the_gpu_until_it_settles() {
        val list = clock + ViewFacts(depth = 2, scrolls = true) + ViewFacts(depth = 3, ripple = true)
        assertTrue(WidgetDrawRule.touchNeedsGpu(list))
        assertFalse(WidgetDrawRule.touchNeedsGpu(clock))
        val mode = WidgetDrawMode()
        assertFalse(mode.touching(1_000))
        mode.touchDown()
        assertTrue(mode.touching(1_000))
        assertEquals(WidgetDraw.GPU, choose(list, mode, mode.touching(1_000)))
        // A widget without touch feedback stays in software while it is touched.
        assertEquals(WidgetDraw.SOFTWARE, choose(clock, mode, mode.touching(1_000)))
        mode.touchUp(2_000)
        assertTrue(mode.touching(2_000 + WidgetDrawMode.TOUCH_SETTLE_MILLIS - 1))
        assertEquals(WidgetDraw.GPU, choose(list, mode, mode.touching(2_100)))
        assertFalse(mode.touching(2_000 + WidgetDrawMode.TOUCH_SETTLE_MILLIS))
        assertEquals(WidgetDraw.SOFTWARE, choose(list, mode, mode.touching(2_000 + WidgetDrawMode.TOUCH_SETTLE_MILLIS)))
    }
}

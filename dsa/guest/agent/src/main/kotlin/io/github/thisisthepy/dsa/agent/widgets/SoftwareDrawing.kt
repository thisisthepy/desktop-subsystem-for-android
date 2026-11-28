package io.github.thisisthepy.dsa.agent.widgets

/** How one picture of a widget is drawn. */
enum class WidgetDraw {
    /** On the CPU, onto a bitmap: the agent holds no GPU device for it. */
    SOFTWARE,

    /** With the framework's GPU renderer, exactly as every widget was drawn before. */
    GPU,
}

/**
 * Something in a widget's views that software drawing cannot draw as the GPU renderer does.
 * Any one of them sends the widget to the GPU.
 */
enum class GpuNeed {
    /**
     * A view raised or lowered (elevation or translationZ): the GPU renderer draws its shadow
     * and orders it by depth; software draws no shadow.
     */
    DEPTH,

    /**
     * A view clips to an outline that software cannot clip exactly as the GPU does: an outline
     * that is a path or empty, or one on a view nested inside the app's own views, which the
     * agent has no hook into, or one on a see-through view, whose layer the GPU clips inside
     * while software would clip it twice.
     */
    OUTLINE_CLIP,

    /** A bitmap that lives on the GPU, which software cannot draw at all. */
    HARDWARE_BITMAP,

    /** A bitmap with a gain map (HDR), which the two renderers may tone map differently. */
    GAINMAP,

    /** A view with a layer of its own, which software draws through a cache bitmap. */
    LAYER,

    /** A rotated view: text and bitmaps are then rasterized by different means on the GPU. */
    ROTATION,

    /**
     * A running view animation, or a flipper that animates its flips, whose fades the GPU
     * composes differently.
     */
    ANIMATION,

    /**
     * A see-through view that says it draws nothing overlapping: the GPU fades each of its
     * drawings, software fades them together as one layer.
     */
    FADE_WITHOUT_LAYER,

    /**
     * An animated vector or animated image: the GPU renderer runs it on its own thread, and
     * one software drawing would move it to the main thread for good.
     */
    ANIMATED_DRAWABLE,

    /** Text with a blurred shadow: the GPU blurs it with its own filter. */
    TEXT_SHADOW,

    /**
     * Touch feedback in progress: a ripple, or a list's overscroll stretch, which the GPU
     * renderer draws with effects software does not have.
     */
    TOUCH_FEEDBACK,
}

/** The shape a view's outline provider gave it. */
sealed class OutlineShape {
    /** The view has no outline provider: nothing is clipped, on the GPU either. */
    object None : OutlineShape()

    /** A rectangle with corners of [radius], from [left], [top] to [right], [bottom] in the view's pixels. */
    data class RoundRect(val left: Int, val top: Int, val right: Int, val bottom: Int, val radius: Float) : OutlineShape()

    /** A path, an empty outline, or one that could not be read. */
    object Other : OutlineShape()
}

/**
 * What one view of a widget shows, as far as choosing its renderer goes. The platform reads
 * these from the views that show (visible, under visible parents), before each picture.
 */
data class ViewFacts(
    /** How deep the view is: 0 is the widget's own view, 1 the root of the app's views. */
    val depth: Int,
    /** Elevation plus translationZ, in pixels. */
    val z: Float = 0f,
    val clipToOutline: Boolean = false,
    val outline: OutlineShape = OutlineShape.None,
    /** Alpha times transition alpha. */
    val alpha: Float = 1f,
    val overlappingRendering: Boolean = true,
    val hasLayer: Boolean = false,
    val rotated: Boolean = false,
    val animating: Boolean = false,
    val hardwareBitmap: Boolean = false,
    val gainmap: Boolean = false,
    val animatedDrawable: Boolean = false,
    val textShadow: Boolean = false,
    /** Holds a ripple (touch feedback that the GPU renderer animates). */
    val ripple: Boolean = false,
    /**
     * Touch feedback shows right now, touched or not: a ripple of it is pressed, focused or
     * hovered, or it is a list still stretched past its end, as after a fling.
     */
    val feedbackActive: Boolean = false,
    /** A list or a stack, which stretches when scrolled past its end. */
    val scrolls: Boolean = false,
)

/** How a view that clips to its outline is clipped in software, as the GPU renderer clips it. */
data class OutlineClipOp(
    val left: Int,
    val top: Int,
    val right: Int,
    val bottom: Int,
    val radius: Float,
    /**
     * Whether the clip is antialiased. The GPU renderer clips a rounded outline with an
     * antialiased round rectangle and a square one with a plain rectangle clip, and so does
     * this. Android's software canvas clips a path antialiased and a rectangle not.
     */
    val antiAlias: Boolean,
)

/**
 * Chooses, for each picture of a widget, whether software can draw it exactly as the GPU
 * renderer would, from what its views show now.
 *
 * Software draws the same drawing operations as the GPU renderer, with the same geometry,
 * sampling and clips, so it draws the same picture. What the GPU renderer adds of its own is
 * listed in [GpuNeed]; a widget whose views use any of it is drawn with the GPU, exactly as
 * before. The one renderer feature software reproduces is the outline clip of the app's root
 * view, which the agent's own widget view applies around it ([clipOp]).
 */
object WidgetDrawRule {
    /** Everything in [views] that needs the GPU; [touching] while a touch or its feedback lasts. */
    fun needs(views: List<ViewFacts>, touching: Boolean): Set<GpuNeed> {
        val needs = sortedSetOf<GpuNeed>()
        for (view in views) {
            if (view.z != 0f) needs += GpuNeed.DEPTH
            if (view.clipToOutline && !softwareClips(view)) needs += GpuNeed.OUTLINE_CLIP
            if (view.hardwareBitmap) needs += GpuNeed.HARDWARE_BITMAP
            if (view.gainmap) needs += GpuNeed.GAINMAP
            if (view.hasLayer) needs += GpuNeed.LAYER
            if (view.rotated) needs += GpuNeed.ROTATION
            if (view.animating) needs += GpuNeed.ANIMATION
            if (view.alpha < 1f && !view.overlappingRendering) needs += GpuNeed.FADE_WITHOUT_LAYER
            if (view.animatedDrawable) needs += GpuNeed.ANIMATED_DRAWABLE
            if (view.textShadow) needs += GpuNeed.TEXT_SHADOW
            if (view.feedbackActive || (touching && (view.ripple || view.scrolls))) needs += GpuNeed.TOUCH_FEEDBACK
        }
        return needs
    }

    /** Whether a touch on these views draws feedback that only the GPU renderer draws. */
    fun touchNeedsGpu(views: List<ViewFacts>): Boolean = views.any { it.ripple || it.scrolls }

    /** Whether software clips [view] to its outline exactly as the GPU renderer does. */
    private fun softwareClips(view: ViewFacts): Boolean = when (view.outline) {
        OutlineShape.None -> true
        OutlineShape.Other -> false
        is OutlineShape.RoundRect -> view.depth == 1 && view.alpha >= 1f && !view.hasLayer && !view.animating
    }

    /**
     * The clip software applies to a view that clips to its outline [shape], as the GPU
     * renderer clips it; null when the GPU clips nothing.
     */
    fun clipOp(clipToOutline: Boolean, shape: OutlineShape): OutlineClipOp? {
        if (!clipToOutline || shape !is OutlineShape.RoundRect) return null
        return OutlineClipOp(shape.left, shape.top, shape.right, shape.bottom, shape.radius, antiAlias = shape.radius != 0f)
    }
}

/**
 * The renderer state of one widget, kept across the drawings that replace each other.
 *
 * A widget is drawn in software unless [WidgetDrawRule] finds something only the GPU draws
 * exactly, or software failed to draw its current views; then it is drawn with the GPU until
 * its app sends new views. A touch on a widget whose views give touch feedback sends it to the
 * GPU for as long as the touch and [TOUCH_SETTLE_MILLIS] after it.
 */
class WidgetDrawMode {
    private var softwareFailed = false
    private var down = false
    private var releasedAt = Long.MIN_VALUE

    /** Software could not draw the current views: the GPU draws them until new views come. */
    fun softwareFailed() {
        softwareFailed = true
    }

    /** The widget's app sent new views: they are judged again. */
    fun viewsReplaced() {
        softwareFailed = false
    }

    fun touchDown() {
        down = true
    }

    fun touchUp(nowMillis: Long) {
        down = false
        releasedAt = nowMillis
    }

    /** Whether a touch, or the feedback that follows it, lasts at [nowMillis]. */
    fun touching(nowMillis: Long): Boolean =
        down || (releasedAt != Long.MIN_VALUE && nowMillis - releasedAt < TOUCH_SETTLE_MILLIS)

    /** How the next picture is drawn, given what its views [needs]. */
    fun choose(needs: Set<GpuNeed>): WidgetDraw =
        if (softwareFailed || needs.isNotEmpty()) WidgetDraw.GPU else WidgetDraw.SOFTWARE

    companion object {
        /** How long after a touch ends its ripple or stretch may still be moving. */
        const val TOUCH_SETTLE_MILLIS = 1_500L
    }
}

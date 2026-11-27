package io.github.thisisthepy.dsa.agent.widgets

/** How one picture of a widget is drawn. */
enum class WidgetDraw {
    /** On the CPU, onto a bitmap: the agent holds no GPU device for it. */
    SOFTWARE,

    /** With the framework's GPU renderer, for views that software cannot draw. */
    GPU,
}

/**
 * Whether a widget's next picture is drawn in software or with the GPU.
 *
 * A widget is drawn in software, so the agent's process keeps no GPU device, context or color
 * buffers for the pictures of the widgets on the host's desktop. Software cannot draw a bitmap
 * that lives on the GPU (a hardware bitmap), which an app may put in its widget's views. Only
 * then is the widget drawn with the GPU, and only until its app sends new views: the next
 * picture after that tries software again, and the GPU renderer goes once software draws.
 */
class WidgetDrawMode {
    private var gpu = false
    private var retry = false

    /** Whether the widget keeps a GPU renderer: software could not draw its current views. */
    val holdsGpu: Boolean get() = gpu

    /** How the next picture is drawn. */
    fun next(): WidgetDraw = if (gpu && !retry) WidgetDraw.GPU else WidgetDraw.SOFTWARE

    /** The widget's app sent new views: software may draw them. */
    fun viewsReplaced() {
        if (gpu) retry = true
    }

    /** Software drew the picture: the widget needs no GPU renderer any more. */
    fun drewInSoftware() {
        gpu = false
        retry = false
    }

    /** Software could not draw the widget's views, which hold a bitmap that lives on the GPU. */
    fun needsGpu() {
        gpu = true
        retry = false
    }
}

/**
 * Clipping a view to its outline in a widget drawn in software.
 *
 * A view that clips to its outline (`android:clipToOutline`, or RemoteViews'
 * `setViewOutlinePreferredRadius`), as a widget's rounded background does, is clipped by the
 * GPU renderer only: software drawing clips a view to its bounds and nothing more. So the
 * agent clips it itself, and only where the outline cuts off more than the bounds do.
 */
object OutlineClip {
    /**
     * Whether software drawing must clip a view of [width] by [height] pixels to its outline:
     * the view clips to its outline ([clipToOutline]), the outline is a rectangle with
     * corners of [radius] ([roundRect]) from [left], [top] to [right], [bottom] in the view's
     * pixels, and it either rounds its corners or lies inside the view's bounds. An outline
     * that is no rectangle (a path) is not clipped: its shape cannot be read back. An empty
     * outline clips nothing, on the GPU too.
     */
    fun needed(
        clipToOutline: Boolean,
        roundRect: Boolean,
        left: Int,
        top: Int,
        right: Int,
        bottom: Int,
        radius: Float,
        width: Int,
        height: Int,
    ): Boolean {
        if (!clipToOutline || !roundRect || width <= 0 || height <= 0) return false
        if (right <= left || bottom <= top) return false
        val wholeView = left <= 0 && top <= 0 && right >= width && bottom >= height
        return radius > 0f || !wholeView
    }
}

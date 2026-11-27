package io.github.thisisthepy.dsa.agent.platform

import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.Outline
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PixelFormat
import android.graphics.PorterDuff
import android.graphics.PorterDuffXfermode
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.drawable.Drawable
import android.view.View
import android.view.ViewGroup
import io.github.thisisthepy.dsa.agent.widgets.OutlineClip
import java.lang.ref.WeakReference
import java.util.WeakHashMap

/**
 * Clips the views of a widget drawn in software to their outlines, as the GPU renderer does.
 *
 * Software drawing clips a view to its bounds only. A widget's rounded background that clips
 * its content (`android:clipToOutline`) would then show its content, a picture or a coloured
 * band, square in the corners the GPU cut round. So each such view is drawn into a software
 * layer of its own, and a drawable in its overlay, which is drawn after its content and its
 * children, clears what lies outside its outline in that layer ([OutsideOutline]). The layer
 * holds the view alone, so clearing it takes away nothing drawn under the view.
 *
 * The views are the widget views' own, which the agent keeps across a widget's drawings, so
 * one instance serves every widget. Main thread only.
 */
internal class SoftwareOutlines {
    private val clipped = WeakHashMap<View, OutsideOutline>()
    private val outline = Outline()
    private val rect = Rect()

    /**
     * Readies every view under [root] for a drawing in software: a view that clips to its
     * outline gets its layer and its clearing drawable, and one that no longer does loses
     * them. Each clipped view is invalidated, so its layer is drawn again from its views and
     * never shows a change that did not reach it. Invalidating calls the widget's root, so the
     * caller keeps it from asking for another drawing meanwhile.
     */
    fun prepare(root: View) {
        visit(root)
    }

    /**
     * Takes the software layers off the views under [root], for a drawing with the GPU: a
     * software layer cannot hold a bitmap that lives on the GPU. Their clearing drawables
     * stay and do nothing without the layer; [prepare] puts the layers back.
     */
    fun release(root: View) {
        walk(root) { view ->
            if (view in clipped && view.layerType == View.LAYER_TYPE_SOFTWARE) {
                view.setLayerType(View.LAYER_TYPE_NONE, null)
            }
        }
    }

    private fun walk(view: View, each: (View) -> Unit) {
        each(view)
        if (view is ViewGroup) {
            for (i in 0 until view.childCount) view.getChildAt(i)?.let { walk(it, each) }
        }
    }

    private fun visit(root: View) = walk(root) { view ->
        val shape = shape(view)
        val clip = clipped[view]
        if (shape == null) {
            if (clip != null) {
                clipped.remove(view)
                view.overlay.remove(clip)
                if (view.layerType == View.LAYER_TYPE_SOFTWARE) view.setLayerType(View.LAYER_TYPE_NONE, null)
                if (clip.nudged) view.transitionAlpha = 1f
            }
            return@walk
        }
        val outside = clip ?: OutsideOutline(view).also {
            clipped[view] = it
            view.overlay.add(it)
        }
        outside.set(shape, outline.radius, view.width, view.height)
        if (view.layerType != View.LAYER_TYPE_SOFTWARE) view.setLayerType(View.LAYER_TYPE_SOFTWARE, null)
        // An opaque view's software layer has no alpha channel, and what is cleared in it
        // would show black. A view a hair short of opaque gets a layer with alpha; the GPU
        // left those corners clear too.
        if (view.isOpaque) {
            view.transitionAlpha = NEARLY_OPAQUE
            outside.nudged = true
        }
        view.invalidate()
    }

    /** The rectangle [view] clips its content to, or null when software needs no clip of its own. */
    private fun shape(view: View): Rect? {
        if (view.visibility != View.VISIBLE || !view.clipToOutline || view.width <= 0 || view.height <= 0) return null
        val provider = view.outlineProvider ?: return null
        outline.setEmpty()
        try {
            provider.getOutline(view, outline)
        } catch (e: RuntimeException) {
            return null
        }
        val roundRect = !outline.isEmpty && outline.getRect(rect)
        val needed = OutlineClip.needed(
            clipToOutline = true,
            roundRect = roundRect,
            left = rect.left,
            top = rect.top,
            right = rect.right,
            bottom = rect.bottom,
            radius = outline.radius,
            width = view.width,
            height = view.height,
        )
        return if (needed) Rect(rect) else null
    }

    /**
     * Clears what lies outside a view's outline, in the view's own software layer. It keeps
     * its view weakly: the view's overlay holds it, and the map that finds it keeps its views
     * weakly too.
     */
    private class OutsideOutline(view: View) : Drawable() {
        private val view = WeakReference(view)
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply { xfermode = PorterDuffXfermode(PorterDuff.Mode.CLEAR) }
        private val path = Path()
        private var width = 0
        private var height = 0

        /** Whether the view was made a hair short of opaque for its layer to have alpha. */
        var nudged = false

        /** The outline: [shape], with corners of [radius], in a view of [width] by [height] pixels. */
        fun set(shape: Rect, radius: Float, width: Int, height: Int) {
            path.reset()
            path.fillType = Path.FillType.EVEN_ODD
            path.addRect(0f, 0f, width.toFloat(), height.toFloat(), Path.Direction.CW)
            path.addRoundRect(RectF(shape), radius, radius, Path.Direction.CW)
            this.width = width
            this.height = height
            setBounds(0, 0, width, height)
        }

        override fun draw(canvas: Canvas) {
            val view = view.get() ?: return
            // Only into the view's own layer, which is exactly the view's size: anywhere else
            // clearing would take away what is drawn under the view.
            if (canvas.isHardwareAccelerated || view.layerType != View.LAYER_TYPE_SOFTWARE) return
            if (canvas.width != width || canvas.height != height) return
            val saved = canvas.save()
            // The layer's canvas is moved by the view's scroll; the outline stays in place.
            canvas.translate(view.scrollX.toFloat(), view.scrollY.toFloat())
            canvas.drawPath(path, paint)
            canvas.restoreToCount(saved)
        }

        override fun setAlpha(alpha: Int) {}

        override fun setColorFilter(colorFilter: ColorFilter?) {}

        @Deprecated("Deprecated in Java")
        override fun getOpacity(): Int = PixelFormat.TRANSLUCENT
    }

    private companion object {
        /** An alpha that draws as 254 of 255: no eye tells it from opaque. */
        const val NEARLY_OPAQUE = 0.999f
    }
}

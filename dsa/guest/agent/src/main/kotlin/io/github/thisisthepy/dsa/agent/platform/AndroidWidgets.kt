package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityOptions
import android.app.PendingIntent
import android.app.Presentation
import android.appwidget.AppWidgetHost
import android.appwidget.AppWidgetHostView
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProviderInfo
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.HardwareRenderer
import android.graphics.PixelFormat
import android.graphics.PorterDuff
import android.graphics.RenderNode
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.Drawable
import android.hardware.HardwareBuffer
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.os.UserManager
import android.util.SizeF
import android.view.Display
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.widget.AnalogClock
import android.widget.FrameLayout
import android.widget.RemoteViews
import android.widget.TextClock
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.widgets.FrameGate
import io.github.thisisthepy.dsa.agent.widgets.WidgetInfo
import io.github.thisisthepy.dsa.agent.widgets.WidgetManager
import io.github.thisisthepy.dsa.agent.widgets.WidgetPlatform
import io.github.thisisthepy.dsa.agent.widgets.WidgetSize
import io.github.thisisthepy.dsa.agent.widgets.WidgetStore
import io.github.thisisthepy.dsa.agent.widgets.allClear
import io.github.thisisthepy.dsa.control.v1.PointerAction
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentHashMap
import kotlin.math.ceil

/**
 * The guest's widget framework, with the agent as its widget host.
 *
 * Each placed widget's view (an AppWidgetHostView, which shows the RemoteViews its app sends)
 * lives in a window of its own: a presentation on a private virtual display of the widget's
 * size, which only the agent can show anything on. Being in a window is what makes the
 * widget's views run, as a clock's ticking and a list's adapter need. The widget's image does
 * not come from that display, though: the agent records the view into a render node and draws
 * it with a hardware renderer of its own into an image reader ([Renderer]), or in software
 * when the GPU's image comes back with nothing in it. That drawing
 * does not depend on the guest's screen, so a widget keeps drawing while the guest's screen is
 * off and the device dozes, at the low rate a clock needs, without waking anything else.
 *
 * A widget is drawn again only when one of its views asks to be ([CaptureRoot]), and at most
 * as often as its [FrameGate] allows. A clock is drawn once a minute (once a second when it
 * shows seconds) even when nothing asks, so its time stays right should its own ticking stop.
 *
 * A click in a widget runs its PendingIntent on the guest's own display, where apps open as
 * task windows, not on the widget's private display.
 *
 * The framework hands a widget's updates only to a view its host made for that widget ID
 * ([AppWidgetHost.createView]); an update for an ID with no view is dropped with "null
 * listener". Those views live in the agent's process, so each start makes them again for
 * every placed widget, and a widget's view is made before anything else about its drawing,
 * so a window or display that fails to open never leaves the widget deaf. When the window
 * cannot be opened, the widget is drawn without one, in software ([Renderer]).
 */
class AndroidWidgets(private val context: Context, private val log: Log) : WidgetPlatform {
    private val manager = AppWidgetManager.getInstance(context)
    private val host = object : AppWidgetHost(context, HOST_ID) {
        override fun onCreateView(context: Context, appWidgetId: Int, appWidget: AppWidgetProviderInfo?): AppWidgetHostView =
            WidgetView(context) { renderers[appWidgetId]?.requestCapture() }
    }
    private val displays = context.getSystemService(DisplayManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private val encoder: Handler by lazy { Handler(HandlerThread("dsa-widget-frames").apply { start() }.looper) }
    private val renderers = ConcurrentHashMap<Int, Renderer>()

    /** Each widget's view, with the density it was made at. Only the main thread uses it. */
    private val views = HashMap<Int, Pair<Int, AppWidgetHostView>>()

    /** The widgets whose failure to draw was logged, so a retry does not log it again. */
    private val warned = HashSet<Int>()
    private var onFrame: (Int, Int, Int, ByteArray) -> Unit = { _, _, _, _ -> }
    private var onFailed: (Int) -> Unit = {}

    @Volatile
    private var dozing = false

    override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit, failed: (Int) -> Unit) {
        onFrame = frame
        onFailed = failed
        host.startListening()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) = changed()
        }
        context.registerReceiver(
            receiver,
            IntentFilter().apply {
                addAction(Intent.ACTION_PACKAGE_ADDED)
                addAction(Intent.ACTION_PACKAGE_REMOVED)
                addAction(Intent.ACTION_PACKAGE_CHANGED)
                addAction(Intent.ACTION_PACKAGE_REPLACED)
                addDataScheme("package")
            },
            Context.RECEIVER_NOT_EXPORTED,
        )
    }

    /**
     * The framework refuses every widget call ("User 0 must be unlocked for widgets to be
     * available") until the user's storage is unlocked, which comes after the agent starts.
     * The broadcast that says it has is the signal to start; the check after registering
     * covers an unlock that came in between.
     */
    override fun whenReady(ready: () -> Unit) {
        val users = context.getSystemService(UserManager::class.java)
        if (users == null || users.isUserUnlocked) {
            ready()
            return
        }
        val once = java.util.concurrent.atomic.AtomicBoolean(false)
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (!once.compareAndSet(false, true)) return
                try {
                    context.unregisterReceiver(this)
                } catch (ignored: IllegalArgumentException) {
                    // Unregistered already.
                }
                ready()
            }
        }
        context.registerReceiver(
            receiver,
            IntentFilter().apply {
                addAction(Intent.ACTION_USER_UNLOCKED)
                addAction(Intent.ACTION_BOOT_COMPLETED)
            },
            Context.RECEIVER_EXPORTED,
        )
        if (users.isUserUnlocked && once.compareAndSet(false, true)) {
            try {
                context.unregisterReceiver(receiver)
            } catch (ignored: IllegalArgumentException) {
                // Unregistered already.
            }
            ready()
        }
    }

    override fun providers(): List<WidgetInfo> =
        manager.installedProviders.map(::describe).sortedBy { it.label.lowercase() }

    override fun placedIds(): List<Int> = host.appWidgetIds.toList()

    override fun info(widgetId: Int): WidgetInfo? = manager.getAppWidgetInfo(widgetId)?.let(::describe)

    override fun allocate(): Int = host.allocateAppWidgetId()

    override fun bind(widgetId: Int, provider: String): Boolean {
        val component = ComponentName.unflattenFromString(provider) ?: return false
        return manager.bindAppWidgetIdIfAllowed(widgetId, component)
    }

    override fun delete(widgetId: Int) {
        main.post {
            renderers.remove(widgetId)?.close()
            views.remove(widgetId)
            warned.remove(widgetId)
        }
        host.deleteAppWidgetId(widgetId)
    }

    override fun configure(widgetId: Int, done: (Boolean) -> Unit) {
        WidgetConfigureActivity.start(context, widgetId, done)
    }

    override fun show(widgetId: Int, size: WidgetSize) {
        main.post {
            try {
                val old = renderers[widgetId]
                if (old != null && old.size == size) return@post
                renderers.remove(widgetId)
                old?.close()
                val info = manager.getAppWidgetInfo(widgetId)
                if (info == null) {
                    log.warn("widget $widgetId has no app; it is not drawn")
                    return@post
                }
                // The view first: from here on the widget's updates reach the agent.
                val view = viewOf(widgetId, info, size.densityDpi)
                renderers[widgetId] = Renderer(widgetId, size, view)
                if (warned.remove(widgetId)) log.info("widget $widgetId draws again")
            } catch (e: Exception) {
                renderers.remove(widgetId)?.close()
                if (warned.add(widgetId)) log.warn("drawing widget $widgetId failed; trying again", e)
                onFailed(widgetId)
            }
        }
    }

    /**
     * The widget's view, made and registered with the framework when it has none at
     * [densityDpi]. Making it again replaces the framework's listener for the widget ID.
     */
    private fun viewOf(widgetId: Int, info: AppWidgetProviderInfo, densityDpi: Int): AppWidgetHostView {
        views[widgetId]?.let { (density, view) ->
            if (density == densityDpi) {
                (view.parent as? ViewGroup)?.removeView(view)
                return view
            }
        }
        val view = host.createView(densityContext(densityDpi), widgetId, info)
        routeClicksToTheMainDisplay(view)
        view.setPadding(0, 0, 0, 0)
        views[widgetId] = densityDpi to view
        return view
    }

    /** The agent's context at [densityDpi], which a widget's layouts are inflated with. */
    private fun densityContext(densityDpi: Int): Context {
        val configuration = Configuration(context.resources.configuration)
        configuration.densityDpi = densityDpi
        return context.createConfigurationContext(configuration)
    }

    override fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float) {
        main.post { renderers[widgetId]?.pointer(action, x, y) }
    }

    override fun dozing(dozing: Boolean) {
        this.dozing = dozing
        main.post { for (renderer in renderers.values) renderer.gate.dozing = dozing }
    }

    /** A widget as a gallery shows it. The framework gives its sizes in this display's pixels. */
    private fun describe(info: AppWidgetProviderInfo): WidgetInfo {
        val density = context.resources.displayMetrics.density.takeIf { it > 0f } ?: 1f
        val dp = { px: Int -> ceil(px / density).toInt().coerceAtLeast(0) }
        val minWidth = dp(if (info.minResizeWidth > 0) minOf(info.minResizeWidth, info.minWidth) else info.minWidth)
        val minHeight = dp(if (info.minResizeHeight > 0) minOf(info.minResizeHeight, info.minHeight) else info.minHeight)
        // A launcher cell is about 74 dp with 16 dp between cells.
        val defaultWidth = if (info.targetCellWidth > 0) info.targetCellWidth * CELL_DP - GAP_DP else dp(info.minWidth)
        val defaultHeight = if (info.targetCellHeight > 0) info.targetCellHeight * CELL_DP - GAP_DP else dp(info.minHeight)
        val optional = info.widgetFeatures and AppWidgetProviderInfo.WIDGET_FEATURE_CONFIGURATION_OPTIONAL != 0
        return WidgetInfo(
            provider = info.provider.flattenToString(),
            packageName = info.provider.packageName,
            label = info.loadLabel(context.packageManager)?.toString().orEmpty(),
            previewPng = preview(info),
            minWidthDp = minWidth,
            minHeightDp = minHeight,
            defaultWidthDp = maxOf(defaultWidth, minWidth),
            defaultHeightDp = maxOf(defaultHeight, minHeight),
            maxWidthDp = dp(info.maxResizeWidth),
            maxHeightDp = dp(info.maxResizeHeight),
            resizeMode = info.resizeMode and (AppWidgetProviderInfo.RESIZE_HORIZONTAL or AppWidgetProviderInfo.RESIZE_VERTICAL),
            needsConfiguration = info.configure != null && !optional,
            appLabel = appLabel(info.provider.packageName),
        )
    }

    /** The name of the app [packageName], as the launcher shows it; empty when unknown. */
    private fun appLabel(packageName: String): String = try {
        val pm = context.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(packageName, 0)).toString()
    } catch (e: PackageManager.NameNotFoundException) {
        ""
    }

    /** The widget's preview, else its app's icon, as a PNG within the gallery's limits. */
    private fun preview(info: AppWidgetProviderInfo): ByteArray {
        val density = context.resources.displayMetrics.densityDpi
        val drawable: Drawable = try {
            info.loadPreviewImage(context, density) ?: info.loadIcon(context, density)
        } catch (e: Exception) {
            null
        } ?: return ByteArray(0)
        return try {
            val w = drawable.intrinsicWidth.takeIf { it > 0 } ?: WidgetManager.PREVIEW_PIXELS
            val h = drawable.intrinsicHeight.takeIf { it > 0 } ?: WidgetManager.PREVIEW_PIXELS
            val scale = minOf(1f, WidgetManager.PREVIEW_PIXELS.toFloat() / maxOf(w, h))
            val bw = (w * scale).toInt().coerceAtLeast(1)
            val bh = (h * scale).toInt().coerceAtLeast(1)
            val bitmap = Bitmap.createBitmap(bw, bh, Bitmap.Config.ARGB_8888)
            drawable.setBounds(0, 0, bw, bh)
            drawable.draw(Canvas(bitmap))
            val png = ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
            bitmap.recycle()
            if (png.size <= WidgetManager.MAX_PREVIEW_BYTES) png else ByteArray(0)
        } catch (e: Exception) {
            log.warn("drawing the preview of ${info.provider.flattenToShortString()} failed", e)
            ByteArray(0)
        }
    }

    /**
     * The root of a widget's window: it hears when any view in it needs drawing again.
     *
     * FrameLayout's constructor calls requestLayout() before this class's own fields are set,
     * so the callback is read through a field that is still null at that point.
     */
    internal class CaptureRoot(context: Context, invalidated: () -> Unit) : FrameLayout(context) {
        private val listener: (() -> Unit)? = invalidated

        override fun onDescendantInvalidated(child: View, target: View) {
            super.onDescendantInvalidated(child, target)
            listener?.invoke()
        }

        override fun invalidate() {
            super.invalidate()
            listener?.invoke()
        }

        override fun requestLayout() {
            super.requestLayout()
            listener?.invoke()
        }
    }

    /**
     * A widget's view as the agent makes it: it says when its app sent new views, which is
     * the one sign of an update that reaches a view with no window. Its callback is read the
     * same guarded way, in case the view's constructor reaches an override first.
     */
    internal class WidgetView(context: Context, updated: () -> Unit) : AppWidgetHostView(context) {
        private val listener: (() -> Unit)? = updated

        override fun updateAppWidget(remoteViews: RemoteViews?) {
            super.updateAppWidget(remoteViews)
            listener?.invoke()
        }

        /**
         * What the view shows when the app's views cannot be shown, as for a while after
         * the app is stopped: nothing, rather than the framework's error card, so the host
         * keeps the widget's last image until the app draws again.
         */
        override fun getErrorView(): View = View(context)
    }

    /** The window a widget's views run in: a presentation on a private display of its size. */
    private class WidgetWindow(
        private val presentation: Presentation,
        private val display: VirtualDisplay,
        private val reader: ImageReader,
    ) {
        fun close(log: Log, id: Int) {
            try {
                presentation.dismiss()
            } catch (e: Exception) {
                log.warn("closing the window of widget $id failed", e)
            }
            display.release()
            reader.close()
        }
    }

    /** Opens [root] in a window of [size]; null, with the reason logged, when it cannot be. */
    private fun openWindow(id: Int, size: WidgetSize, root: View): WidgetWindow? {
        var reader: ImageReader? = null
        var display: VirtualDisplay? = null
        var presentation: Presentation? = null
        return try {
            reader = ImageReader.newInstance(size.width, size.height, PixelFormat.RGBA_8888, 2).also {
                // The window's own drawing is not used; its buffers are given straight back.
                it.setOnImageAvailableListener({ r -> r.acquireLatestImage()?.close() }, main)
            }
            display = displays.createVirtualDisplay("dsa-widget-$id", size.width, size.height, size.densityDpi, reader.surface, 0)
                ?: error("Android made no display for the widget")
            presentation = Presentation(context, display.display)
            presentation.window?.setBackgroundDrawable(ColorDrawable(Color.TRANSPARENT))
            presentation.setContentView(root)
            presentation.show()
            WidgetWindow(presentation, display, reader)
        } catch (e: Exception) {
            try {
                presentation?.dismiss()
            } catch (ignored: Exception) {
                // The window is closed below in any case.
            }
            (root.parent as? ViewGroup)?.removeView(root)
            display?.release()
            reader?.close()
            log.warn("the window of widget $id did not open; drawing it without one, in software", e)
            null
        }
    }

    /**
     * One widget's drawing, of the [view] already registered for it. Lives on the main thread.
     * The view runs in a window of its own when one opens; without one it is drawn in
     * software, again each time its app sends new views, and a clock at each tick.
     */
    private inner class Renderer(val id: Int, val size: WidgetSize, private val view: AppWidgetHostView) {
        val gate = FrameGate().also { it.dozing = dozing }
        private val width = size.width
        private val height = size.height
        private val root = CaptureRoot(densityContext(size.densityDpi)) { requestCapture() }.also {
            it.addView(view, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        }
        private val captureReader = ImageReader.newInstance(
            width,
            height,
            PixelFormat.RGBA_8888,
            2,
            HardwareBuffer.USAGE_CPU_READ_OFTEN or HardwareBuffer.USAGE_GPU_COLOR_OUTPUT or HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE,
        )
        private val node = RenderNode("dsa-widget-$id").also { it.setPosition(0, 0, width, height) }
        private val renderer = HardwareRenderer().also {
            // A hardware renderer draws opaque unless told otherwise, and then the alpha it
            // leaves in the buffer means nothing: the host would show the widget see-through.
            it.isOpaque = false
            it.setSurface(captureReader.surface)
            it.setContentRoot(node)
        }

        // Opened last, so nothing after it can fail and leave the window open.
        private val window: WidgetWindow? = openWindow(id, size, root)

        /**
         * Whether this widget is drawn without the GPU: set when the GPU's image of it came
         * back with nothing in it, which a widget never draws.
         */
        @Volatile
        private var software = window == null
        private var captureScheduled = false
        private var tickScheduled = false
        private var closed = false
        private var downTime = 0L

        init {
            val sizes = Bundle()
            view.updateAppWidgetSize(sizes, listOf(SizeF(size.widthDp.toFloat(), size.heightDp.toFloat())))
            captureReader.setOnImageAvailableListener({ reader ->
                val image = reader.acquireLatestImage() ?: return@setOnImageAvailableListener
                val pixels = try {
                    readBack(image)
                } catch (e: Exception) {
                    log.warn("reading the image of widget $id failed", e)
                    null
                } finally {
                    image.close()
                }
                if (pixels == null) return@setOnImageAvailableListener
                if (isClear(pixels)) {
                    pixels.recycle()
                    // Either the widget shows nothing yet (its app has not sent its views),
                    // or the GPU's drawing did not reach this image. A drawing in software
                    // tells which; only the second makes the widget draw in software.
                    main.post { checkClearImage() }
                    return@setOnImageAvailableListener
                }
                send(pixels)
            }, encoder)
            requestCapture()
        }

        /**
         * The GPU's image, read back through the GPU: the image wrapped as a hardware bitmap
         * and copied, which the renderer does with a GPU read. Mapping the buffer for the
         * CPU instead shows nothing on a guest whose graphics driver keeps the buffer's
         * contents on the host, and the widget then looked empty.
         */
        private fun readBack(image: android.media.Image): Bitmap? {
            val buffer = image.hardwareBuffer
            if (buffer != null) {
                try {
                    val wrapped = Bitmap.wrapHardwareBuffer(buffer, null)
                    if (wrapped != null) {
                        val copy = wrapped.copy(Bitmap.Config.ARGB_8888, false)
                        wrapped.recycle()
                        if (copy != null) return copy
                    }
                } finally {
                    buffer.close()
                }
            }
            val plane = image.planes[0]
            val stride = plane.rowStride / plane.pixelStride
            val full = Bitmap.createBitmap(stride, height, Bitmap.Config.ARGB_8888)
            full.copyPixelsFromBuffer(plane.buffer)
            if (stride == width) return full
            return Bitmap.createBitmap(full, 0, 0, width, height).also { full.recycle() }
        }

        /** The GPU's image came back clear: a drawing in software says whether it should have. */
        private fun checkClearImage() {
            if (closed || software) return
            val drawn = drawInSoftware() ?: return
            if (isClear(drawn)) {
                // The widget shows nothing yet; the host keeps what it showed before.
                drawn.recycle()
                return
            }
            software = true
            log.warn("the GPU's image of widget $id came back empty though the widget shows something; drawing it in software from now on")
            encoder.post { send(drawn) }
        }

        /** Sends [pixels] as the widget's image, once, and frees them. */
        private fun send(pixels: Bitmap) {
            val png = try {
                encode(pixels)
            } catch (e: Exception) {
                log.warn("encoding the image of widget $id failed", e)
                null
            } finally {
                pixels.recycle()
            }
            if (png != null) onFrame(id, width, height, png)
        }

        fun requestCapture() {
            if (closed || captureScheduled) return
            captureScheduled = true
            main.postDelayed({
                captureScheduled = false
                capture()
            }, gate.delay(SystemClock.uptimeMillis()))
        }

        private fun capture() {
            if (closed) return
            gate.taken(SystemClock.uptimeMillis())
            if (software) {
                layOut()
                captureInSoftware()
                scheduleTick()
                return
            }
            try {
                layOut()
                val canvas = node.beginRecording(width, height)
                try {
                    canvas.drawColor(Color.TRANSPARENT, PorterDuff.Mode.CLEAR)
                    root.draw(canvas)
                } finally {
                    node.endRecording()
                }
                renderer.createRenderRequest().syncAndDraw()
            } catch (e: Exception) {
                log.warn("drawing widget $id with the GPU failed; drawing it in software", e)
                captureInSoftware()
            }
            scheduleTick()
        }

        private fun layOut() {
            if (root.isLayoutRequested || root.width != width || root.height != height) {
                root.measure(
                    View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY),
                    View.MeasureSpec.makeMeasureSpec(height, View.MeasureSpec.EXACTLY),
                )
                root.layout(0, 0, width, height)
            }
        }

        /** Draws the widget without the GPU, for a view the hardware renderer cannot take. */
        private fun captureInSoftware() {
            val bitmap = drawInSoftware() ?: return
            if (isClear(bitmap)) {
                // Nothing to show yet, or the app's views went: the host keeps the last image.
                bitmap.recycle()
                return
            }
            encoder.post { send(bitmap) }
        }

        /**
         * The widget drawn in software onto a bitmap with an alpha channel that starts fully
         * transparent, so what the widget leaves undrawn stays see-through on the desktop.
         */
        private fun drawInSoftware(): Bitmap? = try {
            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            bitmap.setHasAlpha(true)
            bitmap.eraseColor(Color.TRANSPARENT)
            root.draw(Canvas(bitmap))
            bitmap
        } catch (e: Exception) {
            log.warn("drawing widget $id failed", e)
            null
        }

        /** Draws a clock again at its next second or minute, whatever its own ticking does. */
        private fun scheduleTick() {
            if (tickScheduled || closed) return
            val clocks = mutableListOf<View>()
            collectClocks(root, clocks)
            if (clocks.isEmpty()) return
            val seconds = clocks.any { clock ->
                clock is TextClock && (clock.format24Hour?.contains('s') == true || clock.format12Hour?.contains('s') == true)
            }
            tickScheduled = true
            main.postDelayed({
                tickScheduled = false
                if (closed) return@postDelayed
                for (clock in clocks) refresh(clock)
                requestCapture()
            }, FrameGate.untilNextTick(System.currentTimeMillis(), seconds))
        }

        fun pointer(action: PointerAction, x: Float, y: Float) {
            val now = SystemClock.uptimeMillis()
            val motion = when (action) {
                PointerAction.POINTER_ACTION_DOWN -> MotionEvent.ACTION_DOWN.also { downTime = now }
                PointerAction.POINTER_ACTION_MOVE -> MotionEvent.ACTION_MOVE
                PointerAction.POINTER_ACTION_UP -> MotionEvent.ACTION_UP
                PointerAction.POINTER_ACTION_CANCEL -> MotionEvent.ACTION_CANCEL
                else -> return
            }
            val event = MotionEvent.obtain(downTime, now, motion, x, y, 0)
            event.source = InputDevice.SOURCE_TOUCHSCREEN
            try {
                root.dispatchTouchEvent(event)
            } finally {
                event.recycle()
            }
        }

        fun close() {
            closed = true
            window?.close(log, id)
            root.removeAllViews()
            renderer.destroy()
            encoder.post { captureReader.close() }
        }
    }

    /** Whether [bitmap] shows nothing: every pixel's alpha is zero. */
    private fun isClear(bitmap: Bitmap): Boolean {
        val buffer = java.nio.ByteBuffer.allocate(bitmap.rowBytes * bitmap.height)
        bitmap.copyPixelsToBuffer(buffer)
        buffer.rewind()
        return allClear(buffer, bitmap.width, bitmap.height, bitmap.rowBytes)
    }

    private fun encode(bitmap: Bitmap): ByteArray =
        ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()

    private fun collectClocks(view: View, into: MutableList<View>) {
        if (view is TextClock || view is AnalogClock) into += view
        if (view is ViewGroup) for (i in 0 until view.childCount) collectClocks(view.getChildAt(i), into)
    }

    /** Makes a clock read the time now: setting its format or time zone again does that. */
    private fun refresh(clock: View) {
        try {
            when (clock) {
                is TextClock -> {
                    clock.format12Hour = clock.format12Hour
                    clock.format24Hour = clock.format24Hour
                }
                is AnalogClock -> clock.timeZone = clock.timeZone
            }
        } catch (e: Exception) {
            log.warn("refreshing a clock in a widget failed", e)
        }
    }

    /**
     * A widget's click runs its PendingIntent with the launch options of the view it is in,
     * which name the widget's private display. The agent's own handler opens it on the
     * guest's display instead, so the app shows as a task window. The handler interface is
     * not in the SDK; when it cannot be set, the framework's own handler stays.
     */
    private fun routeClicksToTheMainDisplay(view: AppWidgetHostView) {
        try {
            val handlerType = Class.forName("android.widget.RemoteViews\$InteractionHandler")
            val handler = java.lang.reflect.Proxy.newProxyInstance(handlerType.classLoader, arrayOf(handlerType)) { proxy, method, args ->
                when (method.name) {
                    "onInteraction" -> {
                        val clicked = args[0] as View
                        val pending = args[1] as PendingIntent
                        val response = args[2]
                        launch(clicked, pending, response)
                    }
                    "hashCode" -> System.identityHashCode(proxy)
                    "equals" -> proxy === args?.get(0)
                    "toString" -> "dsa widget click handler"
                    else -> null
                }
            }
            AppWidgetHostView::class.java.getMethod("setInteractionHandler", handlerType).invoke(view, handler)
        } catch (e: ReflectiveOperationException) {
            log.warn("cannot route widget clicks to the guest's display; an app a widget opens may not show", e)
        }
    }

    private fun launch(clicked: View, pending: PendingIntent, response: Any?): Boolean = try {
        val pair = response?.javaClass?.getMethod("getLaunchOptions", View::class.java)?.invoke(response, clicked)
            as? android.util.Pair<*, *>
        val fillIn = pair?.first as? Intent
        val options = (pair?.second as? ActivityOptions) ?: ActivityOptions.makeBasic()
        options.launchDisplayId = Display.DEFAULT_DISPLAY
        options.setPendingIntentBackgroundActivityStartMode(ActivityOptions.MODE_BACKGROUND_ACTIVITY_START_ALLOWED)
        pending.send(context, 0, fillIn, null, null, null, options.toBundle())
        true
    } catch (e: Exception) {
        log.warn("running a widget's click failed", e)
        true
    }

    private companion object {
        const val HOST_ID = WIDGET_HOST_ID
        const val CELL_DP = 74
        const val GAP_DP = 16
    }
}

/** The agent's widget host ID; the framework keeps the agent's widgets under it. */
internal const val WIDGET_HOST_ID = 0x0D5A

/** The size each widget was last drawn at, in the agent's preferences. */
class AndroidWidgetStore(context: Context) : WidgetStore {
    private val preferences = context.getSharedPreferences("widgets", Context.MODE_PRIVATE)

    override fun sizes(): Map<Int, WidgetSize> = preferences.all.mapNotNull { (key, value) ->
        val id = key.toIntOrNull() ?: return@mapNotNull null
        val parts = (value as? String)?.split(',')?.mapNotNull(String::toIntOrNull) ?: return@mapNotNull null
        if (parts.size != 5) return@mapNotNull null
        id to WidgetSize(parts[0], parts[1], parts[2], parts[3], parts[4])
    }.toMap()

    override fun save(widgetId: Int, size: WidgetSize) {
        preferences.edit()
            .putString(widgetId.toString(), "${size.width},${size.height},${size.densityDpi},${size.widthDp},${size.heightDp}")
            .apply()
    }

    override fun remove(widgetId: Int) {
        preferences.edit().remove(widgetId.toString()).apply()
    }
}

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
import android.util.SizeF
import android.view.Display
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.widget.AnalogClock
import android.widget.FrameLayout
import android.widget.TextClock
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.widgets.FrameGate
import io.github.thisisthepy.dsa.agent.widgets.WidgetInfo
import io.github.thisisthepy.dsa.agent.widgets.WidgetManager
import io.github.thisisthepy.dsa.agent.widgets.WidgetPlatform
import io.github.thisisthepy.dsa.agent.widgets.WidgetSize
import io.github.thisisthepy.dsa.agent.widgets.WidgetStore
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
 * it with a hardware renderer of its own into an image reader ([Renderer]). That drawing
 * does not depend on the guest's screen, so a widget keeps drawing while the guest's screen is
 * off and the device dozes, at the low rate a clock needs, without waking anything else.
 *
 * A widget is drawn again only when one of its views asks to be ([CaptureRoot]), and at most
 * as often as its [FrameGate] allows. A clock is drawn once a minute (once a second when it
 * shows seconds) even when nothing asks, so its time stays right should its own ticking stop.
 *
 * A click in a widget runs its PendingIntent on the guest's own display, where apps open as
 * task windows, not on the widget's private display.
 */
class AndroidWidgets(private val context: Context, private val log: Log) : WidgetPlatform {
    private val manager = AppWidgetManager.getInstance(context)
    private val host = AppWidgetHost(context, HOST_ID)
    private val displays = context.getSystemService(DisplayManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private val encoder: Handler by lazy { Handler(HandlerThread("dsa-widget-frames").apply { start() }.looper) }
    private val renderers = ConcurrentHashMap<Int, Renderer>()
    private var onFrame: (Int, Int, Int, ByteArray) -> Unit = { _, _, _, _ -> }

    @Volatile
    private var dozing = false

    override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit) {
        onFrame = frame
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
        main.post { renderers.remove(widgetId)?.close() }
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
                old?.close()
                val info = manager.getAppWidgetInfo(widgetId)
                if (info == null) {
                    renderers.remove(widgetId)
                    log.warn("widget $widgetId has no app; it is not drawn")
                    return@post
                }
                renderers[widgetId] = Renderer(widgetId, info, size)
            } catch (e: Exception) {
                renderers.remove(widgetId)
                log.warn("drawing widget $widgetId failed", e)
            }
        }
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

    /** The root of a widget's window: it hears when any view in it needs drawing again. */
    private class CaptureRoot(context: Context, private val invalidated: () -> Unit) : FrameLayout(context) {
        override fun onDescendantInvalidated(child: View, target: View) {
            super.onDescendantInvalidated(child, target)
            invalidated()
        }

        override fun invalidate() {
            super.invalidate()
            invalidated()
        }

        override fun requestLayout() {
            super.requestLayout()
            invalidated()
        }
    }

    /** One widget's window and drawing. Lives on the main thread. */
    private inner class Renderer(val id: Int, info: AppWidgetProviderInfo, val size: WidgetSize) {
        val gate = FrameGate().also { it.dozing = dozing }
        private val width = size.width
        private val height = size.height
        private val displayReader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 2).also { reader ->
            // The window's own drawing is not used; its buffers are given straight back.
            reader.setOnImageAvailableListener({ it.acquireLatestImage()?.close() }, main)
        }
        private val display: VirtualDisplay = displays.createVirtualDisplay(
            "dsa-widget-$id",
            width,
            height,
            size.densityDpi,
            displayReader.surface,
            0,
        )
        private val presentation = Presentation(context, display.display)
        private val root = CaptureRoot(presentation.context) { requestCapture() }
        private val view: AppWidgetHostView = host.createView(presentation.context, id, info)
        private val captureReader = ImageReader.newInstance(
            width,
            height,
            PixelFormat.RGBA_8888,
            2,
            HardwareBuffer.USAGE_CPU_READ_OFTEN or HardwareBuffer.USAGE_GPU_COLOR_OUTPUT,
        )
        private val node = RenderNode("dsa-widget-$id").also { it.setPosition(0, 0, width, height) }
        private val renderer = HardwareRenderer().also {
            it.setSurface(captureReader.surface)
            it.setContentRoot(node)
        }
        private var captureScheduled = false
        private var tickScheduled = false
        private var closed = false
        private var downTime = 0L

        init {
            routeClicksToTheMainDisplay(view)
            view.setPadding(0, 0, 0, 0)
            root.addView(view, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            presentation.window?.setBackgroundDrawable(ColorDrawable(Color.TRANSPARENT))
            presentation.setContentView(root)
            presentation.show()
            val sizes = Bundle()
            view.updateAppWidgetSize(sizes, listOf(SizeF(size.widthDp.toFloat(), size.heightDp.toFloat())))
            captureReader.setOnImageAvailableListener({ reader ->
                val image = reader.acquireLatestImage() ?: return@setOnImageAvailableListener
                val png = try {
                    val plane = image.planes[0]
                    val stride = plane.rowStride / plane.pixelStride
                    val full = Bitmap.createBitmap(stride, height, Bitmap.Config.ARGB_8888)
                    full.copyPixelsFromBuffer(plane.buffer)
                    val cropped = if (stride == width) full else Bitmap.createBitmap(full, 0, 0, width, height)
                    encode(cropped).also {
                        if (cropped !== full) cropped.recycle()
                        full.recycle()
                    }
                } catch (e: Exception) {
                    log.warn("reading the image of widget $id failed", e)
                    null
                } finally {
                    image.close()
                }
                if (png != null) onFrame(id, width, height, png)
            }, encoder)
            requestCapture()
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
            try {
                if (root.isLayoutRequested || root.width != width || root.height != height) {
                    root.measure(
                        View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY),
                        View.MeasureSpec.makeMeasureSpec(height, View.MeasureSpec.EXACTLY),
                    )
                    root.layout(0, 0, width, height)
                }
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

        /** Draws the widget without the GPU, for a view the hardware renderer cannot take. */
        private fun captureInSoftware() {
            try {
                val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                root.draw(Canvas(bitmap))
                val png = encode(bitmap)
                bitmap.recycle()
                encoder.post { onFrame(id, width, height, png) }
            } catch (e: Exception) {
                log.warn("drawing widget $id failed", e)
            }
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
            try {
                presentation.dismiss()
            } catch (e: Exception) {
                log.warn("closing the window of widget $id failed", e)
            }
            renderer.destroy()
            display.release()
            displayReader.close()
            encoder.post { captureReader.close() }
        }
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

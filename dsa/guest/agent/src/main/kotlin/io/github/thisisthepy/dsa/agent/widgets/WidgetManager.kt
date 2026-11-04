package io.github.thisisthepy.dsa.agent.widgets

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.WidgetAdded
import io.github.thisisthepy.dsa.control.v1.WidgetFrame
import io.github.thisisthepy.dsa.control.v1.WidgetList
import io.github.thisisthepy.dsa.control.v1.WidgetProvider
import io.github.thisisthepy.dsa.control.v1.WidgetProviders
import io.github.thisisthepy.dsa.control.v1.WidgetUpdated

/** A widget an app offers, as the agent reads it. Sizes are in dp. */
data class WidgetInfo(
    /** The provider's component, as `package/class`. */
    val provider: String,
    val packageName: String,
    val label: String,
    /** A PNG image; empty for none. */
    val previewPng: ByteArray,
    val minWidthDp: Int,
    val minHeightDp: Int,
    val defaultWidthDp: Int,
    val defaultHeightDp: Int,
    /** 0 for no limit. */
    val maxWidthDp: Int,
    val maxHeightDp: Int,
    /** 1 horizontally, 2 vertically. */
    val resizeMode: Int,
    val needsConfiguration: Boolean,
    /** The name of the app that offers the widget, as the launcher shows it. */
    val appLabel: String = "",
) {
    override fun equals(other: Any?): Boolean =
        other is WidgetInfo && provider == other.provider && packageName == other.packageName &&
            label == other.label && previewPng.contentEquals(other.previewPng) &&
            minWidthDp == other.minWidthDp && minHeightDp == other.minHeightDp &&
            defaultWidthDp == other.defaultWidthDp && defaultHeightDp == other.defaultHeightDp &&
            maxWidthDp == other.maxWidthDp && maxHeightDp == other.maxHeightDp &&
            resizeMode == other.resizeMode && needsConfiguration == other.needsConfiguration &&
            appLabel == other.appLabel

    override fun hashCode(): Int = provider.hashCode()

    fun toProto(): WidgetProvider = WidgetProvider.newBuilder()
        .setProvider(provider)
        .setPackageName(packageName)
        .setLabel(label)
        .setPreviewPng(
            if (previewPng.size <= WidgetManager.MAX_PREVIEW_BYTES) ByteString.copyFrom(previewPng) else ByteString.EMPTY,
        )
        .setMinWidthDp(minWidthDp.coerceAtLeast(0))
        .setMinHeightDp(minHeightDp.coerceAtLeast(0))
        .setDefaultWidthDp(defaultWidthDp.coerceAtLeast(0))
        .setDefaultHeightDp(defaultHeightDp.coerceAtLeast(0))
        .setMaxWidthDp(maxWidthDp.coerceAtLeast(0))
        .setMaxHeightDp(maxHeightDp.coerceAtLeast(0))
        .setResizeMode(resizeMode and 3)
        .setNeedsConfiguration(needsConfiguration)
        .setAppLabel(appLabel)
        .build()
}

/** The size a widget is drawn at: pixels at a density, and the same size in dp. */
data class WidgetSize(val width: Int, val height: Int, val densityDpi: Int, val widthDp: Int, val heightDp: Int) {
    companion object {
        /** A widget's size before its window says, at a density of 160: its default size. */
        fun defaultOf(info: WidgetInfo): WidgetSize {
            val width = info.defaultWidthDp.takeIf { it > 0 } ?: info.minWidthDp.takeIf { it > 0 } ?: 110
            val height = info.defaultHeightDp.takeIf { it > 0 } ?: info.minHeightDp.takeIf { it > 0 } ?: 110
            return WidgetSize(width, height, 160, width, height)
        }
    }
}

/** The guest's widget framework, as the agent drives it. Every method runs on the agent's thread. */
interface WidgetPlatform {
    /** The widgets the installed apps offer. */
    fun providers(): List<WidgetInfo>

    /** The widget IDs the agent holds, which outlive its process and the guest's restarts. */
    fun placedIds(): List<Int>

    /** The widget [widgetId] is bound to, or null when its app is gone. */
    fun info(widgetId: Int): WidgetInfo?

    /** A new widget ID. */
    fun allocate(): Int

    /** Binds [widgetId] to [provider]. False when the framework does not allow it. */
    fun bind(widgetId: Int, provider: String): Boolean

    /** Forgets [widgetId] and stops drawing it. */
    fun delete(widgetId: Int)

    /**
     * Runs the provider's configuration activity for [widgetId]. [done] is called once, on any
     * thread, with whether the user finished it.
     */
    fun configure(widgetId: Int, done: (Boolean) -> Unit)

    /** Draws the widget at [size] from now on, and tells its app the size. */
    fun show(widgetId: Int, size: WidgetSize)

    /** Hands pointer input to the widget's view. */
    fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float)

    /** Whether the guest is in Doze, when the widgets draw at their lowest rate. */
    fun dozing(dozing: Boolean)

    /**
     * Starts the platform: [frame] is called, on any thread, with each new image of a widget as
     * a PNG; [changed] when the installed widgets may have changed, as when an app is removed.
     */
    fun start(frame: (widgetId: Int, width: Int, height: Int, png: ByteArray) -> Unit, changed: () -> Unit)

    companion object {
        val NONE = object : WidgetPlatform {
            override fun providers() = emptyList<WidgetInfo>()
            override fun placedIds() = emptyList<Int>()
            override fun info(widgetId: Int): WidgetInfo? = null
            override fun allocate() = error("this agent hosts no widgets")
            override fun bind(widgetId: Int, provider: String) = false
            override fun delete(widgetId: Int) = Unit
            override fun configure(widgetId: Int, done: (Boolean) -> Unit) = done(false)
            override fun show(widgetId: Int, size: WidgetSize) = Unit
            override fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float) = Unit
            override fun dozing(dozing: Boolean) = Unit
            override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit) = Unit
        }
    }
}

/** Where the agent keeps the size each widget was last drawn at, across its restarts. */
interface WidgetStore {
    fun sizes(): Map<Int, WidgetSize>

    fun save(widgetId: Int, size: WidgetSize)

    fun remove(widgetId: Int)

    companion object {
        /** Keeps them in memory only. */
        fun memory(): WidgetStore = object : WidgetStore {
            private val kept = mutableMapOf<Int, WidgetSize>()
            override fun sizes() = kept.toMap()
            override fun save(widgetId: Int, size: WidgetSize) {
                kept[widgetId] = size
            }

            override fun remove(widgetId: Int) {
                kept.remove(widgetId)
            }
        }
    }
}

/**
 * How often a widget's image is taken. A widget is drawn again only when something in it
 * changed, and at most every [activeMillis]; while the guest is in Doze at most every
 * [dozeMillis], which is enough for a clock and keeps the guest's CPU idle otherwise.
 */
class FrameGate(private val activeMillis: Long = 100, private val dozeMillis: Long = 1000) {
    private var last = Long.MIN_VALUE / 2

    var dozing = false

    /** How long to wait at [now] before the next image may be taken. */
    fun delay(now: Long): Long = (last + (if (dozing) dozeMillis else activeMillis) - now).coerceAtLeast(0)

    /** An image was taken at [now]. */
    fun taken(now: Long) {
        last = now
    }

    companion object {
        /**
         * How long until a clock that shows seconds, or one that shows minutes, needs drawing
         * again at [nowMillis] of wall time: to the next second or the next minute.
         */
        fun untilNextTick(nowMillis: Long, seconds: Boolean): Long {
            val period = if (seconds) 1000L else 60_000L
            return period - Math.floorMod(nowMillis, period)
        }
    }
}

/**
 * The widgets the user placed on the host's desktop, as the agent hosts them.
 *
 * The agent is the guest's widget host: it allocates and binds each widget's ID, runs the
 * widget's setup when its app asks for one, and draws each widget at the size of its host
 * window ([WidgetPlatform]). It tells the host of each placed widget on every connection, then
 * which are placed ([WidgetList]), so a widget whose app was removed goes. Each widget's images
 * go to the host as PNG frames, only when they change; the host's pointer input goes to the
 * widget's view, so its click actions run as on a phone.
 *
 * Every method runs on the agent's one thread; [frame] and [changed] may be called from any
 * thread and post to it.
 */
class WidgetManager(
    private val platform: WidgetPlatform,
    private val store: WidgetStore,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val isConnected: () -> Boolean,
) {
    private var post: (() -> Unit) -> Unit = { it() }
    private val shown = mutableMapOf<Int, WidgetSize>()
    private val lastFrame = mutableMapOf<Int, ByteArray>()

    /** Whether the host has heard which widgets are placed since the agent started. */
    private var listed = false

    /** Whether the widget host is running, so the placed widgets can be read. */
    private var started = false

    /** Whether a start was refused and is being tried again. */
    private var waiting = false

    /**
     * Starts drawing the placed widgets; [post] runs work on the agent's thread. The agent
     * starts before the user's storage is unlocked, and Android keeps widgets until then, so
     * a refused start runs again through [later] until it succeeds. Until it has, the host
     * hears nothing about widgets: an empty list would make it remove the ones placed.
     */
    fun start(later: (() -> Unit) -> Unit = {}, post: (() -> Unit) -> Unit) {
        this.post = post
        try {
            platform.start(
                frame = { id, width, height, png -> post { frame(id, width, height, png) } },
                changed = { post { if (isConnected()) announce() else prune() } },
            )
        } catch (e: Exception) {
            if (!waiting) log.warn("the widget host did not start yet; trying again until it does", e)
            waiting = true
            later { start(later, post) }
            return
        }
        started = true
        if (waiting) log.info("the widget host started")
        prune()
        val sizes = store.sizes()
        for (id in placed()) {
            val size = sizes[id] ?: platform.info(id)?.let(WidgetSize::defaultOf) ?: continue
            show(id, size)
        }
        if (waiting && isConnected()) announce()
        waiting = false
    }

    private fun placed(): List<Int> = try {
        platform.placedIds().sorted()
    } catch (e: Exception) {
        log.warn("reading the placed widgets failed", e)
        emptyList()
    }

    /** Forgets the widgets whose app is gone. */
    private fun prune(): List<Pair<Int, WidgetInfo>> {
        val alive = mutableListOf<Pair<Int, WidgetInfo>>()
        for (id in placed()) {
            val info = try {
                platform.info(id)
            } catch (e: Exception) {
                log.warn("reading widget $id failed", e)
                continue
            }
            if (info == null) {
                log.info("widget $id: its app is gone; removing it")
                forget(id)
            } else {
                alive += id to info
            }
        }
        return alive
    }

    private fun forget(id: Int) {
        attempt("deleting widget $id") { platform.delete(id) }
        store.remove(id)
        shown.remove(id)
        lastFrame.remove(id)
    }

    /** The host connected: it hears of every placed widget and what each shows now. */
    fun connected() {
        lastFrame.clear()
        if (started) announce()
    }

    private fun announce() {
        val before = placed().size
        val alive = prune()
        for ((id, info) in alive) {
            send(updated(id, info))
            if (id !in shown) show(id, store.sizes()[id] ?: WidgetSize.defaultOf(info))
        }
        // A guest that never had a widget says nothing about widgets.
        if (alive.isNotEmpty() || before > 0 || listed) sendList()
    }

    private fun updated(id: Int, info: WidgetInfo) = Envelope.newBuilder()
        .setWidgetUpdated(WidgetUpdated.newBuilder().setWidgetId(id).setProvider(info.toProto()))
        .build().toByteArray()

    private fun sendList() {
        listed = true
        send(
            Envelope.newBuilder()
                .setWidgetList(WidgetList.newBuilder().addAllWidgetIds(placed()))
                .build().toByteArray(),
        )
    }

    private fun show(id: Int, size: WidgetSize) {
        shown[id] = size
        attempt("drawing widget $id") { platform.show(id, size) }
    }

    /** Acts on the frame if it is about widgets. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.LIST_WIDGET_PROVIDERS -> listProviders(envelope.listWidgetProviders.requestId)
            Envelope.BodyCase.ADD_WIDGET -> add(envelope.addWidget.requestId, envelope.addWidget.provider)
            Envelope.BodyCase.REMOVE_WIDGET -> remove(envelope.removeWidget.widgetId)
            Envelope.BodyCase.RESIZE_WIDGET -> resize(envelope.resizeWidget)
            Envelope.BodyCase.WIDGET_POINTER -> pointer(envelope.widgetPointer)
            else -> return false
        }
        return true
    }

    /** Whether the guest's screen is on; while it is off the widgets draw at their lowest rate. */
    fun screen(on: Boolean) {
        attempt("telling the widgets the guest dozes") { platform.dozing(!on) }
    }

    private fun listProviders(requestId: Int) {
        val providers = try {
            platform.providers()
        } catch (e: Exception) {
            log.warn("reading the installed widgets failed", e)
            emptyList()
        }.take(MAX_PROVIDERS)
        var batch = WidgetProviders.newBuilder().setRequestId(requestId)
        var bytes = 0
        for (info in providers) {
            val proto = info.toProto()
            if (batch.providersCount > 0 && bytes + proto.serializedSize > MAX_BATCH_BYTES) {
                send(Envelope.newBuilder().setWidgetProviders(batch).build().toByteArray())
                batch = WidgetProviders.newBuilder().setRequestId(requestId)
                bytes = 0
            }
            batch.addProviders(proto)
            bytes += proto.serializedSize
        }
        send(Envelope.newBuilder().setWidgetProviders(batch.setComplete(true)).build().toByteArray())
    }

    private fun add(requestId: Int, provider: String) {
        val refuse = { why: String ->
            log.warn("widget $provider not placed: $why")
            send(
                Envelope.newBuilder()
                    .setWidgetAdded(WidgetAdded.newBuilder().setRequestId(requestId).setError(why))
                    .build().toByteArray(),
            )
            Unit
        }
        if (placed().size >= MAX_WIDGETS) return refuse("$MAX_WIDGETS widgets are placed already; remove one first")
        val offered = try {
            platform.providers().firstOrNull { it.provider == provider }
        } catch (e: Exception) {
            log.warn("reading the installed widgets failed", e)
            null
        } ?: return refuse("no app in Android offers the widget $provider")
        val id = try {
            platform.allocate()
        } catch (e: Exception) {
            log.warn("allocating a widget ID failed", e)
            return refuse("Android could not make room for another widget")
        }
        val bound = try {
            platform.bind(id, provider)
        } catch (e: Exception) {
            log.warn("binding widget $id failed", e)
            false
        }
        if (!bound) {
            forget(id)
            return refuse("Android did not allow the widget to be placed")
        }
        val placedNow = {
            val info = platform.info(id) ?: offered
            val size = WidgetSize.defaultOf(info)
            store.save(id, size)
            show(id, size)
            send(updated(id, info))
            send(
                Envelope.newBuilder()
                    .setWidgetAdded(WidgetAdded.newBuilder().setRequestId(requestId).setWidgetId(id))
                    .build().toByteArray(),
            )
            sendList()
            log.info("placed widget $id of $provider")
        }
        if (!offered.needsConfiguration) return placedNow()
        log.info("widget $id: running its setup")
        var answered = false
        try {
            platform.configure(id) { finished ->
                post {
                    if (answered) return@post
                    answered = true
                    if (finished) {
                        placedNow()
                    } else {
                        forget(id)
                        refuse("the user canceled the widget's setup")
                    }
                }
            }
        } catch (e: Exception) {
            log.warn("starting the setup of widget $id failed", e)
            answered = true
            forget(id)
            refuse("the widget's setup did not start")
        }
    }

    private fun remove(id: Int) {
        if (id !in placed()) {
            log.warn("the host removed widget $id, which is not placed")
        } else {
            forget(id)
            log.info("removed widget $id")
        }
        sendList()
    }

    private fun resize(resize: io.github.thisisthepy.dsa.control.v1.ResizeWidget) {
        val id = resize.widgetId
        if (id !in shown && id !in placed()) {
            log.warn("dropped a size for widget $id, which is not placed")
            return
        }
        if (resize.width !in 1..MAX_SIDE || resize.height !in 1..MAX_SIDE ||
            resize.densityDpi !in 40..1280 || resize.widthDp !in 1..MAX_SIDE || resize.heightDp !in 1..MAX_SIDE
        ) {
            log.warn("dropped a size for widget $id that makes no sense: ${resize.width}x${resize.height} at ${resize.densityDpi} dpi")
            return
        }
        val size = WidgetSize(resize.width, resize.height, resize.densityDpi, resize.widthDp, resize.heightDp)
        if (shown[id] == size) return
        store.save(id, size)
        show(id, size)
    }

    private fun pointer(event: io.github.thisisthepy.dsa.control.v1.WidgetPointer) {
        val size = shown[event.widgetId]
        if (size == null) {
            log.warn("dropped pointer input for widget ${event.widgetId}, which is not drawn")
            return
        }
        if (!event.x.isFinite() || !event.y.isFinite()) {
            log.warn("dropped pointer input for widget ${event.widgetId} at no position")
            return
        }
        when (event.action) {
            PointerAction.POINTER_ACTION_DOWN,
            PointerAction.POINTER_ACTION_MOVE,
            PointerAction.POINTER_ACTION_UP,
            PointerAction.POINTER_ACTION_CANCEL,
            -> Unit
            else -> {
                log.warn("dropped pointer input of kind ${event.action} for widget ${event.widgetId}")
                return
            }
        }
        val x = event.x.coerceIn(0f, size.width.toFloat())
        val y = event.y.coerceIn(0f, size.height.toFloat())
        attempt("handing pointer input to widget ${event.widgetId}") {
            platform.pointer(event.widgetId, event.action, event.pointerId.coerceIn(0, 31), x, y)
        }
    }

    /** A new image of a widget: it goes to the host if it differs from the last one. */
    private fun frame(id: Int, width: Int, height: Int, png: ByteArray) {
        if (id !in shown || !isConnected()) return
        if (png.size > MAX_FRAME_BYTES) {
            log.warn("widget $id drew an image of ${png.size} bytes, more than $MAX_FRAME_BYTES; not sent")
            return
        }
        if (lastFrame[id]?.contentEquals(png) == true) return
        val sent = send(
            Envelope.newBuilder()
                .setWidgetFrame(
                    WidgetFrame.newBuilder().setWidgetId(id).setWidth(width).setHeight(height)
                        .setPng(ByteString.copyFrom(png)),
                )
                .build().toByteArray(),
        )
        if (sent) lastFrame[id] = png
    }

    private fun attempt(what: String, action: () -> Unit) {
        try {
            action()
        } catch (e: Exception) {
            log.warn("$what failed", e)
        }
    }

    companion object {
        const val MAX_PREVIEW_BYTES = 48 * 1024
        const val PREVIEW_PIXELS = 160
        const val MAX_FRAME_BYTES = 512 * 1024
        const val MAX_BATCH_BYTES = 256 * 1024
        const val MAX_PROVIDERS = 512
        const val MAX_WIDGETS = 64
        const val MAX_SIDE = 4096
    }
}

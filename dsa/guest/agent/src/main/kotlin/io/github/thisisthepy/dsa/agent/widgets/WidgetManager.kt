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
import io.github.thisisthepy.dsa.control.v1.WidgetReconfigured
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
    /** Whether a placed widget can be set up again from its menu ([canSetUpAgain]). */
    val reconfigurable: Boolean = false,
) {
    override fun equals(other: Any?): Boolean =
        other is WidgetInfo && provider == other.provider && packageName == other.packageName &&
            label == other.label && previewPng.contentEquals(other.previewPng) &&
            minWidthDp == other.minWidthDp && minHeightDp == other.minHeightDp &&
            defaultWidthDp == other.defaultWidthDp && defaultHeightDp == other.defaultHeightDp &&
            maxWidthDp == other.maxWidthDp && maxHeightDp == other.maxHeightDp &&
            resizeMode == other.resizeMode && needsConfiguration == other.needsConfiguration &&
            appLabel == other.appLabel && reconfigurable == other.reconfigurable

    override fun hashCode(): Int = provider.hashCode()

    companion object {
        /** AppWidgetProviderInfo.WIDGET_FEATURE_RECONFIGURABLE. */
        const val FEATURE_RECONFIGURABLE = 1

        /** AppWidgetProviderInfo.WIDGET_FEATURE_CONFIGURATION_OPTIONAL. */
        const val FEATURE_CONFIGURATION_OPTIONAL = 4

        /**
         * Whether placing a widget runs its setup first: it has a configuration activity, and
         * its app does not say the setup may be skipped. An app says so with both the
         * reconfigurable and the configuration optional features, as Android's own launcher
         * reads them: the optional feature means nothing without the other, since it says the
         * widget can be set up later instead.
         */
        fun needsSetup(hasConfigureActivity: Boolean, widgetFeatures: Int): Boolean {
            if (!hasConfigureActivity) return false
            val skippable = FEATURE_RECONFIGURABLE or FEATURE_CONFIGURATION_OPTIONAL
            return (widgetFeatures and skippable) != skippable
        }

        /**
         * Whether a placed widget can be set up again, as Android's launcher offers "Widget
         * settings" for it: it has a configuration activity, and its app says it is
         * reconfigurable.
         */
        fun canSetUpAgain(hasConfigureActivity: Boolean, widgetFeatures: Int): Boolean =
            hasConfigureActivity && (widgetFeatures and FEATURE_RECONFIGURABLE) != 0

        /** A launcher cell, in dp, as on a phone: [n] cells span `n * CELL_DP - GAP_DP`. */
        const val CELL_DP = 74

        /** The room between two launcher cells, in dp. */
        const val GAP_DP = 16

        /** The most cells a launcher's row or column gives a widget, as a phone's 5 by 5 grid. */
        const val GRID_CELLS = 5

        /**
         * The cells a launcher gives a widget of at least [minDp] along a side: Android's rule
         * for widget sizes is that n cells hold `70n - 30` dp, so a widget gets the fewest
         * cells that hold its minimum, and at least one.
         */
        fun cellsFor(minDp: Int): Int = ((minDp.coerceAtLeast(0) + 30 + 69) / 70).coerceAtLeast(1)

        /**
         * The side a widget is placed at, as a launcher places it: whole cells, the
         * [targetCells] its app asks for when it asks, and never fewer than hold its minimum
         * [minDp] or more than [GRID_CELLS]. A launcher's cells are larger than the minimum
         * they hold, and an app lays its widget out for that room, so a widget placed at its
         * bare minimum has its last row cut off once its text is a little larger. The side is
         * never below [minDp], and never above [maxDp] when that is set (0 for no limit).
         */
        fun launcherSide(minDp: Int, targetCells: Int, maxDp: Int = 0): Int {
            val min = minDp.coerceAtLeast(0)
            val cells = maxOf(targetCells, cellsFor(min)).coerceAtMost(GRID_CELLS)
            val side = maxOf(cells * CELL_DP - GAP_DP, min)
            return if (maxDp in 1 until side) maxOf(maxDp, min) else side
        }
    }

    /**
     * This widget as the host is told of it once its content was seen to need [widthDp] by
     * [heightDp]: its default size grows to hold the content, within its maximum, so a window
     * the user has not sized grows until nothing of the widget is cut off.
     */
    fun holding(widthDp: Int, heightDp: Int): WidgetInfo {
        fun side(default: Int, need: Int, max: Int): Int {
            val grown = maxOf(default, need).coerceIn(0, WidgetManager.MAX_SIDE)
            return if (max > 0) minOf(grown, maxOf(max, default)) else grown
        }
        return copy(
            defaultWidthDp = side(defaultWidthDp, widthDp, maxWidthDp),
            defaultHeightDp = side(defaultHeightDp, heightDp, maxHeightDp),
        )
    }

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
        .setReconfigurable(reconfigurable)
        .build()
}

/** How a widget's setup ended. */
sealed interface WidgetSetup {
    /** The user finished it: the setup answered RESULT_OK. */
    object Finished : WidgetSetup

    /** The user canceled it: the setup answered anything else, or its window was closed. */
    object Canceled : WidgetSetup

    /** The setup could not run at all, for the reason [why]; the user canceled nothing. */
    data class Failed(val why: String) : WidgetSetup

    companion object {
        /** Activity.RESULT_OK. */
        const val RESULT_OK = -1

        /** The ending an activity's result code means. */
        fun ofResult(resultCode: Int): WidgetSetup = if (resultCode == RESULT_OK) Finished else Canceled
    }
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

/**
 * How a widget's content fits the window it is drawn in. An app lays its widget out for the
 * size it is told, but its views can need more, as when its text is larger than it planned
 * for: a view laid out past the window's edge is cut off. The widget is then drawn shrunk just
 * enough that all of it shows ([scale]), and the host is told the size its content needs, so a
 * window the user has not sized grows to it and the widget is drawn at full size again.
 */
object WidgetFit {
    /** The most a widget's drawing is shrunk: content that reaches further is taken as no content. */
    const val MIN_SCALE = 0.5f

    /**
     * The scale, from [MIN_SCALE] to 1, at which content that reaches [reachWidth] by
     * [reachHeight] pixels from a widget's corner shows whole in [width] by [height] pixels.
     */
    fun scale(width: Int, height: Int, reachWidth: Int, reachHeight: Int): Float {
        if (width <= 0 || height <= 0) return 1f
        val across = if (reachWidth > width) width.toFloat() / reachWidth else 1f
        val down = if (reachHeight > height) height.toFloat() / reachHeight else 1f
        return minOf(across, down).coerceIn(MIN_SCALE, 1f)
    }

    /**
     * The pixels a widget drawn at [scale] in a window of [windowPixels] lays its views out in:
     * the window's, made larger by as much as the drawing is shrunk.
     */
    fun layoutSide(windowPixels: Int, scale: Float): Int =
        if (scale >= 1f) windowPixels else kotlin.math.ceil(windowPixels / scale.toDouble()).toInt()

    /** [px] pixels at [densityDpi], in dp, rounded up so the content is held whole. */
    fun dpOf(px: Int, densityDpi: Int): Int =
        if (densityDpi <= 0) px else kotlin.math.ceil(px * 160.0 / densityDpi - 0.001).toInt().coerceAtLeast(0)

    /**
     * The pixels a widget's views take along a side of [room] pixels when they span from
     * [start] to [end] pixels from its edge: the room, grown by as far as they reach past
     * either end. Android centres a widget's content in its view, so content larger than the
     * room reaches past both ends, the first as far as the last; counting only the far end
     * took half of it, and the window grew by halves without ever holding it.
     */
    fun span(room: Int, start: Int, end: Int): Int = maxOf(end, room) - minOf(start, 0)

    /**
     * Where a parent that cuts off its children at [edge] leaves the limit of what shows,
     * [current] before it, along a side whose own edge is [roomEdge]: the start of the room
     * when [low], its end otherwise. An edge inside the room is the app's own, and what it cuts
     * off is drawn as the app means it. An edge at or past the room's is where the widget's
     * window ends, and what reaches past it is cut off by the window, so it does not count as
     * a limit.
     */
    fun clipEdge(current: Int, edge: Int, roomEdge: Int, low: Boolean): Int = when {
        low && edge > roomEdge -> maxOf(current, edge)
        !low && edge < roomEdge -> minOf(current, edge)
        else -> current
    }

    /**
     * What a widget's app is told its size is, in dp: the size of its window, which is the
     * room its view is laid out in. Android's `updateAppWidgetSize` takes 8 dp off each side
     * for padding a launcher puts around a widget, but the agent's widget view has none: the
     * app was told 16 dp less each way than it had, laid its content out that much shorter,
     * and the window showed it with an empty band along its bottom.
     */
    fun told(widthDp: Int, heightDp: Int): Told = Told(widthDp.coerceAtLeast(1), heightDp.coerceAtLeast(1))

    /** A widget's size as its app is told it, in dp: the least and the most are both this size. */
    data class Told(val widthDp: Int, val heightDp: Int)
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
     * Runs the provider's configuration activity for [widgetId] in a task window of its own,
     * which the host shows as the app's window and focuses. [done] is called once, on any
     * thread, with how the setup ended.
     */
    fun configure(widgetId: Int, done: (WidgetSetup) -> Unit)

    /**
     * Draws the widget at [size] from now on, and tells its app the size. Before anything else
     * the widget's view is made and registered with the framework, which drops every update
     * of a widget ID no view of its host listens to. It is called for every placed widget
     * each time the agent starts, since the views of an earlier process are gone. When the
     * widget cannot be drawn, the platform calls the `failed` callback given to [start].
     */
    fun show(widgetId: Int, size: WidgetSize)

    /**
     * Draws the widget at [size] anew even when it is drawn at that size already, as after
     * the user set it up again: its next image goes to the host whatever it drew before.
     */
    fun redraw(widgetId: Int, size: WidgetSize) = show(widgetId, size)

    /** Hands pointer input to the widget's view. */
    fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float)

    /** Whether the guest is in Doze, when the widgets draw at their lowest rate. */
    fun dozing(dozing: Boolean)

    /**
     * Starts the platform: [frame] is called, on any thread, with each new image of a widget as
     * a PNG; [changed] when the installed widgets may have changed, as when an app is removed;
     * [failed], on any thread, when a widget [show] was asked to draw could not be, so it is
     * tried again.
     */
    fun start(
        frame: (widgetId: Int, width: Int, height: Int, png: ByteArray) -> Unit,
        changed: () -> Unit,
        failed: (widgetId: Int) -> Unit,
    )

    /**
     * Calls [ready], on any thread, once the framework keeps widgets: Android keeps them only
     * once the user's storage is unlocked, which comes some time after the agent starts. It
     * is called at once when that has happened already.
     */
    fun whenReady(ready: () -> Unit) = ready()

    /**
     * Calls [needs], on any thread, when a widget's views reach past the size it is drawn at:
     * its content needs [widthDp] by [heightDp] to show whole. Until its window is that large
     * the platform draws the widget shrunk to fit ([WidgetFit]), never cut off.
     */
    fun fitting(needs: (widgetId: Int, widthDp: Int, heightDp: Int) -> Unit) = Unit

    companion object {
        val NONE = object : WidgetPlatform {
            override fun providers() = emptyList<WidgetInfo>()
            override fun placedIds() = emptyList<Int>()
            override fun info(widgetId: Int): WidgetInfo? = null
            override fun allocate() = error("this agent hosts no widgets")
            override fun bind(widgetId: Int, provider: String) = false
            override fun delete(widgetId: Int) = Unit
            override fun configure(widgetId: Int, done: (WidgetSetup) -> Unit) =
                done(WidgetSetup.Failed("this agent hosts no widgets"))
            override fun show(widgetId: Int, size: WidgetSize) = Unit
            override fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float) = Unit
            override fun dozing(dozing: Boolean) = Unit
            override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit, failed: (Int) -> Unit) = Unit
        }
    }
}

/**
 * Where the agent keeps the size each widget was last drawn at, and the size in dp its content
 * was seen to need, across its restarts. [remove] forgets both.
 */
interface WidgetStore {
    fun sizes(): Map<Int, WidgetSize>

    fun save(widgetId: Int, size: WidgetSize)

    fun remove(widgetId: Int)

    /**
     * The width and height in dp each widget's content needs, so the host hears a widget
     * described at the size that holds it from the start, and its window does not shrink and
     * grow again each time Android starts.
     */
    fun needs(): Map<Int, Pair<Int, Int>> = emptyMap()

    fun saveNeed(widgetId: Int, widthDp: Int, heightDp: Int) = Unit

    companion object {
        /** Keeps them in memory only. */
        fun memory(): WidgetStore = object : WidgetStore {
            private val kept = mutableMapOf<Int, WidgetSize>()
            private val needed = mutableMapOf<Int, Pair<Int, Int>>()
            override fun sizes() = kept.toMap()
            override fun save(widgetId: Int, size: WidgetSize) {
                kept[widgetId] = size
            }

            override fun remove(widgetId: Int) {
                kept.remove(widgetId)
                needed.remove(widgetId)
            }

            override fun needs() = needed.toMap()

            override fun saveNeed(widgetId: Int, widthDp: Int, heightDp: Int) {
                needed[widgetId] = widthDp to heightDp
            }
        }
    }
}

/**
 * Whether an RGBA image read back from the GPU holds nothing: every pixel's alpha is zero.
 * A widget's view always draws something, so an image that is all clear means the GPU's
 * drawing did not reach memory the CPU can read, as when the guest's graphics driver keeps
 * the buffer's contents on the host. [buffer] holds [height] rows of [rowStride] bytes,
 * [pixelStride] bytes a pixel, alpha last; its position is left as it was.
 */
fun allClear(buffer: java.nio.ByteBuffer, width: Int, height: Int, rowStride: Int, pixelStride: Int = 4): Boolean {
    val start = buffer.position()
    for (y in 0 until height) {
        val row = start + y * rowStride
        for (x in 0 until width) {
            val at = row + x * pixelStride + 3
            if (at >= buffer.limit()) return true
            if (buffer.get(at) != 0.toByte()) return false
        }
    }
    return true
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
    private var later: (() -> Unit) -> Unit = {}

    /** The widgets waiting to be shown again after drawing them failed. */
    private val retrying = mutableSetOf<Int>()
    private val shown = mutableMapOf<Int, WidgetSize>()
    private val lastFrame = mutableMapOf<Int, ByteArray>()

    /**
     * The latest image each widget drew, sent or not. A widget draws once when it is shown
     * and then only when it changes, so an image drawn before the host connected is the only
     * one a still widget makes: each new connection gets it from here.
     */
    private val drawn = mutableMapOf<Int, Drawn>()

    private class Drawn(val width: Int, val height: Int, val png: ByteArray)

    /**
     * The size in dp each widget's content was seen to need, the largest since the agent
     * started; the host hears each widget described with its default grown to it
     * ([WidgetInfo.holding]).
     */
    private val needs = mutableMapOf<Int, Pair<Int, Int>>()

    /** Whether the host has heard which widgets are placed since the agent started. */
    private var listed = false

    /** Whether the widget host is running, so the placed widgets can be read. */
    private var started = false

    /** Whether a start was refused and is being tried again. */
    private var waiting = false

    /** Whether the host connected before the widget host started, so it is told once it has. */
    private var missedConnection = false

    /** Whether a later start is scheduled already. */
    private var startScheduled = false

    /** Why the last start was refused, so each new reason is logged once. */
    private var refusal: String? = null

    /** What the host asked before the widget host started, run in order once it has. */
    private val held = mutableListOf<() -> Unit>()

    /**
     * Starts drawing the placed widgets; [post] runs work on the agent's thread. The agent
     * starts before the user's storage is unlocked, and Android keeps no widgets until then,
     * so the widget host starts when the platform says the user is unlocked
     * ([WidgetPlatform.whenReady]), at once from there. A start refused all the same runs
     * again through [later] until it succeeds. Until it has, the host hears nothing about
     * widgets: an empty list would make it remove the ones placed.
     */
    fun start(later: (() -> Unit) -> Unit = {}, post: (() -> Unit) -> Unit) {
        this.post = post
        this.later = later
        try {
            // Ready already: the widget host starts now, as part of this call. Ready later:
            // it starts on the agent's thread then.
            var calling = true
            platform.whenReady { if (calling) begin() else post { begin() } }
            calling = false
        } catch (e: Exception) {
            log.warn("cannot hear when the user is unlocked; starting the widget host by trying", e)
            begin()
        }
    }

    private fun begin() {
        if (started) return
        try {
            platform.fitting { id, width, height -> post { contentNeeds(id, width, height) } }
            platform.start(
                frame = { id, width, height, png -> post { frame(id, width, height, png) } },
                changed = { post { if (isConnected()) announce() else prune() } },
                failed = { id -> post { retry(id) } },
            )
        } catch (e: Exception) {
            val why = "${e.javaClass.name}: ${e.message}"
            if (why != refusal) log.warn("the widget host did not start yet; trying again until it does", e)
            refusal = why
            waiting = true
            if (!startScheduled) {
                startScheduled = true
                later {
                    startScheduled = false
                    begin()
                }
            }
            return
        }
        started = true
        if (waiting) log.info("the widget host started")
        try {
            needs.putAll(store.needs())
        } catch (e: Exception) {
            log.warn("reading what the widgets' content needs failed; each is measured again", e)
        }
        prune()
        val sizes = store.sizes()
        for (id in placed()) {
            val size = sizes[id] ?: platform.info(id)?.let(WidgetSize::defaultOf) ?: continue
            show(id, size)
        }
        if ((waiting || missedConnection) && isConnected()) announce()
        missedConnection = false
        waiting = false
        val asked = held.toList()
        held.clear()
        for (work in asked) work()
    }

    /**
     * Runs [work] now when the widget host has started, or once it has. Android answers no
     * widget call until the user's storage is unlocked ("User 0 must be unlocked for widgets
     * to be available"), and the host can ask before then: Android reports its boot finished
     * when it starts unlocking the user, not when the unlock is done. Answered at once, a
     * request to place the clock was refused with "no app in Android offers the widget".
     * The host waits for each answer with a timeout of its own, so a request held here is
     * answered or given up, never lost.
     */
    private fun whenStarted(work: () -> Unit) {
        if (started) {
            work()
        } else if (held.size >= MAX_HELD) {
            log.warn("${held.size} widget requests wait for the user to be unlocked; answering this one now")
            work()
        } else {
            held += work
        }
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
        drawn.remove(id)
        needs.remove(id)
    }

    /**
     * Widget [id]'s views reach past its size: its content needs [widthDp] by [heightDp]. When
     * that grows what the host was told of the widget, the host hears it described again with
     * a default that holds the content, and grows the widget's window to it unless the user
     * chose its size. A need only ever grows, and is kept across the agent's restarts, so a
     * widget whose content changes size from one update to the next, or whose Android
     * restarts, never makes its window jump back and forth.
     */
    private fun contentNeeds(id: Int, widthDp: Int, heightDp: Int) {
        if (id !in shown) return
        val before = needs[id]
        val width = maxOf(before?.first ?: 0, widthDp.coerceIn(0, MAX_SIDE))
        val height = maxOf(before?.second ?: 0, heightDp.coerceIn(0, MAX_SIDE))
        if (before == (width to height)) return
        needs[id] = width to height
        try {
            store.saveNeed(id, width, height)
        } catch (e: Exception) {
            log.warn("keeping what widget $id's content needs failed", e)
        }
        val info = try {
            platform.info(id)
        } catch (e: Exception) {
            log.warn("reading widget $id failed", e)
            null
        } ?: return
        val told = before?.let { info.holding(it.first, it.second) } ?: info
        val now = info.holding(width, height)
        if (now == told) return
        log.info(
            "widget $id: its content needs ${width}x$height dp, more than " +
                "${told.defaultWidthDp}x${told.defaultHeightDp}; describing it at ${now.defaultWidthDp}x${now.defaultHeightDp}",
        )
        if (isConnected()) send(updated(id, info))
    }

    /** The host connected: it hears of every placed widget and what each shows now. */
    fun connected() {
        lastFrame.clear()
        if (!started) {
            missedConnection = true
            return
        }
        announce()
        for ((id, image) in drawn.toList()) {
            if (id in shown) frame(id, image.width, image.height, image.png)
        }
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

    /** The widget [id] of [info] as the host is told of it: with a default that holds its content. */
    private fun updated(id: Int, info: WidgetInfo): ByteArray {
        val described = needs[id]?.let { (width, height) -> info.holding(width, height) } ?: info
        return Envelope.newBuilder()
            .setWidgetUpdated(WidgetUpdated.newBuilder().setWidgetId(id).setProvider(described.toProto()))
            .build().toByteArray()
    }

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
        try {
            platform.show(id, size)
        } catch (e: Exception) {
            log.warn("drawing widget $id failed; trying again", e)
            retry(id)
        }
    }

    /**
     * A placed widget could not be drawn: it is shown again a little later, until it is, so a
     * widget never stays without a view listening to its updates.
     */
    private fun retry(id: Int) {
        if (id !in shown || !retrying.add(id)) return
        later {
            retrying.remove(id)
            val size = shown[id] ?: return@later
            if (id !in placed()) return@later
            show(id, size)
        }
    }

    /** Acts on the frame if it is about widgets. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.LIST_WIDGET_PROVIDERS ->
                whenStarted { listProviders(envelope.listWidgetProviders.requestId) }
            Envelope.BodyCase.ADD_WIDGET ->
                whenStarted { add(envelope.addWidget.requestId, envelope.addWidget.provider) }
            Envelope.BodyCase.REMOVE_WIDGET -> whenStarted { remove(envelope.removeWidget.widgetId) }
            Envelope.BodyCase.RECONFIGURE_WIDGET -> whenStarted {
                reconfigure(envelope.reconfigureWidget.requestId, envelope.reconfigureWidget.widgetId)
            }
            Envelope.BodyCase.RESIZE_WIDGET -> whenStarted { resize(envelope.resizeWidget) }
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
            platform.configure(id) { setup ->
                post {
                    if (answered) return@post
                    answered = true
                    when (setup) {
                        WidgetSetup.Finished -> placedNow()
                        WidgetSetup.Canceled -> {
                            forget(id)
                            refuse("the user canceled the widget's setup")
                        }
                        is WidgetSetup.Failed -> {
                            forget(id)
                            refuse("the widget's setup did not start: ${setup.why}")
                        }
                    }
                }
            }
        } catch (e: Exception) {
            log.warn("starting the setup of widget $id failed", e)
            if (answered) return
            answered = true
            forget(id)
            refuse("the widget's setup did not start")
        }
    }

    /**
     * The user chose the settings of placed widget [id]: its setup runs again for the same
     * widget ID, in a task window of its own as when it was placed, and the host hears how it
     * ended. A finished setup describes the widget to the host again and draws it anew, so
     * the widget shows what the user chose; a canceled one leaves it as it was. Either way
     * the widget stays placed.
     */
    private fun reconfigure(requestId: Int, id: Int) {
        val answer = { finished: Boolean, why: String ->
            send(
                Envelope.newBuilder()
                    .setWidgetReconfigured(
                        WidgetReconfigured.newBuilder().setRequestId(requestId).setWidgetId(id)
                            .setFinished(finished).setError(why),
                    )
                    .build().toByteArray(),
            )
            Unit
        }
        val info = if (id in placed()) {
            try {
                platform.info(id)
            } catch (e: Exception) {
                log.warn("reading widget $id failed", e)
                null
            }
        } else {
            null
        }
        if (info == null) {
            log.warn("the host asked for the settings of widget $id, which is not placed")
            return answer(false, "the widget $id is not placed")
        }
        if (!info.reconfigurable) {
            log.warn("the host asked for the settings of widget $id, whose app offers none")
            return answer(false, "the app of the widget offers no settings for it")
        }
        log.info("widget $id: running its setup again")
        var answered = false
        try {
            platform.configure(id) { setup ->
                post {
                    if (answered) return@post
                    answered = true
                    when (setup) {
                        WidgetSetup.Finished -> {
                            val now = platform.info(id) ?: info
                            send(updated(id, now))
                            val size = shown[id] ?: store.sizes()[id] ?: WidgetSize.defaultOf(now)
                            shown[id] = size
                            lastFrame.remove(id)
                            attempt("drawing widget $id again") { platform.redraw(id, size) }
                            answer(true, "")
                            log.info("widget $id: set up again")
                        }
                        WidgetSetup.Canceled -> answer(false, "the user closed the widget's settings without a change")
                        is WidgetSetup.Failed -> answer(false, "the widget's settings did not open: ${setup.why}")
                    }
                }
            }
        } catch (e: Exception) {
            log.warn("opening the settings of widget $id failed", e)
            if (answered) return
            answered = true
            answer(false, "the widget's settings did not open")
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
        if (id !in shown) return
        if (png.size > MAX_FRAME_BYTES) {
            log.warn("widget $id drew an image of ${png.size} bytes, more than $MAX_FRAME_BYTES; not sent")
            return
        }
        drawn[id] = Drawn(width, height, png)
        if (!isConnected()) return
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

        /** How many requests wait for the widget host to start, at most. */
        const val MAX_HELD = 64
    }
}

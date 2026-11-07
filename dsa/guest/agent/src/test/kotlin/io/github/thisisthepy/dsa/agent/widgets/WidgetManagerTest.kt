package io.github.thisisthepy.dsa.agent.widgets

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.AddWidget
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ListWidgetProviders
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.RemoveWidget
import io.github.thisisthepy.dsa.control.v1.ResizeWidget
import io.github.thisisthepy.dsa.control.v1.WidgetPointer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The guest's widget framework: what is offered and placed, and what the agent asked of it. */
class FakeWidgetPlatform : WidgetPlatform {
    val offered = mutableListOf<WidgetInfo>()
    val bound = sortedMapOf<Int, String>()
    var allowBind = true
    var nextId = 1
    val deleted = mutableListOf<Int>()
    val shown = mutableListOf<Pair<Int, WidgetSize>>()
    val pointers = mutableListOf<String>()
    val dozing = mutableListOf<Boolean>()
    val configuring = mutableMapOf<Int, (Boolean) -> Unit>()
    var frame: (Int, Int, Int, ByteArray) -> Unit = { _, _, _, _ -> }
    var changed: () -> Unit = {}
    var failed: (Int) -> Unit = {}

    /**
     * The widget IDs a view listens to. Like Android's widget host, it starts empty in every
     * process: the views an earlier agent made are gone with it, and [update] drops an update
     * for an ID nothing listens to.
     */
    val listening = mutableSetOf<Int>()

    /** Updates [update] dropped because no view listened to their widget. */
    val dropped = mutableListOf<Int>()

    /** How many of the next [show] calls fail to draw, as a window that does not open. */
    var failingShows = 0

    /** The widget's app sent new views: the widget draws [png] if a view listens to it. */
    fun update(widgetId: Int, png: ByteArray) {
        if (widgetId !in listening) {
            dropped += widgetId
            return
        }
        val size = shown.last { it.first == widgetId }.second
        frame(widgetId, size.width, size.height, png)
    }

    override fun providers() = offered.toList()
    override fun placedIds() = bound.keys.toList()
    override fun info(widgetId: Int) = bound[widgetId]?.let { provider -> offered.firstOrNull { it.provider == provider } }
    override fun allocate() = nextId++
    override fun bind(widgetId: Int, provider: String): Boolean {
        if (!allowBind) return false
        bound[widgetId] = provider
        return true
    }

    override fun delete(widgetId: Int) {
        bound.remove(widgetId)
        listening.remove(widgetId)
        deleted += widgetId
    }

    override fun configure(widgetId: Int, done: (Boolean) -> Unit) {
        configuring[widgetId] = done
    }

    override fun show(widgetId: Int, size: WidgetSize) {
        shown += widgetId to size
        if (failingShows > 0) {
            failingShows--
            failed(widgetId)
            return
        }
        listening += widgetId
    }

    override fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float) {
        pointers += "$widgetId $action $x,$y"
    }

    override fun dozing(dozing: Boolean) {
        this.dozing += dozing
    }

    override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit, failed: (Int) -> Unit) {
        this.frame = frame
        this.changed = changed
        this.failed = failed
    }
}

fun clock(configure: Boolean = false) = WidgetInfo(
    provider = "com.example.clock/.ClockWidget",
    packageName = "com.example.clock",
    label = "Clock",
    previewPng = byteArrayOf(0x89.toByte(), 'P'.code.toByte(), 'N'.code.toByte(), 'G'.code.toByte()),
    minWidthDp = 110,
    minHeightDp = 40,
    defaultWidthDp = 180,
    defaultHeightDp = 110,
    maxWidthDp = 0,
    maxHeightDp = 0,
    resizeMode = 3,
    needsConfiguration = configure,
)

class WidgetManagerTest {
    private val platform = FakeWidgetPlatform()
    private val store = WidgetStore.memory()
    private val sent = mutableListOf<Envelope>()
    private val log = RecordingLog()
    private val scheduler = FakeScheduler()
    private var connected = true
    private val manager = WidgetManager(platform, store, { frame -> sent += decode(frame); true }, log, { connected })

    private fun started(): WidgetManager {
        manager.start { work -> scheduler.post(work) }
        return manager
    }

    private fun add(provider: String = "com.example.clock/.ClockWidget", requestId: Int = 8) {
        manager.handle(Envelope.newBuilder().setAddWidget(AddWidget.newBuilder().setRequestId(requestId).setProvider(provider)).build())
        scheduler.runDue()
    }

    @Test
    fun fr12_the_gallery_lists_every_offered_widget_in_batches_that_fit_a_frame() {
        started()
        repeat(40) { i ->
            platform.offered += clock().copy(provider = "com.example.w$i/.W", previewPng = ByteArray(40 * 1024) { 1 })
        }
        manager.handle(Envelope.newBuilder().setListWidgetProviders(ListWidgetProviders.newBuilder().setRequestId(7)).build())
        val parts = sent.filter { it.hasWidgetProviders() }.map { it.widgetProviders }
        assertTrue(parts.size > 1)
        assertTrue(parts.all { it.requestId == 7 && it.serializedSize < 300 * 1024 })
        assertTrue(parts.dropLast(1).none { it.complete })
        assertTrue(parts.last().complete)
        assertEquals(40, parts.sumOf { it.providersCount })
    }

    @Test
    fun fr12_a_preview_too_large_is_left_out_and_the_widget_still_listed() {
        assertEquals(0, clock().copy(previewPng = ByteArray(WidgetManager.MAX_PREVIEW_BYTES + 1)).toProto().previewPng.size())
        assertEquals(4, clock().toProto().previewPng.size())
    }

    @Test
    fun fr12_a_widget_carries_the_name_of_the_app_that_offers_it() {
        assertEquals("Clock App", clock().copy(appLabel = "Clock App").toProto().appLabel)
        assertEquals("", clock().toProto().appLabel)
    }

    @Test
    fun fr12_placing_a_widget_binds_it_draws_it_at_its_default_size_and_tells_the_host() {
        started()
        platform.offered += clock()
        add()
        assertEquals(mapOf(1 to "com.example.clock/.ClockWidget"), platform.bound.toMap())
        assertEquals(listOf(1 to WidgetSize(180, 110, 160, 180, 110)), platform.shown)
        val kinds = sent.map { it.bodyCase }
        assertEquals(
            listOf(Envelope.BodyCase.WIDGET_UPDATED, Envelope.BodyCase.WIDGET_ADDED, Envelope.BodyCase.WIDGET_LIST),
            kinds,
        )
        assertEquals(1, sent[1].widgetAdded.widgetId)
        assertEquals(8, sent[1].widgetAdded.requestId)
        assertEquals(listOf(1), sent[2].widgetList.widgetIdsList)
        assertEquals(WidgetSize(180, 110, 160, 180, 110), store.sizes()[1])
    }

    @Test
    fun fr12_a_widget_android_does_not_allow_or_no_app_offers_is_refused_with_the_reason() {
        started()
        add("com.example.none/.Nothing")
        assertTrue(sent.single().widgetAdded.error.contains("no app"))
        platform.offered += clock()
        platform.allowBind = false
        add(requestId = 9)
        val refused = sent.last().widgetAdded
        assertEquals(9, refused.requestId)
        assertEquals(0, refused.widgetId)
        assertTrue(refused.error.contains("did not allow"))
        assertEquals(listOf(1), platform.deleted)
    }

    @Test
    fun fr12_a_widget_that_needs_setup_is_placed_once_the_user_finishes_it() {
        started()
        platform.offered += clock(configure = true)
        add()
        assertTrue("nothing is placed before the setup ends", sent.isEmpty())
        platform.configuring.getValue(1)(true)
        scheduler.runDue()
        assertEquals(1, sent.first { it.hasWidgetAdded() }.widgetAdded.widgetId)

        add(requestId = 9)
        platform.configuring.getValue(2)(false)
        scheduler.runDue()
        assertTrue(sent.last { it.hasWidgetAdded() }.widgetAdded.error.contains("canceled"))
        assertEquals(listOf(2), platform.deleted)
        // A second answer changes nothing.
        platform.configuring.getValue(2)(true)
        scheduler.runDue()
        assertEquals(listOf(1), platform.placedIds())
    }

    @Test
    fun fr12_resizing_tells_the_widget_its_size_in_dp_and_keeps_it() {
        started()
        platform.offered += clock()
        add()
        manager.handle(
            Envelope.newBuilder().setResizeWidget(
                ResizeWidget.newBuilder().setWidgetId(1).setWidth(360).setHeight(220).setDensityDpi(240)
                    .setWidthDp(240).setHeightDp(147),
            ).build(),
        )
        assertEquals(1 to WidgetSize(360, 220, 240, 240, 147), platform.shown.last())
        assertEquals(WidgetSize(360, 220, 240, 240, 147), store.sizes()[1])
        // Nonsense sizes and unknown widgets are dropped.
        val before = platform.shown.size
        manager.handle(Envelope.newBuilder().setResizeWidget(ResizeWidget.newBuilder().setWidgetId(1).setWidth(0).setHeight(5).setDensityDpi(240).setWidthDp(1).setHeightDp(1)).build())
        manager.handle(Envelope.newBuilder().setResizeWidget(ResizeWidget.newBuilder().setWidgetId(99).setWidth(10).setHeight(10).setDensityDpi(160).setWidthDp(10).setHeightDp(10)).build())
        assertEquals(before, platform.shown.size)
    }

    @Test
    fun fr12_a_click_goes_to_the_widgets_view_within_its_size() {
        started()
        platform.offered += clock()
        add()
        fun press(action: PointerAction, x: Float, y: Float, id: Int = 1) = manager.handle(
            Envelope.newBuilder().setWidgetPointer(WidgetPointer.newBuilder().setWidgetId(id).setAction(action).setX(x).setY(y)).build(),
        )
        press(PointerAction.POINTER_ACTION_DOWN, 10f, 20f)
        press(PointerAction.POINTER_ACTION_UP, 500f, 20f)
        press(PointerAction.POINTER_ACTION_SCROLL, 1f, 1f)
        press(PointerAction.POINTER_ACTION_DOWN, Float.NaN, 1f)
        press(PointerAction.POINTER_ACTION_DOWN, 1f, 1f, id = 42)
        assertEquals(listOf("1 POINTER_ACTION_DOWN 10.0,20.0", "1 POINTER_ACTION_UP 180.0,20.0"), platform.pointers)
    }

    @Test
    fun fr12_widgets_persist_and_each_connection_hears_of_them_and_a_widget_whose_app_is_gone_is_removed() {
        platform.offered += clock()
        platform.bound[3] = "com.example.clock/.ClockWidget"
        platform.bound[4] = "com.example.gone/.Widget"
        store.save(3, WidgetSize(300, 200, 240, 200, 133))
        started()
        assertEquals(listOf(4), platform.deleted)
        assertEquals(listOf(3 to WidgetSize(300, 200, 240, 200, 133)), platform.shown)
        manager.connected()
        assertEquals(listOf(Envelope.BodyCase.WIDGET_UPDATED, Envelope.BodyCase.WIDGET_LIST), sent.map { it.bodyCase })
        assertEquals(3, sent[0].widgetUpdated.widgetId)
        assertEquals("Clock", sent[0].widgetUpdated.provider.label)
        assertEquals(listOf(3), sent[1].widgetList.widgetIdsList)
        // The app is uninstalled while connected: the host hears the widget is gone.
        platform.offered.clear()
        sent.clear()
        platform.changed()
        scheduler.runDue()
        assertEquals(listOf<Int>(), sent.single().widgetList.widgetIdsList)
    }

    @Test
    fun fr12_removing_a_widget_forgets_it_and_tells_the_host() {
        started()
        platform.offered += clock()
        add()
        sent.clear()
        manager.handle(Envelope.newBuilder().setRemoveWidget(RemoveWidget.newBuilder().setWidgetId(1)).build())
        assertEquals(listOf(1), platform.deleted)
        assertEquals(null, store.sizes()[1])
        assertEquals(listOf<Int>(), sent.single().widgetList.widgetIdsList)
    }

    @Test
    fun fr12_a_widgets_images_go_to_the_host_only_when_they_change() {
        started()
        platform.offered += clock()
        add()
        sent.clear()
        val png = byteArrayOf(0x89.toByte(), 1, 2)
        platform.frame(1, 180, 110, png)
        platform.frame(1, 180, 110, png.copyOf())
        platform.frame(1, 180, 110, ByteArray(WidgetManager.MAX_FRAME_BYTES + 1))
        platform.frame(77, 10, 10, png)
        scheduler.runDue()
        assertEquals(1, sent.size)
        assertEquals(180, sent[0].widgetFrame.width)
        // A new connection gets the image again, as it connects, and only once: the same
        // image drawn again after it is not sent twice.
        sent.clear()
        manager.connected()
        platform.frame(1, 180, 110, png)
        scheduler.runDue()
        assertEquals(1, sent.count { it.hasWidgetFrame() })
        // Nothing goes while the host is not connected.
        connected = false
        platform.frame(1, 180, 110, byteArrayOf(9))
        scheduler.runDue()
        assertEquals(1, sent.count { it.hasWidgetFrame() })
    }

    @Test
    fun fr12_an_image_drawn_before_the_host_connected_reaches_it_when_it_connects() {
        // A still widget draws once, when it is shown at boot, long before the host connects,
        // and never again: that image is the only one there is.
        platform.offered += clock()
        platform.bound[3] = "com.example.clock/.ClockWidget"
        connected = false
        started()
        val png = byteArrayOf(0x89.toByte(), 5, 6)
        platform.frame(3, 180, 110, png)
        scheduler.runDue()
        assertTrue(sent.none { it.hasWidgetFrame() })
        connected = true
        manager.connected()
        val kinds = sent.map { it.bodyCase }
        assertEquals(
            listOf(Envelope.BodyCase.WIDGET_UPDATED, Envelope.BodyCase.WIDGET_LIST, Envelope.BodyCase.WIDGET_FRAME),
            kinds,
        )
        val frame = sent.last().widgetFrame
        assertEquals(3, frame.widgetId)
        assertEquals(180, frame.width)
        assertTrue(frame.png.toByteArray().contentEquals(png))
        // A later connection gets it again, and a removed widget's image is forgotten.
        sent.clear()
        manager.connected()
        assertEquals(1, sent.count { it.hasWidgetFrame() })
        manager.handle(Envelope.newBuilder().setRemoveWidget(RemoveWidget.newBuilder().setWidgetId(3)).build())
        sent.clear()
        manager.connected()
        assertEquals(0, sent.count { it.hasWidgetFrame() })
    }

    @Test
    fun fr12_an_image_the_gpu_left_clear_is_told_from_one_with_something_drawn() {
        // Two rows of three pixels, with two bytes of padding at the end of each row.
        val stride = 3 * 4 + 2
        val clear = java.nio.ByteBuffer.allocate(stride * 2)
        assertTrue(allClear(clear, 3, 2, stride))
        val drawn = java.nio.ByteBuffer.allocate(stride * 2)
        drawn.put(stride + 2 * 4 + 3, 0x80.toByte())
        assertFalse(allClear(drawn, 3, 2, stride))
        // Padding is not a pixel: something there leaves the image clear.
        val padded = java.nio.ByteBuffer.allocate(stride * 2)
        padded.put(3 * 4 + 1, 0x7f)
        assertTrue(allClear(padded, 3, 2, stride))
    }

    @Test
    fun fr12_widgets_draw_at_their_lowest_rate_while_the_guest_dozes() {
        started()
        manager.screen(false)
        manager.screen(true)
        assertEquals(listOf(true, false), platform.dozing)
        val gate = FrameGate(activeMillis = 100, dozeMillis = 1000)
        gate.taken(5000)
        assertEquals(100, gate.delay(5000))
        assertEquals(0, gate.delay(5200))
        gate.dozing = true
        assertEquals(1000, gate.delay(5000))
        assertEquals(500, gate.delay(5500))
        assertEquals(1000, FrameGate.untilNextTick(60_000, seconds = true))
        assertEquals(400, FrameGate.untilNextTick(60_600, seconds = true))
        assertEquals(59_400, FrameGate.untilNextTick(60_600, seconds = false))
    }

    @Test
    fun fr12_widgets_start_once_the_user_is_unlocked_and_the_host_keeps_them_meanwhile() {
        var unlocked = false
        val locked = object : WidgetPlatform by platform {
            override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit, failed: (Int) -> Unit) {
                check(unlocked) { "User 0 must be unlocked for widgets to be available" }
                platform.start(frame, changed, failed)
            }
            override fun placedIds(): List<Int> {
                check(unlocked) { "User 0 must be unlocked for widgets to be available" }
                return platform.placedIds()
            }
        }
        platform.offered += clock()
        platform.bound[3] = "com.example.clock/.ClockWidget"
        val retries = mutableListOf<() -> Unit>()
        val manager = WidgetManager(locked, store, { frame -> sent += decode(frame); true }, log, { connected })
        manager.start(later = { retries += it }) { it() }
        manager.connected()
        // Locked: no list, which the host would take as every widget removed.
        assertTrue(sent.isEmpty())
        assertEquals(1, retries.size)
        retries.removeAt(0)()
        assertEquals(1, retries.size)
        assertEquals(1, log.lines.size)
        unlocked = true
        retries.removeAt(0)()
        assertTrue(retries.isEmpty())
        assertEquals(listOf(3), platform.shown.map { it.first })
        assertEquals(listOf(Envelope.BodyCase.WIDGET_UPDATED, Envelope.BodyCase.WIDGET_LIST), sent.map { it.bodyCase })
        assertEquals(listOf(3), sent[1].widgetList.widgetIdsList)
    }

    @Test
    fun fr12_widgets_start_the_moment_the_user_is_unlocked_without_waiting_for_a_retry() {
        var unlocked = false
        var readyCallback: (() -> Unit)? = null
        val waiting = object : WidgetPlatform by platform {
            override fun whenReady(ready: () -> Unit) {
                if (unlocked) ready() else readyCallback = ready
            }
            override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit, failed: (Int) -> Unit) {
                check(unlocked) { "User 0 must be unlocked for widgets to be available" }
                platform.start(frame, changed, failed)
            }
        }
        platform.offered += clock()
        platform.bound[3] = "com.example.clock/.ClockWidget"
        val retries = mutableListOf<() -> Unit>()
        val manager = WidgetManager(waiting, store, { frame -> sent += decode(frame); true }, log, { connected })
        manager.start(later = { retries += it }) { it() }
        manager.connected()
        // Locked: the widget host is not even tried, so nothing fails and nothing is sent.
        assertTrue(sent.isEmpty())
        assertTrue(retries.isEmpty())
        assertTrue(log.lines.isEmpty())
        assertTrue(platform.shown.isEmpty())
        // Unlocked: the widgets start then, with no retry in between.
        unlocked = true
        readyCallback!!()
        assertTrue(retries.isEmpty())
        assertEquals(listOf(3), platform.shown.map { it.first })
        assertEquals(listOf(Envelope.BodyCase.WIDGET_UPDATED, Envelope.BodyCase.WIDGET_LIST), sent.map { it.bodyCase })
        // A second signal starts nothing again.
        readyCallback!!()
        assertEquals(1, platform.shown.size)
    }

    @Test
    fun fr12_placed_widgets_listen_and_draw_again_after_the_agent_restarts() {
        // Widgets 2 and 3 were placed before the agent or Android restarted. Android kept the
        // IDs; the agent kept a size for 2 only. The new process has no views yet.
        platform.offered += clock()
        platform.bound[2] = "com.example.clock/.ClockWidget"
        platform.bound[3] = "com.example.clock/.ClockWidget"
        store.save(2, WidgetSize(600, 340, 320, 300, 170))
        connected = false
        started()
        // Every placed widget has a view listening before the host is even there, so the
        // updates its app sends at boot are not dropped.
        assertEquals(setOf(2, 3), platform.listening)
        platform.update(3, byteArrayOf(0x89.toByte(), 3))
        platform.update(2, byteArrayOf(0x89.toByte(), 2))
        scheduler.runDue()
        assertTrue(platform.dropped.isEmpty())
        connected = true
        manager.connected()
        val frames = sent.filter { it.hasWidgetFrame() }.map { it.widgetFrame }
        assertEquals(setOf(2, 3), frames.map { it.widgetId }.toSet())
        assertEquals(600, frames.single { it.widgetId == 2 }.width)
        // The clock ticks: each new image reaches the host as it is drawn.
        sent.clear()
        platform.update(2, byteArrayOf(0x89.toByte(), 2, 1))
        platform.update(2, byteArrayOf(0x89.toByte(), 2, 2))
        scheduler.runDue()
        assertEquals(2, sent.count { it.hasWidgetFrame() && it.widgetFrame.widgetId == 2 })
    }

    @Test
    fun fr12_a_widget_whose_drawing_failed_is_tried_again_until_it_listens() {
        platform.offered += clock()
        platform.bound[3] = "com.example.clock/.ClockWidget"
        platform.failingShows = 2
        manager.start(later = { work -> scheduler.postDelayed(1000, work) }) { work -> scheduler.post(work) }
        scheduler.runDue()
        assertTrue(3 !in platform.listening)
        platform.update(3, byteArrayOf(1))
        assertEquals(listOf(3), platform.dropped)
        scheduler.advance(1000)
        scheduler.runDue()
        assertTrue(3 !in platform.listening)
        scheduler.advance(1000)
        scheduler.runDue()
        assertTrue(3 in platform.listening)
        manager.connected()
        platform.update(3, byteArrayOf(0x89.toByte(), 7))
        scheduler.runDue()
        assertEquals(3, sent.last { it.hasWidgetFrame() }.widgetFrame.widgetId)
        // A removed widget is not tried again.
        platform.failingShows = 1
        manager.handle(Envelope.newBuilder().setResizeWidget(ResizeWidget.newBuilder().setWidgetId(3).setWidth(200).setHeight(100).setDensityDpi(160).setWidthDp(200).setHeightDp(100)).build())
        scheduler.runDue()
        manager.handle(Envelope.newBuilder().setRemoveWidget(RemoveWidget.newBuilder().setWidgetId(3)).build())
        val shows = platform.shown.size
        scheduler.advance(5000)
        scheduler.runDue()
        assertEquals(shows, platform.shown.size)
    }

    @Test
    fun fr12_a_click_reaches_a_widget_restored_after_a_restart() {
        platform.offered += clock()
        platform.bound[5] = "com.example.clock/.ClockWidget"
        started()
        manager.connected()
        manager.handle(
            Envelope.newBuilder().setWidgetPointer(
                WidgetPointer.newBuilder().setWidgetId(5).setAction(PointerAction.POINTER_ACTION_DOWN).setX(4f).setY(5f),
            ).build(),
        )
        assertEquals(listOf("5 POINTER_ACTION_DOWN 4.0,5.0"), platform.pointers)
    }

    @Test
    fun nfr5_a_broken_widget_framework_does_not_stop_the_agent() {
        val broken = object : WidgetPlatform by FakeWidgetPlatform() {
            override fun providers(): List<WidgetInfo> = error("binder died")
            override fun placedIds(): List<Int> = error("binder died")
        }
        val manager = WidgetManager(broken, store, { frame -> sent += decode(frame); true }, log, { true })
        manager.start { it() }
        manager.connected()
        manager.handle(Envelope.newBuilder().setListWidgetProviders(ListWidgetProviders.newBuilder().setRequestId(1)).build())
        assertTrue(sent.last().widgetProviders.complete)
        assertFalse(log.lines.isEmpty())
    }
}

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
        deleted += widgetId
    }

    override fun configure(widgetId: Int, done: (Boolean) -> Unit) {
        configuring[widgetId] = done
    }

    override fun show(widgetId: Int, size: WidgetSize) {
        shown += widgetId to size
    }

    override fun pointer(widgetId: Int, action: PointerAction, pointerId: Int, x: Float, y: Float) {
        pointers += "$widgetId $action $x,$y"
    }

    override fun dozing(dozing: Boolean) {
        this.dozing += dozing
    }

    override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit) {
        this.frame = frame
        this.changed = changed
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
        // A new connection gets the image again.
        manager.connected()
        sent.clear()
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
            override fun start(frame: (Int, Int, Int, ByteArray) -> Unit, changed: () -> Unit) {
                check(unlocked) { "User 0 must be unlocked for widgets to be available" }
                platform.start(frame, changed)
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

package io.github.thisisthepy.dsa.agent.widgets

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.AddWidget
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ListWidgetProviders
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.ReconfigureWidget
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
    val configuring = mutableMapOf<Int, (WidgetSetup) -> Unit>()

    /** How the next setup ends before [configure] returns, as one that finishes at once; null to wait. */
    var setupAtOnce: WidgetSetup? = null

    /** Setups started, by widget ID, in order. */
    val setups = mutableListOf<Int>()
    var frame: (Int, Int, Int, ByteArray) -> Unit = { _, _, _, _ -> }
    var changed: () -> Unit = {}
    var failed: (Int) -> Unit = {}

    /** Says a widget's views reach past its size: its content needs a width and height in dp. */
    var needs: (Int, Int, Int) -> Unit = { _, _, _ -> }

    override fun fitting(needs: (widgetId: Int, widthDp: Int, heightDp: Int) -> Unit) {
        this.needs = needs
    }

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

    override fun configure(widgetId: Int, done: (WidgetSetup) -> Unit) {
        setups += widgetId
        configuring[widgetId] = done
        setupAtOnce?.let(done)
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

    /** The widgets drawn anew whatever they drew before, in order. */
    val redrawn = mutableListOf<Pair<Int, WidgetSize>>()

    override fun redraw(widgetId: Int, size: WidgetSize) {
        redrawn += widgetId to size
        show(widgetId, size)
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

fun clock(configure: Boolean = false, provider: String = "com.example.clock/.ClockWidget") = WidgetInfo(
    provider = provider,
    packageName = provider.substringBefore('/'),
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
    fun fr12_a_widget_is_placed_in_whole_launcher_cells_that_hold_its_minimum() {
        // Android's rule: n cells hold 70n - 30 dp.
        assertEquals(1, WidgetInfo.cellsFor(0))
        assertEquals(1, WidgetInfo.cellsFor(40))
        assertEquals(2, WidgetInfo.cellsFor(110))
        assertEquals(3, WidgetInfo.cellsFor(180))
        // A 5 by 2 music widget of 369 by 110 dp: two rows of cells, 132 dp, rather than its
        // bare 110 dp minimum, which cut its playback buttons off.
        assertEquals(132, WidgetInfo.launcherSide(110, 0))
        assertEquals("never below the minimum", 369, WidgetInfo.launcherSide(369, 0))
        assertEquals(58, WidgetInfo.launcherSide(40, 0))
        assertEquals("the cells its app asks for", 206, WidgetInfo.launcherSide(110, 3))
        assertEquals("never fewer cells than hold the minimum", 206, WidgetInfo.launcherSide(180, 1))
        assertEquals("no more than a launcher's grid", 354, WidgetInfo.launcherSide(110, 9))
        assertEquals("within its maximum", 120, WidgetInfo.launcherSide(110, 0, 120))
        assertEquals("a maximum below the minimum leaves the minimum", 110, WidgetInfo.launcherSide(110, 0, 90))
    }

    @Test
    fun fr12_a_widget_whose_content_needs_more_than_its_minimum_gets_a_window_that_fits() {
        val music = clock(provider = "com.example.music/.Widget5x2").copy(
            label = "Music",
            minWidthDp = 369,
            minHeightDp = 110,
            defaultWidthDp = WidgetInfo.launcherSide(369, 0),
            defaultHeightDp = WidgetInfo.launcherSide(110, 0),
            resizeMode = 1,
        )
        started()
        platform.offered += music
        add("com.example.music/.Widget5x2")
        val placed = sent.last { it.hasWidgetUpdated() }.widgetUpdated.provider
        assertEquals(369 to 132, placed.defaultWidthDp to placed.defaultHeightDp)

        // Its text is larger than its app planned for: its last row reaches 168 dp down.
        sent.clear()
        platform.needs(1, 369, 168)
        scheduler.runDue()
        val grown = sent.single { it.hasWidgetUpdated() }.widgetUpdated
        assertEquals(1, grown.widgetId)
        assertEquals("the window that holds it", 369 to 168, grown.provider.defaultWidthDp to grown.provider.defaultHeightDp)
        assertEquals("its minimum is what its app says", 110, grown.provider.minHeightDp)

        // A smaller need later changes nothing: the window never jumps back.
        sent.clear()
        platform.needs(1, 369, 150)
        scheduler.runDue()
        assertTrue(sent.none { it.hasWidgetUpdated() })

        // Each new connection hears the size that holds it.
        manager.connected()
        val again = sent.first { it.hasWidgetUpdated() }.widgetUpdated.provider
        assertEquals(168, again.defaultHeightDp)

        // An agent that starts again describes it at that size from the start, so its window
        // does not shrink and grow again each time Android starts.
        val restarted = mutableListOf<Envelope>()
        val next = WidgetManager(platform, store, { frame -> restarted += decode(frame); true }, log, { true })
        next.start { it() }
        next.connected()
        assertEquals(168, restarted.first { it.hasWidgetUpdated() }.widgetUpdated.provider.defaultHeightDp)

        // A need within the widget's maximum only.
        val capped = music.copy(maxHeightDp = 150).holding(369, 400)
        assertEquals(150, capped.defaultHeightDp)
    }

    @Test
    fun fr12_a_widget_whose_views_reach_past_its_window_is_drawn_shrunk_to_fit_never_cut() {
        // Its views fit: drawn at full size.
        assertEquals(1f, WidgetFit.scale(443, 158, 443, 158), 0f)
        // Its last row reaches 200 pixels down a window of 158: shrunk until it shows whole.
        val scale = WidgetFit.scale(443, 158, 443, 200)
        assertEquals(158f / 200f, scale, 0.0001f)
        assertTrue(WidgetFit.layoutSide(158, scale) >= 200)
        assertEquals(WidgetFit.layoutSide(443, scale).toFloat() * scale, 443f, 1f)
        // Across too, and the side that needs it more decides.
        assertEquals(0.8f, WidgetFit.scale(400, 100, 500, 110), 0.0001f)
        // Shrunk no further than can still be read.
        assertEquals(WidgetFit.MIN_SCALE, WidgetFit.scale(100, 100, 100, 1000), 0f)
        // The size it needs, rounded up so it is held whole.
        assertEquals(168, WidgetFit.dpOf(210, 200))
        assertEquals(169, WidgetFit.dpOf(200, 190))
        assertEquals(110, WidgetFit.dpOf(132, 192))
    }

    /**
     * A 5 by 2 music widget at 369 by 170 dp, on a monitor at 200% (262 dpi) and at 125%
     * (164 dpi). Its app was told 16 dp less each way than its window, Android's padding for a
     * launcher's grid, which the agent's widget view does not have: at 262 dpi it laid out
     * 154 dp, 252 pixels of a 278 pixel window, and the window showed a 26 pixel empty band
     * below the widget.
     */
    @Test
    fun fr12_a_widgets_app_is_told_the_size_of_its_window_at_200_and_125_percent() {
        for ((density, window) in listOf(262 to (604 to 278), 164 to (378 to 174))) {
            val told = WidgetFit.told(369, 170)
            assertEquals(WidgetFit.Told(369, 170), told)
            // The size the app lays out for covers the window to the pixel.
            val px = { dp: Int -> Math.round(dp * density / 160.0).toInt() }
            assertEquals("width at $density dpi", window.first.toDouble(), px(told.widthDp).toDouble(), 1.0)
            assertEquals("height at $density dpi", window.second.toDouble(), px(told.heightDp).toDouble(), 1.0)
        }
    }

    /**
     * Android centres a widget's content, so content taller than its window is cut off at the
     * top as much as at the bottom. Counting only the bottom, the 369 by 132 dp music widget
     * whose content was 172 dp grew 132, 140, 156, 163, 167, 169, 170 dp and never held it.
     */
    @Test
    fun fr12_a_widget_whose_content_overflows_at_the_top_too_is_measured_whole_and_shown_whole() {
        // 200%: 132 dp is 216 pixels, and 172 dp of content 282 pixels, from -33 to 249.
        val room = 216
        val content = 282
        val start = (room - content) / 2
        assertEquals(-33, start)
        val span = WidgetFit.span(room, start, start + content)
        assertEquals(content, span)
        assertEquals(173, WidgetFit.dpOf(span, 262))
        // Drawn shrunk until a room that large holds it, where centred content starts at 0.
        val scale = WidgetFit.scale(604, room, 604, span)
        val layout = WidgetFit.layoutSide(room, scale)
        assertTrue(layout >= content)
        assertTrue((layout - content) / 2 >= 0)
        // At its 170 dp (278 pixels) the content still reaches 2 pixels past each edge.
        assertEquals(282, WidgetFit.span(278, -2, 280))
        assertEquals(173, WidgetFit.dpOf(282, 262))
        // 125%: 132 dp is 135 pixels, the content 176.
        assertEquals(176, WidgetFit.span(135, (135 - 176) / 2, (135 - 176) / 2 + 176))
        assertEquals(172, WidgetFit.dpOf(176, 164))
        // Content inside its window needs nothing more.
        assertEquals(278, WidgetFit.span(278, 0, 278))
        assertEquals(278, WidgetFit.span(278, 20, 258))
    }

    @Test
    fun fr12_what_an_apps_own_view_cuts_off_inside_the_window_is_not_counted_as_cut() {
        // A card from 20 to 584 of a 604 pixel window cuts off its picture: the app's design.
        assertEquals(20, WidgetFit.clipEdge(Int.MIN_VALUE, 20, 0, low = true))
        assertEquals(584, WidgetFit.clipEdge(Int.MAX_VALUE, 584, 604, low = false))
        // A view at the window's edges, or past them, is cut off by the window itself.
        assertEquals(Int.MIN_VALUE, WidgetFit.clipEdge(Int.MIN_VALUE, 0, 0, low = true))
        assertEquals(Int.MIN_VALUE, WidgetFit.clipEdge(Int.MIN_VALUE, -13, 0, low = true))
        assertEquals(Int.MAX_VALUE, WidgetFit.clipEdge(Int.MAX_VALUE, 278, 278, low = false))
        assertEquals(Int.MAX_VALUE, WidgetFit.clipEdge(Int.MAX_VALUE, 291, 278, low = false))
        // An inner edge never loosens an outer one.
        assertEquals(30, WidgetFit.clipEdge(30, 20, 0, low = true))
        assertEquals(500, WidgetFit.clipEdge(500, 584, 604, low = false))
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
        assertEquals("the setup runs once", listOf(1), platform.setups)
        platform.configuring.getValue(1)(WidgetSetup.Finished)
        scheduler.runDue()
        assertEquals(1, sent.first { it.hasWidgetAdded() }.widgetAdded.widgetId)
        assertEquals(listOf(1), platform.placedIds())
        assertEquals(1, platform.shown.last().first)

        add(requestId = 9)
        platform.configuring.getValue(2)(WidgetSetup.Canceled)
        scheduler.runDue()
        assertTrue(sent.last { it.hasWidgetAdded() }.widgetAdded.error.contains("canceled"))
        assertEquals(listOf(2), platform.deleted)
        // A second answer changes nothing.
        platform.configuring.getValue(2)(WidgetSetup.Finished)
        scheduler.runDue()
        assertEquals(listOf(1), platform.placedIds())
    }

    @Test
    fun fr12_a_setup_that_finishes_at_once_with_ok_places_the_widget() {
        started()
        platform.offered += clock(configure = true)
        platform.setupAtOnce = WidgetSetup.Finished
        add()
        val added = sent.single { it.hasWidgetAdded() }.widgetAdded
        assertEquals("", added.error)
        assertEquals(1, added.widgetId)
        assertEquals(listOf(1), platform.placedIds())
        assertTrue("the host hears of the placed widget", sent.any { it.hasWidgetUpdated() && it.widgetUpdated.widgetId == 1 })
    }

    @Test
    fun fr12_a_setup_canceled_at_once_removes_the_widget_and_says_the_user_canceled() {
        started()
        platform.offered += clock(configure = true)
        platform.setupAtOnce = WidgetSetup.Canceled
        add()
        val added = sent.single { it.hasWidgetAdded() }.widgetAdded
        assertEquals("the user canceled the widget's setup", added.error)
        assertEquals(listOf(1), platform.deleted)
        assertTrue(platform.placedIds().isEmpty())
    }

    @Test
    fun fr12_only_a_real_cancel_is_reported_as_canceled() {
        started()
        platform.offered += clock(configure = true)
        platform.setupAtOnce = WidgetSetup.Failed("ActivityNotFoundException: no setup activity")
        add()
        val failed = sent.single { it.hasWidgetAdded() }.widgetAdded.error
        assertFalse(failed, failed.contains("canceled"))
        assertTrue(failed, failed.contains("did not start") && failed.contains("no setup activity"))
        assertEquals(listOf(1), platform.deleted)

        // A setup that cannot even be asked for is not a cancel either.
        val throwing = object : WidgetPlatform by platform {
            override fun configure(widgetId: Int, done: (WidgetSetup) -> Unit) {
                throw SecurityException("not allowed")
            }
        }
        val other = mutableListOf<Envelope>()
        val manager = WidgetManager(throwing, WidgetStore.memory(), { frame -> other += decode(frame); true }, log, { true })
        manager.start { it() }
        manager.handle(Envelope.newBuilder().setAddWidget(AddWidget.newBuilder().setRequestId(3).setProvider(clock().provider)).build())
        val error = other.single { it.hasWidgetAdded() }.widgetAdded.error
        assertFalse(error, error.contains("canceled"))
        assertTrue(error, error.contains("did not start"))
    }

    @Test
    fun fr12_the_setups_answer_is_read_from_its_result_code() {
        assertEquals(WidgetSetup.Finished, WidgetSetup.ofResult(-1))
        assertEquals(WidgetSetup.Canceled, WidgetSetup.ofResult(0))
        // A result an app made up is not a finished setup.
        assertEquals(WidgetSetup.Canceled, WidgetSetup.ofResult(1))
    }

    @Test
    fun fr12_a_setup_the_app_says_is_optional_is_skipped_and_the_widget_placed_at_once() {
        val reconfigurable = WidgetInfo.FEATURE_RECONFIGURABLE
        val optional = WidgetInfo.FEATURE_CONFIGURATION_OPTIONAL
        assertFalse("no configuration activity, no setup", WidgetInfo.needsSetup(false, 0))
        assertTrue(WidgetInfo.needsSetup(true, 0))
        assertTrue("reconfigurable alone still sets the widget up first", WidgetInfo.needsSetup(true, reconfigurable))
        assertTrue("optional means nothing without reconfigurable", WidgetInfo.needsSetup(true, optional))
        assertFalse(WidgetInfo.needsSetup(true, reconfigurable or optional))
        // Hidden from the picker (2) does not change it.
        assertFalse(WidgetInfo.needsSetup(true, reconfigurable or optional or 2))

        started()
        platform.offered += clock(configure = WidgetInfo.needsSetup(true, reconfigurable or optional))
        add()
        assertTrue("no setup runs", platform.setups.isEmpty())
        assertEquals(1, sent.single { it.hasWidgetAdded() }.widgetAdded.widgetId)
    }

    private fun reconfigure(widgetId: Int, requestId: Int) {
        manager.handle(
            Envelope.newBuilder().setReconfigureWidget(
                ReconfigureWidget.newBuilder().setRequestId(requestId).setWidgetId(widgetId),
            ).build(),
        )
        scheduler.runDue()
    }

    @Test
    fun fr12_long_pressing_a_widget_edits_it_like_a_home_screen() {
        val reconfigurable = WidgetInfo.FEATURE_RECONFIGURABLE
        val optional = WidgetInfo.FEATURE_CONFIGURATION_OPTIONAL
        // Its settings are offered only for a widget with a setup whose app says it can run again.
        assertTrue(WidgetInfo.canSetUpAgain(true, reconfigurable))
        assertTrue(WidgetInfo.canSetUpAgain(true, reconfigurable or optional))
        assertFalse("a setup only when placed", WidgetInfo.canSetUpAgain(true, 0))
        assertFalse("optional alone is not reconfigurable", WidgetInfo.canSetUpAgain(true, optional))
        assertFalse("no setup to run", WidgetInfo.canSetUpAgain(false, reconfigurable))
        assertTrue(clock().copy(reconfigurable = true).toProto().reconfigurable)
        assertFalse(clock().toProto().reconfigurable)

        started()
        platform.offered += clock().copy(reconfigurable = true)
        platform.offered += clock(provider = "com.example.notes/.NotesWidget")
        add(requestId = 1)
        add(provider = "com.example.notes/.NotesWidget", requestId = 2)
        assertTrue(platform.setups.isEmpty())
        sent.clear()

        // The user finishes the settings: the same widget ID is set up, then described and
        // drawn anew at its size, and the host hears it finished.
        reconfigure(widgetId = 1, requestId = 5)
        assertEquals(listOf(1), platform.setups)
        assertTrue("nothing is answered before the user is done", sent.isEmpty())
        platform.configuring.getValue(1)(WidgetSetup.Finished)
        scheduler.runDue()
        assertEquals(
            listOf(Envelope.BodyCase.WIDGET_UPDATED, Envelope.BodyCase.WIDGET_RECONFIGURED),
            sent.map { it.bodyCase },
        )
        assertEquals(1, sent[0].widgetUpdated.widgetId)
        assertTrue(sent[0].widgetUpdated.provider.reconfigurable)
        val done = sent[1].widgetReconfigured
        assertEquals(5, done.requestId)
        assertEquals(1, done.widgetId)
        assertTrue(done.finished)
        assertEquals(listOf(1 to WidgetSize(180, 110, 160, 180, 110)), platform.redrawn)
        assertEquals(mapOf(1 to "com.example.clock/.ClockWidget", 2 to "com.example.notes/.NotesWidget"), platform.bound.toMap())

        // The user closes them instead: the widget stays as it was, and the host hears why.
        sent.clear()
        reconfigure(widgetId = 1, requestId = 6)
        platform.configuring.getValue(1)(WidgetSetup.Canceled)
        scheduler.runDue()
        val canceled = sent.single().widgetReconfigured
        assertEquals(6, canceled.requestId)
        assertFalse(canceled.finished)
        assertTrue(canceled.error, canceled.error.contains("without a change"))
        assertEquals(1, platform.redrawn.size)
        assertTrue(platform.deleted.isEmpty())

        // A widget whose app offers no settings, and one that is not placed, run no setup.
        sent.clear()
        reconfigure(widgetId = 2, requestId = 7)
        reconfigure(widgetId = 9, requestId = 8)
        assertEquals(listOf(1, 1), platform.setups)
        assertEquals(listOf(7, 8), sent.map { it.widgetReconfigured.requestId })
        assertTrue(sent.none { it.widgetReconfigured.finished })
    }

    @Test
    fun fr12_two_setups_at_once_each_get_their_own_answer() {
        started()
        platform.offered += clock(configure = true)
        platform.offered += clock(configure = true, provider = "com.example.notes/.NotesWidget")
        add(requestId = 1)
        add(provider = "com.example.notes/.NotesWidget", requestId = 2)
        assertEquals(listOf(1, 2), platform.setups)
        platform.configuring.getValue(2)(WidgetSetup.Finished)
        platform.configuring.getValue(1)(WidgetSetup.Canceled)
        scheduler.runDue()
        val answers = sent.filter { it.hasWidgetAdded() }.associate { it.widgetAdded.requestId to it.widgetAdded }
        assertEquals(2, answers.getValue(2).widgetId)
        assertTrue(answers.getValue(1).error.contains("canceled"))
        assertEquals(listOf(2), platform.placedIds())
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

    /**
     * A size the widget is drawn at already changes nothing: the host passes the size of a
     * window being resized as the drag pauses and again at its end, and a widget drawn anew
     * makes its views again, which costs memory and time.
     */
    @Test
    fun fr12_a_size_the_widget_is_drawn_at_already_does_not_draw_it_again() {
        started()
        platform.offered += clock()
        add()
        fun resize(width: Int, widthDp: Int) = manager.handle(
            Envelope.newBuilder().setResizeWidget(
                ResizeWidget.newBuilder().setWidgetId(1).setWidth(width).setHeight(220).setDensityDpi(240)
                    .setWidthDp(widthDp).setHeightDp(147),
            ).build(),
        )
        resize(360, 240)
        val drawn = platform.shown.size
        resize(360, 240)
        resize(360, 240)
        scheduler.runDue()
        assertEquals("the same size drew the widget again", drawn, platform.shown.size)
        assertEquals(WidgetSize(360, 220, 240, 240, 147), store.sizes()[1])
        // A new size draws it once more.
        resize(420, 280)
        assertEquals(drawn + 1, platform.shown.size)
        assertEquals(1 to WidgetSize(420, 220, 240, 280, 147), platform.shown.last())
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
    fun fr12_a_widget_asked_for_before_the_user_is_unlocked_is_placed_once_the_user_is() {
        // Android reports its boot finished before the user's storage is unlocked, and the
        // host may ask for the gallery and a widget in between. Until the unlock Android
        // offers no widget, so an answer then would refuse the widget.
        var unlocked = false
        var readyCallback: (() -> Unit)? = null
        val waiting = object : WidgetPlatform by platform {
            override fun whenReady(ready: () -> Unit) {
                if (unlocked) ready() else readyCallback = ready
            }
            override fun providers(): List<WidgetInfo> {
                check(unlocked) { "User 0 must be unlocked for widgets to be available" }
                return platform.providers()
            }
        }
        platform.offered += clock()
        val manager = WidgetManager(waiting, store, { frame -> sent += decode(frame); true }, log, { connected })
        manager.start { it() }
        manager.connected()
        manager.handle(Envelope.newBuilder().setListWidgetProviders(ListWidgetProviders.newBuilder().setRequestId(4)).build())
        manager.handle(
            Envelope.newBuilder()
                .setAddWidget(AddWidget.newBuilder().setRequestId(8).setProvider("com.example.clock/.ClockWidget"))
                .build(),
        )
        assertTrue("nothing is answered while the user is locked", sent.isEmpty())
        assertTrue(platform.bound.isEmpty())

        unlocked = true
        readyCallback!!()
        val gallery = sent.first { it.bodyCase == Envelope.BodyCase.WIDGET_PROVIDERS }.widgetProviders
        assertEquals(4, gallery.requestId)
        assertEquals(listOf("com.example.clock/.ClockWidget"), gallery.providersList.map { it.provider })
        val added = sent.single { it.bodyCase == Envelope.BodyCase.WIDGET_ADDED }.widgetAdded
        assertEquals(8, added.requestId)
        assertEquals("", added.error)
        assertEquals(1, added.widgetId)
        assertEquals(mapOf(1 to "com.example.clock/.ClockWidget"), platform.bound.toMap())
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

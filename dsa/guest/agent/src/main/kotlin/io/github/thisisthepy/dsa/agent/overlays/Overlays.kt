package io.github.thisisthepy.dsa.agent.overlays

import io.github.thisisthepy.dsa.agent.tasks.AppInfoSource
import io.github.thisisthepy.dsa.agent.tasks.Messages
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.OverlayWindow
import io.github.thisisthepy.dsa.control.v1.Rect
import io.github.thisisthepy.dsa.control.v1.TaskUpdated

/**
 * One window on the guest's default display, as SurfaceFlinger lists it, as far as telling
 * the app overlay windows apart goes.
 */
data class GuestWindow(
    /** What names the window for as long as it lives: its client token. */
    val identity: Any,
    /** The package that added the window; empty when the system did not say. */
    val packageName: String,
    val ownerUid: Int,
    /** WindowManager.LayoutParams.type. */
    val type: Int,
    /** The frame of the window's surface on the display: its frame with its surface insets. */
    val surface: TaskBounds,
    /** The window's title; empty, or the package's name, when its app gave none. */
    val title: String,
    /** Whether the window is shown: it has a surface that is drawn and not fully transparent. */
    val visible: Boolean,
    /** Whether it may take the keyboard focus (FLAG_NOT_FOCUSABLE is not set). */
    val focusable: Boolean,
    /** Whether it takes pointer input (FLAG_NOT_TOUCHABLE is not set). */
    val touchable: Boolean,
    /**
     * What the window manager names the window by: the identity hash that starts its name in
     * SurfaceFlinger's list. Empty when the list did not say.
     */
    val windowName: String = "",
)

/** Where the windows of the default display come from: SurfaceFlinger's list on a device. */
interface OverlaySource {
    /**
     * Starts listening. [onChange] gets the display's windows as they are and again whenever
     * any of them changes, on any thread, and must return at once. False when the framework
     * cannot say, after which app overlay windows are not shown on the host.
     */
    fun listen(onChange: (List<GuestWindow>) -> Unit): Boolean

    /** The density the default display's windows are drawn at, in dots per inch; 0 when unknown. */
    fun density(): Int

    companion object {
        /** Lists no window. */
        val NONE = object : OverlaySource {
            override fun listen(onChange: (List<GuestWindow>) -> Unit) = false
            override fun density() = 0
        }
    }
}

/** An app overlay window as the host was told of it. */
data class OverlaySnapshot(
    val overlayId: Int,
    val packageName: String,
    val type: Int,
    val surface: TaskBounds,
    val label: String,
    val focusable: Boolean,
    val touchable: Boolean,
    val densityDpi: Int,
    /** The density its app draws this window at alone ([OverlayReporter.densityGiven]); 0 for none. */
    val windowDensityDpi: Int = 0,
)

/**
 * The density the host gave one overlay window, and the density its app draws it at, as far
 * as the agent can tell.
 *
 * The framework converts the window's views in the app's process and lays the window out
 * again; nothing reports back when that is done. The window's size on the display changes
 * when it is (a window drawn at another density is that much larger or smaller), so the
 * density the host asked for counts as drawn once the size differs from the one it had when
 * the density it is drawn at was last known.
 */
private class WindowDensity(
    /** The density the app draws the window at alone; 0 for its own. */
    var drawnAt: Int,
) {
    /** The density asked for and not yet drawn at; null when none is waited for. */
    var asked: Int? = null

    /** The window's size when [drawnAt] was last known to hold. */
    var sizeAtDrawn: Pair<Int, Int>? = null

    fun given(densityDpi: Int) {
        asked = if (densityDpi == drawnAt) null else densityDpi
    }

    /** The window is [size] now. */
    fun settle(size: Pair<Int, Int>) {
        val waited = asked
        if (waited != null && sizeAtDrawn != null && size != sizeAtDrawn) {
            drawnAt = waited
            asked = null
        }
        if (asked == null) sizeAtDrawn = size
    }
}

/** Which windows are app overlay windows. */
object OverlayRules {
    /** The first ID an overlay gets; Android's task IDs stay far below it. */
    const val FIRST_ID = 0x40000000

    /** WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY. */
    const val TYPE_APPLICATION_OVERLAY = 2038

    /**
     * The window types an app adds over other apps with the "display over other apps"
     * permission: the application overlay, and the older phone (2002), system alert (2003),
     * system overlay (2006), priority phone (2007) and system error (2010) types, which the
     * window manager still takes from an app built for Android 7.1 or older.
     */
    val TYPES = setOf(TYPE_APPLICATION_OVERLAY, 2002, 2003, 2006, 2007, 2010)

    /** The first uid of an app (Process.FIRST_APPLICATION_UID), within a user's 100000. */
    private const val FIRST_APP_UID = 10000
    private const val PER_USER = 100000

    /**
     * Whether [window] is an app's overlay the host shows: one of [TYPES], added by an app
     * (never by the system, whose own windows of these types are not the user's), not by the
     * agent itself, and on the screen now.
     */
    fun isOverlay(window: GuestWindow, ownPackage: String): Boolean =
        window.type in TYPES &&
            window.ownerUid % PER_USER >= FIRST_APP_UID &&
            window.packageName.isNotEmpty() &&
            window.packageName != ownPackage &&
            window.visible &&
            !window.surface.isEmpty()
}

/**
 * Keeps the host's picture of the guest's app overlay windows in step with the guest.
 *
 * An app overlay window (floating lyrics, a chat head, a screen filter) is in no task, so no
 * host window shows it unless the host hears of it. Each [update] takes the display's windows,
 * keeps the app overlays ([OverlayRules.isOverlay]), and tells the host the difference with the
 * messages a task is told with: TaskUpdated with `overlay` set for one that is new or whose
 * place, size, title or flags changed, and TaskRemoved for one that is gone (its app removed
 * or hid it, or its process died). The hwcomposer hears the same frames and sends each
 * overlay's layers to a host display of its own.
 *
 * An overlay gets an ID when it is first seen and keeps it while its window lives. IDs start
 * at [OverlayRules.FIRST_ID], so none is ever a task's.
 *
 * As for a task, the host is told an app's name and icon (AppInfo) before the first overlay
 * of the app, since the overlay's host window belongs to the app's own program.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class OverlayReporter(
    private val ownPackage: String,
    private val apps: AppInfoSource = AppInfoSource.NONE,
    /**
     * The density the framework says it gave a window ([GuestWindow.windowName]) before the
     * agent started, which its app may still draw it at; 0 for none. Asked only of the overlays
     * of the first list of windows.
     */
    private val givenDensity: (String) -> Int = { 0 },
    /** Sends one control frame; false when it could not be sent. */
    private val send: (ByteArray) -> Boolean,
) {
    private val ids = HashMap<Any, Int>()
    private var nextId = OverlayRules.FIRST_ID
    private val told = LinkedHashMap<Int, OverlaySnapshot>()
    private val toldApps = HashSet<String>()
    private val labels = HashMap<String, String>()
    private var stale = false
    private var display: Size? = null
    private val names = HashMap<Int, String>()
    private val densities = HashMap<Int, WindowDensity>()
    private var listed = false

    /**
     * What the window manager names the overlay's window by, while it is on the screen; null
     * for an overlay that is not, or a window the list did not name.
     */
    fun windowName(overlayId: Int): String? = names[overlayId]?.takeIf { it.isNotEmpty() }

    /**
     * The framework was asked to draw the overlay's window alone at [densityDpi] (0: at its
     * app's own). The host is told the window has it once its app has laid it out again
     * ([OverlayWindow.density_dpi]).
     */
    fun densityGiven(overlayId: Int, densityDpi: Int) {
        densities[overlayId]?.given(maxOf(0, densityDpi))
    }

    /** The overlays the host was told about and not yet told are gone. */
    val reported: Set<Int> get() = told.keys.toSet()

    fun isReported(overlayId: Int): Boolean = overlayId in told

    /** Where the host was last told each overlay's surface is. */
    val bounds: Map<Int, TaskBounds> get() = told.mapValues { it.value.surface }

    /** The apps that have an overlay the host was told about. */
    val packages: Set<String> get() = told.values.map { it.packageName }.toSet()

    /**
     * Takes the default display's [windows], [display] its size and [densityDpi] the density
     * its windows are drawn at, and tells the host what changed.
     */
    fun update(windows: List<GuestWindow>, display: Size?, densityDpi: Int) {
        if (display != this.display) {
            this.display = display
            stale = true
        }
        val overlays = windows.filter { OverlayRules.isOverlay(it, ownPackage) }
        // A window that is gone gives its ID up; one that is only hidden keeps it.
        val present = windows.map { it.identity }.toSet()
        ids.keys.retainAll(present)
        val current = LinkedHashMap<Int, GuestWindow>()
        for (window in overlays) {
            val id = ids.getOrPut(window.identity) { freshId() }
            if (id !in current) current[id] = window
        }
        for (overlayId in told.keys.toList()) {
            if (overlayId !in current && send(Messages.taskRemoved(overlayId))) told.remove(overlayId)
        }
        // A window keeps the density it was given while it lives, hidden or not.
        val living = ids.values.toSet()
        densities.keys.retainAll(living)
        names.keys.retainAll(current.keys)
        // The windows of the first list were there before the agent: the framework may have
        // given one a density the agent no longer knows of.
        val before = !listed && windows.isNotEmpty()
        if (windows.isNotEmpty()) listed = true
        for ((overlayId, window) in current) {
            names[overlayId] = window.windowName
            val density = densities.getOrPut(overlayId) {
                WindowDensity(if (before && window.windowName.isNotEmpty()) maxOf(0, givenDensity(window.windowName)) else 0)
            }
            density.settle(window.surface.right - window.surface.left to window.surface.bottom - window.surface.top)
        }
        var allSent = true
        for ((overlayId, window) in current) {
            if (!announce(window.packageName)) {
                allSent = false
                continue
            }
            val snapshot = OverlaySnapshot(
                overlayId = overlayId,
                packageName = window.packageName,
                type = window.type,
                surface = window.surface,
                label = label(window),
                focusable = window.focusable,
                touchable = window.touchable,
                densityDpi = maxOf(0, densityDpi),
                windowDensityDpi = densities[overlayId]?.drawnAt ?: 0,
            )
            if (!stale && told[overlayId] == snapshot) continue
            if (send(overlayUpdated(snapshot, display))) told[overlayId] = snapshot else allSent = false
        }
        // An app with no overlay left is told again before its next one.
        toldApps.retainAll(packages)
        labels.keys.retainAll(packages)
        if (allSent && told.keys.all { it in current }) stale = false
    }

    /**
     * Treats what the host was told as unknown, for a new connection: the next [update] sends
     * every app and overlay again, and still ends the overlays that went in the meantime.
     */
    fun resend() {
        stale = true
        toldApps.clear()
    }

    /** The next ID no overlay has. */
    private fun freshId(): Int {
        while (true) {
            val id = nextId
            nextId = if (nextId == Int.MAX_VALUE) OverlayRules.FIRST_ID else nextId + 1
            if (id !in told && id !in ids.values) return id
        }
    }

    /**
     * The name the overlay's host window carries, which a screen reader reads: the window's
     * own title when its app gave it one, else the app's name.
     */
    private fun label(window: GuestWindow): String {
        val title = window.title.trim()
        if (title.isNotEmpty() && title != window.packageName) return title
        return labels[window.packageName].orEmpty()
    }

    /** Tells the host the app's name and icon unless it was told already. */
    private fun announce(packageName: String): Boolean {
        if (packageName in toldApps) return true
        val details = try {
            apps.appInfo(packageName)
        } catch (e: Exception) {
            null
        }
        // An app the agent cannot describe still gets its overlays, under its package's name.
        if (details != null) {
            if (!send(Messages.appInfo(packageName, details))) return false
            labels[packageName] = details.label
        }
        toldApps += packageName
        return true
    }

    companion object {
        /** [overlay] as the host and the hwcomposer read it: a TaskUpdated with `overlay` set. */
        fun overlayUpdated(overlay: OverlaySnapshot, display: Size? = null): ByteArray {
            val update = TaskUpdated.newBuilder()
                .setTaskId(overlay.overlayId)
                .setBounds(
                    Rect.newBuilder()
                        .setLeft(overlay.surface.left)
                        .setTop(overlay.surface.top)
                        .setRight(overlay.surface.right)
                        .setBottom(overlay.surface.bottom),
                )
                .setPackageName(overlay.packageName)
                .setLabel(overlay.label)
                .setDensityDpi(overlay.densityDpi)
                .setOverlay(
                    OverlayWindow.newBuilder()
                        .setWindowType(overlay.type)
                        .setFocusable(overlay.focusable)
                        .setTouchable(overlay.touchable)
                        .setDensityDpi(overlay.windowDensityDpi),
                )
            if (display != null && display.width > 0 && display.height > 0) {
                update.setMaxWidth(display.width).setMaxHeight(display.height)
            }
            return Envelope.newBuilder().setTaskUpdated(update).build().toByteArray()
        }
    }
}

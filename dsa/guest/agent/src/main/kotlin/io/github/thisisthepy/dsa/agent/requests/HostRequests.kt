package io.github.thisisthepy.dsa.agent.requests

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.AppSettingsPage
import io.github.thisisthepy.dsa.control.v1.Envelope

/** What the agent does to the guest's tasks on the host's behalf. */
interface TaskActions {
    /**
     * Starts the launcher activity of [packageName] as a new task in its own freeform window,
     * or brings the app's task to the front. False when the package has no launcher activity
     * or the framework refused.
     */
    fun launchApp(packageName: String): Boolean

    /**
     * Whether [packageName] says it supports several windows, so New window may start another
     * task of it. False when it says nothing.
     */
    fun supportsMultipleWindows(packageName: String): Boolean = false

    /**
     * Starts the launcher activity of [packageName] as an additional task in its own freeform
     * window, even when the app has a task already. False when the package has no launcher
     * activity or the framework refused.
     */
    fun launchNewWindow(packageName: String): Boolean = false

    /**
     * Starts the shortcut [shortcutId] of [packageName] as a task in its own freeform window,
     * as a launcher does (LauncherApps.startShortcut). False when the app has no such shortcut,
     * it is turned off, or the framework refused.
     */
    fun launchShortcut(packageName: String, shortcutId: String): Boolean = false

    /**
     * Opens Android Settings' [page] of [packageName], or the list of every installed app when
     * it is null, as a new task in its own freeform window. A Settings without that page opens
     * the app's info instead. False when the framework refused.
     */
    fun openAppSettings(packageName: String?, page: SettingsPage = SettingsPage.DETAILS): Boolean = false

    /** Ends the task. False when the framework has no such task or refused. */
    fun closeTask(taskId: Int): Boolean

    /**
     * Asks the window manager to give the task [bounds] on the guest display. It may apply
     * them later. False when the framework refused.
     */
    fun moveTask(taskId: Int, bounds: TaskBounds): Boolean

    /**
     * Asks the window manager to give the task [bounds] and, when [densityDpi] is not null,
     * to have the task's app draw at that density, both in one change so the app relays out
     * once. It may apply them later. Returns what the framework took.
     */
    fun resizeTask(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome

    /**
     * One step of a resize the user is still dragging: asks the window manager to give the
     * task [bounds] while its configuration keeps the screen size [screen], in one change.
     * The app then lays itself out again in the new bounds instead of having its activities
     * recreated for a new configuration at every step. It may apply them later. Returns what
     * the framework took ([ResizeOutcome.BOUNDS_ONLY]: the bounds, but the screen size
     * follows them).
     */
    fun resizeTaskLive(taskId: Int, bounds: TaskBounds, screen: ScreenDp): ResizeOutcome

    /**
     * The end of a drag: asks the window manager to give the task [bounds] and to let its
     * configuration follow them again, with the density [densityDpi] when it is not null,
     * in one change, so the app is reconfigured once at its final size. It may apply them
     * later. Returns what the framework took.
     */
    fun endLiveResize(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome

    /**
     * Gives the task the input focus, and brings it to the front of its display, so the keys
     * the host sends next reach its focused window. False when the framework refused.
     */
    fun focusTask(taskId: Int): Boolean

    /**
     * Gives the task a top system bar inset covering [frame] (guest display pixels): the band
     * of the task the host's window draws its title bar over. The task's app receives it as its
     * status bar, system bar and caption bar inset. A null [frame] removes it. The caption bar
     * inset carries [boundingRects] (relative to [frame]) as the places in it the host draws
     * over, for an app that draws its own title bar; none otherwise. False when the framework
     * has no way to give a task an inset, or refused.
     *
     * With [opaque] the task's app starts below the band instead: the task gets no status bar
     * inset, and the caption bar inset is one the framework consumes for the app, which then
     * has no top inset to handle.
     */
    fun setCaptionInset(
        taskId: Int,
        frame: TaskBounds?,
        boundingRects: List<TaskBounds> = emptyList(),
        opaque: Boolean = false,
    ): Boolean

    /**
     * Puts the task in the freeform windowing mode at [freeformBounds], or, with null, in the
     * fullscreen windowing mode. False when the framework lacks the call or refused.
     */
    fun setWindowingMode(taskId: Int, freeformBounds: TaskBounds?): Boolean = false

    /**
     * The status bar's height, in pixels, that an app drawing at [densityDpi] reads from the
     * framework's resources. The image sets it to the host window's title bar at the monitor
     * scale that density belongs to. 0 when it cannot be read.
     */
    fun statusBarHeight(densityDpi: Int): Int = 0

    /**
     * Whether the framework can mark a task so that a change of density alone does not
     * relaunch its activities ([markForcedDensity]). Only an image whose framework carries
     * that change can. False when it cannot, or this cannot tell.
     */
    fun forcedDensitySupported(): Boolean = false

    /**
     * Marks the task ([forced]) so that the framework gives it a new density without relaunching
     * its activities, even those that do not say they handle a change of density, or takes the
     * mark away. The mark ends with the task. False when the framework has no such mark or
     * refused.
     */
    fun markForcedDensity(taskId: Int, forced: Boolean): Boolean = false
}

/** Which of Android Settings' pages of an app to open. */
enum class SettingsPage {
    /** The app's info: its permissions, storage and notifications. */
    DETAILS,

    /** The user's override of the app's aspect ratio. */
    ASPECT_RATIO,

    /** Whether the links the app supports open in the app or in the browser. */
    OPEN_BY_DEFAULT,
}

/** What the framework took of a resize. */
enum class ResizeOutcome {
    /** The bounds, and the density or screen size if one was asked for. */
    APPLIED,

    /**
     * The bounds, but not the density or the screen size: the framework has no way to set
     * them for one task.
     */
    BOUNDS_ONLY,

    /** Nothing. */
    REFUSED,
}

/** What became of one frame from the channel. */
sealed interface RequestResult {
    /** The app started, or came to the front; [newWindow] when it started another task. */
    data class Launched(val packageName: String, val newWindow: Boolean = false) : RequestResult
    data class LaunchFailed(val packageName: String, val reason: String) : RequestResult

    /**
     * The shortcut [shortcutId] of [packageName] started; or, with [appInstead], the shortcut
     * could not start and the app started in its place.
     */
    data class ShortcutLaunched(val packageName: String, val shortcutId: String, val appInstead: Boolean = false) : RequestResult
    /** Android's [page] of [packageName] opened; null for the list of every app. */
    data class SettingsOpened(val packageName: String?, val page: SettingsPage = SettingsPage.DETAILS) : RequestResult
    data class SettingsFailed(val packageName: String?, val reason: String) : RequestResult
    data class Closed(val taskId: Int) : RequestResult
    data class CloseRefused(val taskId: Int, val reason: String) : RequestResult

    /**
     * The task took the input focus, for an app the user set to keep its focus while its
     * window is not the active one on the desktop.
     */
    data class Focused(val taskId: Int) : RequestResult
    data class FocusRefused(val taskId: Int, val reason: String) : RequestResult

    /**
     * The host resized the task's window; the task takes [size]. [live] while the user is
     * still dragging the window's edge. The first one with [live] false after a drag ends
     * the drag, even when [size] is the size the task already has.
     */
    data class Resize(val taskId: Int, val size: Size, val live: Boolean = false) : RequestResult

    /**
     * The host's window for the task is on a monitor of another scale; it draws at [densityDpi].
     * [forced] when the user set the task's app to System (forced) scaling: the task is to take
     * the density without its activity being relaunched ([io.github.thisisthepy.dsa.agent.windowing.ForcedDensity]).
     */
    data class Density(val taskId: Int, val densityDpi: Int, val forced: Boolean = false) : RequestResult

    /**
     * The host's window for the task draws its title bar over the task's top [height] pixels;
     * the task gets a top inset of that height (0 removes it).
     */
    data class Caption(val taskId: Int, val height: Int, val startWidth: Int = 0, val endWidth: Int = 0) : RequestResult

    /**
     * Where the host's window for the task shows a clear full screen request of its app:
     * inside the window ([inWindow]), or on the whole screen.
     */
    data class FullScreen(val taskId: Int, val inWindow: Boolean) : RequestResult

    /** The apps whose tasks start below the title bar, as the host's user chose for each. */
    data class CaptionCompat(val belowTitleBar: Set<String>) : RequestResult

    /** A resize or density for a task the host was never told about. */
    data class SizeRefused(val taskId: Int, val reason: String) : RequestResult

    /** A well-formed frame that is not a request to the agent (another client's task message). */
    data class NotForTheAgent(val body: Envelope.BodyCase) : RequestResult

    /** A frame that does not decode, or a request that makes no sense; dropped. */
    data class Malformed(val reason: String) : RequestResult
}

/**
 * Turns the frames the channel delivers into actions on the guest's tasks.
 *
 * LaunchApp starts an app, or with new_window another task of an app that supports several
 * windows; LaunchShortcut starts one of an app's shortcuts, or the app when the shortcut cannot
 * start; OpenAppSettings opens one of Android's settings pages of an app; CloseTask ends a task, but only one the host was told about, so
 * the host cannot end the agent's own task or anything else that has no window. ResizeTask
 * and SetTaskDensity are checked the same way and handed back for the agent to apply with
 * the rest of the task layout, and so is SetTaskCaption, the height of the host's title bar
 * over a task. Every other message is ignored, and a frame that does not
 * decode is dropped: a bad frame never stops the agent.
 */
class HostRequests(
    private val ownPackage: String,
    private val actions: TaskActions,
    /** Whether the host was told about the task, and so may close it. */
    private val reported: (Int) -> Boolean,
) {
    fun handle(frame: ByteArray): RequestResult {
        val envelope = try {
            Envelope.parseFrom(frame)
        } catch (e: InvalidProtocolBufferException) {
            return RequestResult.Malformed("the frame is not a control message (${e.message})")
        }
        return when (envelope.bodyCase) {
            Envelope.BodyCase.LAUNCH_APP -> launch(envelope.launchApp.packageName, envelope.launchApp.newWindow)
            Envelope.BodyCase.LAUNCH_SHORTCUT ->
                launchShortcut(envelope.launchShortcut.packageName, envelope.launchShortcut.shortcutId)
            Envelope.BodyCase.OPEN_APP_SETTINGS ->
                settings(envelope.openAppSettings.packageName, pageOf(envelope.openAppSettings.page))
            Envelope.BodyCase.CLOSE_TASK -> close(envelope.closeTask.taskId)
            Envelope.BodyCase.FOCUS_TASK -> focus(envelope.focusTask.taskId)
            Envelope.BodyCase.RESIZE_TASK -> envelope.resizeTask.let {
                resize(it.taskId, it.width, it.height, it.live)
            }
            Envelope.BodyCase.SET_TASK_DENSITY -> envelope.setTaskDensity.let {
                density(it.taskId, it.densityDpi, it.forced)
            }
            Envelope.BodyCase.CAPTION_COMPAT ->
                RequestResult.CaptionCompat(envelope.captionCompat.belowTitleBarList.filter { it.isNotEmpty() }.toSet())
            Envelope.BodyCase.SET_TASK_CAPTION -> caption(
                envelope.setTaskCaption.taskId,
                envelope.setTaskCaption.height,
                envelope.setTaskCaption.startWidth,
                envelope.setTaskCaption.endWidth,
            )
            Envelope.BodyCase.SET_TASK_FULL_SCREEN -> envelope.setTaskFullScreen.let {
                if (reported(it.taskId)) {
                    RequestResult.FullScreen(it.taskId, it.inWindow)
                } else {
                    RequestResult.SizeRefused(it.taskId, "the host was never told about this task")
                }
            }
            Envelope.BodyCase.BODY_NOT_SET, null -> RequestResult.Malformed("the frame carries no message")
            else -> RequestResult.NotForTheAgent(envelope.bodyCase)
        }
    }

    private fun launch(packageName: String, newWindow: Boolean): RequestResult = when {
        packageName.isEmpty() -> RequestResult.Malformed("LaunchApp names no package")
        packageName == ownPackage -> RequestResult.LaunchFailed(packageName, "the agent has no window to launch")
        // Another task only of an app that supports several windows; any other app comes to
        // the front, as its launch does, rather than open a second copy it was not made for.
        newWindow && actions.supportsMultipleWindows(packageName) ->
            if (actions.launchNewWindow(packageName)) {
                RequestResult.Launched(packageName, newWindow = true)
            } else {
                RequestResult.LaunchFailed(packageName, "the framework refused to start another task of it")
            }
        actions.launchApp(packageName) -> RequestResult.Launched(packageName)
        else -> RequestResult.LaunchFailed(packageName, "the package has no launcher activity, or the framework refused to start it")
    }

    // A shortcut that is gone or turned off still opens its app, as a launcher's stale
    // shortcut does, so the user's click is never lost.
    private fun launchShortcut(packageName: String, shortcutId: String): RequestResult = when {
        !PACKAGE_NAME.matches(packageName) ->
            RequestResult.Malformed("LaunchShortcut names ${packageName.take(80).ifEmpty { "no package" }}, which is not a package name")
        shortcutId.isEmpty() -> RequestResult.Malformed("LaunchShortcut names no shortcut of $packageName")
        packageName == ownPackage -> RequestResult.LaunchFailed(packageName, "the agent has no shortcuts to start")
        actions.launchShortcut(packageName, shortcutId) -> RequestResult.ShortcutLaunched(packageName, shortcutId)
        actions.launchApp(packageName) -> RequestResult.ShortcutLaunched(packageName, shortcutId, appInstead = true)
        else -> RequestResult.LaunchFailed(packageName, "neither the shortcut $shortcutId nor the app could be started")
    }

    // A page this build does not know opens the app's info, the page every Settings has.
    private fun pageOf(page: AppSettingsPage): SettingsPage = when (page) {
        AppSettingsPage.APP_SETTINGS_PAGE_ASPECT_RATIO -> SettingsPage.ASPECT_RATIO
        AppSettingsPage.APP_SETTINGS_PAGE_OPEN_BY_DEFAULT -> SettingsPage.OPEN_BY_DEFAULT
        else -> SettingsPage.DETAILS
    }

    private fun settings(name: String, asked: SettingsPage): RequestResult {
        val packageName = name.ifEmpty { null }
        // The list of every app has no pages of its own.
        val page = if (packageName == null) SettingsPage.DETAILS else asked
        return when {
            packageName != null && !PACKAGE_NAME.matches(packageName) ->
                RequestResult.Malformed("OpenAppSettings names ${packageName.take(80)}, which is not a package name")
            actions.openAppSettings(packageName, page) -> RequestResult.SettingsOpened(packageName, page)
            else -> RequestResult.SettingsFailed(packageName, "Settings has no such page, or the framework refused to start it")
        }
    }

    private fun close(taskId: Int): RequestResult = when {
        !reported(taskId) -> RequestResult.CloseRefused(taskId, "the host was never told about this task")
        actions.closeTask(taskId) -> RequestResult.Closed(taskId)
        else -> RequestResult.CloseRefused(taskId, "the framework has no such task or refused to end it")
    }

    private fun focus(taskId: Int): RequestResult = when {
        !reported(taskId) -> RequestResult.FocusRefused(taskId, "the host was never told about this task")
        actions.focusTask(taskId) -> RequestResult.Focused(taskId)
        else -> RequestResult.FocusRefused(taskId, "the framework has no such task or refused to focus it")
    }

    // The sizes are unsigned on the wire; a value past Int.MAX_VALUE reads as negative here.
    private fun resize(taskId: Int, width: Int, height: Int, live: Boolean): RequestResult = when {
        width <= 0 || height <= 0 || width > MAX_LENGTH || height > MAX_LENGTH ->
            RequestResult.Malformed("ResizeTask asks for a size of ${Integer.toUnsignedString(width)}x${Integer.toUnsignedString(height)}")
        !reported(taskId) -> RequestResult.SizeRefused(taskId, "the host was never told about this task")
        else -> RequestResult.Resize(taskId, Size(width, height), live)
    }

    private fun density(taskId: Int, densityDpi: Int, forced: Boolean): RequestResult = when {
        densityDpi < MIN_DENSITY || densityDpi > MAX_DENSITY ->
            RequestResult.Malformed("SetTaskDensity asks for ${Integer.toUnsignedString(densityDpi)} dpi")
        !reported(taskId) -> RequestResult.SizeRefused(taskId, "the host was never told about this task")
        else -> RequestResult.Density(taskId, densityDpi, forced)
    }

    // The height is unsigned on the wire; a value past Int.MAX_VALUE reads as negative here.
    private fun caption(taskId: Int, height: Int, startWidth: Int, endWidth: Int): RequestResult = when {
        height < 0 || height > MAX_LENGTH ->
            RequestResult.Malformed("SetTaskCaption asks for a title bar ${Integer.toUnsignedString(height)} pixels high")
        startWidth < 0 || startWidth > MAX_LENGTH || endWidth < 0 || endWidth > MAX_LENGTH ->
            RequestResult.Malformed(
                "SetTaskCaption says its icon and buttons reach ${Integer.toUnsignedString(startWidth)} and " +
                    "${Integer.toUnsignedString(endWidth)} pixels into the title bar",
            )
        !reported(taskId) -> RequestResult.SizeRefused(taskId, "the host was never told about this task")
        else -> RequestResult.Caption(taskId, height, startWidth, endWidth)
    }

    private companion object {
        // Past any display the framework draws.
        const val MAX_LENGTH = 16384

        // The densities the hwcomposer accepts for the display.
        const val MIN_DENSITY = 72
        const val MAX_DENSITY = 960

        // Dot-separated parts of letters, digits and underscores, as Android names packages.
        val PACKAGE_NAME = Regex("[A-Za-z0-9_]+(\\.[A-Za-z0-9_]+)*")
    }
}

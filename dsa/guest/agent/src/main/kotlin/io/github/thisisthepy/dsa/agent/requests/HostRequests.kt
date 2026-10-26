package io.github.thisisthepy.dsa.agent.requests

import com.google.protobuf.InvalidProtocolBufferException
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.Envelope

/** What the agent does to the guest's tasks on the host's behalf. */
interface TaskActions {
    /**
     * Starts the launcher activity of [packageName] as a new task in its own freeform window,
     * or brings the app's task to the front. False when the package has no launcher activity
     * or the framework refused.
     */
    fun launchApp(packageName: String): Boolean

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
}

/** What the framework took of a resize. */
enum class ResizeOutcome {
    /** The bounds, and the density if one was asked for. */
    APPLIED,

    /** The bounds, but not the density: the framework has no way to set a task's density. */
    BOUNDS_ONLY,

    /** Nothing. */
    REFUSED,
}

/** What became of one frame from the channel. */
sealed interface RequestResult {
    data class Launched(val packageName: String) : RequestResult
    data class LaunchFailed(val packageName: String, val reason: String) : RequestResult
    data class Closed(val taskId: Int) : RequestResult
    data class CloseRefused(val taskId: Int, val reason: String) : RequestResult

    /**
     * The host resized the task's window; the task takes [size]. [live] while the user is
     * still dragging the window's edge. The first one with [live] false after a drag ends
     * the drag, even when [size] is the size the task already has.
     */
    data class Resize(val taskId: Int, val size: Size, val live: Boolean = false) : RequestResult

    /** The host's window for the task is on a monitor of another scale; it draws at [densityDpi]. */
    data class Density(val taskId: Int, val densityDpi: Int) : RequestResult

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
 * LaunchApp starts an app; CloseTask ends a task, but only one the host was told about, so
 * the host cannot end the agent's own task or anything else that has no window. ResizeTask
 * and SetTaskDensity are checked the same way and handed back for the agent to apply with
 * the rest of the task layout. Every other message is ignored, and a frame that does not
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
            Envelope.BodyCase.LAUNCH_APP -> launch(envelope.launchApp.packageName)
            Envelope.BodyCase.CLOSE_TASK -> close(envelope.closeTask.taskId)
            Envelope.BodyCase.RESIZE_TASK -> envelope.resizeTask.let {
                resize(it.taskId, it.width, it.height, it.live)
            }
            Envelope.BodyCase.SET_TASK_DENSITY ->
                density(envelope.setTaskDensity.taskId, envelope.setTaskDensity.densityDpi)
            Envelope.BodyCase.BODY_NOT_SET, null -> RequestResult.Malformed("the frame carries no message")
            else -> RequestResult.NotForTheAgent(envelope.bodyCase)
        }
    }

    private fun launch(packageName: String): RequestResult = when {
        packageName.isEmpty() -> RequestResult.Malformed("LaunchApp names no package")
        packageName == ownPackage -> RequestResult.LaunchFailed(packageName, "the agent has no window to launch")
        actions.launchApp(packageName) -> RequestResult.Launched(packageName)
        else -> RequestResult.LaunchFailed(packageName, "the package has no launcher activity, or the framework refused to start it")
    }

    private fun close(taskId: Int): RequestResult = when {
        !reported(taskId) -> RequestResult.CloseRefused(taskId, "the host was never told about this task")
        actions.closeTask(taskId) -> RequestResult.Closed(taskId)
        else -> RequestResult.CloseRefused(taskId, "the framework has no such task or refused to end it")
    }

    // The sizes are unsigned on the wire; a value past Int.MAX_VALUE reads as negative here.
    private fun resize(taskId: Int, width: Int, height: Int, live: Boolean): RequestResult = when {
        width <= 0 || height <= 0 || width > MAX_LENGTH || height > MAX_LENGTH ->
            RequestResult.Malformed("ResizeTask asks for a size of ${Integer.toUnsignedString(width)}x${Integer.toUnsignedString(height)}")
        !reported(taskId) -> RequestResult.SizeRefused(taskId, "the host was never told about this task")
        else -> RequestResult.Resize(taskId, Size(width, height), live)
    }

    private fun density(taskId: Int, densityDpi: Int): RequestResult = when {
        densityDpi < MIN_DENSITY || densityDpi > MAX_DENSITY ->
            RequestResult.Malformed("SetTaskDensity asks for ${Integer.toUnsignedString(densityDpi)} dpi")
        !reported(taskId) -> RequestResult.SizeRefused(taskId, "the host was never told about this task")
        else -> RequestResult.Density(taskId, densityDpi)
    }

    private companion object {
        // Past any display the framework draws.
        const val MAX_LENGTH = 16384

        // The densities the hwcomposer accepts for the display.
        const val MIN_DENSITY = 72
        const val MAX_DENSITY = 960
    }
}

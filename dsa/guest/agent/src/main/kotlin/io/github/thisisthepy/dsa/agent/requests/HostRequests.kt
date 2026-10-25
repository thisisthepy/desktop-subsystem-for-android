package io.github.thisisthepy.dsa.agent.requests

import com.google.protobuf.InvalidProtocolBufferException
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
}

/** What became of one frame from the channel. */
sealed interface RequestResult {
    data class Launched(val packageName: String) : RequestResult
    data class LaunchFailed(val packageName: String, val reason: String) : RequestResult
    data class Closed(val taskId: Int) : RequestResult
    data class CloseRefused(val taskId: Int, val reason: String) : RequestResult

    /** A well-formed frame that is not a request to the agent (another client's task message). */
    data class NotForTheAgent(val body: Envelope.BodyCase) : RequestResult

    /** A frame that does not decode, or a request that makes no sense; dropped. */
    data class Malformed(val reason: String) : RequestResult
}

/**
 * Turns the frames the channel delivers into actions on the guest's tasks.
 *
 * LaunchApp starts an app; CloseTask ends a task, but only one the host was told about, so
 * the host cannot end the agent's own task or anything else that has no window. Every other
 * message is ignored, and a frame that does not decode is dropped: a bad frame never stops
 * the agent.
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
}

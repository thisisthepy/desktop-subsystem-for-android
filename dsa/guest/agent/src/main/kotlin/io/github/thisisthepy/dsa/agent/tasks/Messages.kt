package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.Rect
import io.github.thisisthepy.dsa.control.v1.TaskRemoved
import io.github.thisisthepy.dsa.control.v1.TaskUpdated

/** The control frames the agent sends, each one encoded Envelope. */
object Messages {
    /** [task], with [display] as the largest size the guest can give it, when known. */
    fun taskUpdated(task: TaskSnapshot, display: Size? = null): ByteArray {
        val bounds = Rect.newBuilder()
            .setLeft(task.bounds.left)
            .setTop(task.bounds.top)
            .setRight(task.bounds.right)
            .setBottom(task.bounds.bottom)
        val update = TaskUpdated.newBuilder()
            .setTaskId(task.taskId)
            .setBounds(bounds)
            .setPackageName(task.packageName.orEmpty())
            .setLabel(task.label)
            .setFocused(task.focused)
            .setDensityDpi(maxOf(0, task.densityDpi))
        if (display != null && display.width > 0 && display.height > 0) {
            update.setMaxWidth(display.width).setMaxHeight(display.height)
        }
        return Envelope.newBuilder().setTaskUpdated(update).build().toByteArray()
    }

    fun taskRemoved(taskId: Int): ByteArray =
        Envelope.newBuilder().setTaskRemoved(TaskRemoved.newBuilder().setTaskId(taskId)).build().toByteArray()
}

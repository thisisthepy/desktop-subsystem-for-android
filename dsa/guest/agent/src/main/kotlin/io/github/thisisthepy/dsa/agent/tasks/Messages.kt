package io.github.thisisthepy.dsa.agent.tasks

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.control.v1.AppInfo
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.Rect
import io.github.thisisthepy.dsa.control.v1.TaskRemoved
import io.github.thisisthepy.dsa.control.v1.TaskUpdated

/** The control frames the agent sends, each one encoded Envelope. */
object Messages {
    fun taskUpdated(task: TaskSnapshot): ByteArray {
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
        return Envelope.newBuilder().setTaskUpdated(update).build().toByteArray()
    }

    fun appInfo(packageName: String, details: AppDetails): ByteArray {
        val info = AppInfo.newBuilder()
            .setPackageName(packageName)
            .setLabel(details.label)
            .setIconPng(ByteString.copyFrom(details.iconPng))
        return Envelope.newBuilder().setAppInfo(info).build().toByteArray()
    }

    fun taskRemoved(taskId: Int): ByteArray =
        Envelope.newBuilder().setTaskRemoved(TaskRemoved.newBuilder().setTaskId(taskId)).build().toByteArray()
}

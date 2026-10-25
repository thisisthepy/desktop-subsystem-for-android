package io.github.thisisthepy.dsa.agent.tasks

/** A rectangle in guest display pixels: left and top inclusive, right and bottom exclusive. */
data class TaskBounds(val left: Int, val top: Int, val right: Int, val bottom: Int) {
    /** Whether it covers no pixel: the framework has not given the task bounds. */
    fun isEmpty() = right <= left || bottom <= top

    companion object {
        val EMPTY = TaskBounds(0, 0, 0, 0)
    }
}

/** What kind of task the framework says a task is. Only app tasks get a host window. */
enum class TaskKind {
    /** An ordinary app task (the framework's standard activity type). */
    APP,

    /** Home, recents, the assistant, a dream, or anything else that is not an app's own task. */
    OTHER,
}

/** One running task as the framework describes it, read on the guest. */
data class TaskSnapshot(
    val taskId: Int,
    val kind: TaskKind,
    /** The package of the task's root activity; null when the framework names none. */
    val packageName: String?,
    val label: String,
    val bounds: TaskBounds,
    val focused: Boolean,
)

/** Reads the guest's running tasks and says when they may have changed. */
interface TaskSource {
    /** Every running task, top first. */
    fun snapshot(): List<TaskSnapshot>

    /** Calls [onChange], on any thread, whenever a task may have appeared, changed or ended. */
    fun listen(onChange: () -> Unit)
}

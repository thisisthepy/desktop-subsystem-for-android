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
    /** The density the task's app draws at, in dots per inch; 0 when the framework names none. */
    val densityDpi: Int = 0,
    /**
     * The screen size the task's configuration gives its app; null when the framework names
     * none. The host is not told it: it only says what to hold while a window is resized.
     */
    val screenDp: ScreenDp? = null,
    /**
     * The smallest size the framework gives the task, in pixels; 0 when the framework does not
     * say. The host is not told it: asking for less would leave the task at another size than
     * the one asked for.
     */
    val minWidth: Int = 0,
    val minHeight: Int = 0,
)

/**
 * A configuration's screen size in density-independent pixels (Configuration's
 * screenWidthDp, screenHeightDp and smallestScreenWidthDp): what an app's resources, layout
 * and orientation are chosen by.
 */
data class ScreenDp(val widthDp: Int, val heightDp: Int, val smallestWidthDp: Int)

/** Reads the guest's running tasks and says when they may have changed. */
interface TaskSource {
    /** Every running task, top first. */
    fun snapshot(): List<TaskSnapshot>

    /** Calls [onChange], on any thread, whenever a task may have appeared, changed or ended. */
    fun listen(onChange: () -> Unit)
}

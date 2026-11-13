package io.github.thisisthepy.dsa.agent.tasks

/**
 * One report of the system bars' appearance on the guest display, as the window manager
 * gives it to the status bar (IStatusBar.onSystemBarAttributesChanged) each time it changes.
 *
 * [appearance] holds the WindowInsetsController appearance flags of the window that controls
 * the bars: the display's focused window, which an app changes with
 * setSystemBarsAppearance, screen by screen and tab by tab. [regions] are the parts of the
 * status bar each app window under it calls for (AppearanceRegion), when the window manager
 * computes them. [packageName] is the package of the controlling window, or null when the
 * report did not say.
 */
data class BarsReport(
    val appearance: Int,
    val regions: List<AppearanceArea> = emptyList(),
    val packageName: String? = null,
)

/**
 * A part of the status bar ([bounds], in guest display pixels) and the appearance flags the
 * window under it calls for there (APPEARANCE_LIGHT_STATUS_BARS when it wants dark icons).
 */
data class AppearanceArea(val appearance: Int, val bounds: TaskBounds)

/**
 * The live status bar appearance of each task, which the host's caption buttons follow as
 * Android's own status bar icons follow it.
 *
 * A task description carries an appearance too, but the framework only updates it when an
 * activity's task description changes, so an app that draws edge to edge and switches tabs
 * leaves it stale or empty. The window manager reports the live one to the status bar instead
 * ([BarsReport]); the agent is the status bar in a guest without SystemUI, and each report
 * lands here ([report]). All app tasks share the guest display, so a report belongs to the
 * task of the window that controls the bars: the task the report's package runs in, the
 * focused one first when several do ([apply]). A report whose package runs in no app task
 * (a system window) is left out. A region that covers the top of a task decides for that
 * task instead.
 *
 * A task's appearance is light while the app asks for dark icons on its status bar
 * (APPEARANCE_LIGHT_STATUS_BARS) or caption bar (APPEARANCE_LIGHT_CAPTION_BARS, which an app
 * on a desktop window may set apart; either one set makes the bar light). Once a task has
 * asked for light, clearing it asks for dark. A task that never asked for light has expressed
 * nothing, since an app that never calls setSystemBarsAppearance has the flag clear as well,
 * and stays unknown: the host then reads the tone from the app's pixels.
 *
 * Thread safe: the window manager reports on a binder thread, and the agent reads the tasks
 * on its own.
 */
class SystemBarAppearances {
    private class Seen {
        var light = false
        var everLight = false

        fun take(light: Boolean) {
            this.light = light
            if (light) everLight = true
        }

        val appearance: StatusBarAppearance
            get() = when {
                light -> StatusBarAppearance.LIGHT
                everLight -> StatusBarAppearance.DARK
                else -> StatusBarAppearance.UNKNOWN
            }
    }

    private val pending = ArrayDeque<BarsReport>()
    private val seen = HashMap<Int, Seen>()

    /** A report from the window manager, applied at the next [apply]. */
    @Synchronized
    fun report(report: BarsReport) {
        if (pending.size >= MAX_PENDING) pending.removeFirst()
        pending.addLast(report)
    }

    /**
     * [tasks] as they are now, each with its live status bar appearance, after the reports
     * that came since the last call are given to the tasks they belong to. A task that is gone
     * is forgotten.
     */
    @Synchronized
    fun apply(tasks: List<TaskSnapshot>): List<TaskSnapshot> {
        seen.keys.retainAll(tasks.map { it.taskId }.toSet())
        while (pending.isNotEmpty()) attribute(pending.removeFirst(), tasks)
        return tasks.map { task ->
            val appearance = seen[task.taskId]?.appearance ?: StatusBarAppearance.UNKNOWN
            if (appearance == task.statusBar) task else task.copy(statusBar = appearance)
        }
    }

    private fun attribute(report: BarsReport, tasks: List<TaskSnapshot>) {
        val apps = tasks.filter { it.kind == TaskKind.APP }
        val covered = HashSet<Int>()
        for (task in apps) {
            if (task.bounds.isEmpty()) continue
            val x = (task.bounds.left + task.bounds.right) / 2
            val y = task.bounds.top
            val area = report.regions.firstOrNull { it.bounds.covers(x, y) } ?: continue
            seen.getOrPut(task.taskId, ::Seen).take(area.appearance and APPEARANCE_LIGHT_STATUS_BARS != 0)
            covered += task.taskId
        }
        val owner = controlling(report.packageName, apps) ?: return
        if (owner.taskId in covered) return
        seen.getOrPut(owner.taskId, ::Seen).take(report.appearance and LIGHT_BARS != 0)
    }

    /**
     * The task of the window that controls the bars: one the package runs in (its root or
     * its top activity's), the focused one first; the focused task when the report names no
     * package; none when the package runs in no app task, or in several of which none is
     * focused.
     */
    private fun controlling(packageName: String?, apps: List<TaskSnapshot>): TaskSnapshot? {
        if (packageName.isNullOrEmpty()) return apps.firstOrNull { it.focused }
        val runs = apps.filter { it.packageName == packageName || it.topPackage() == packageName }
        return runs.firstOrNull { it.focused } ?: runs.singleOrNull()
    }

    private fun TaskSnapshot.topPackage(): String? = stack?.top?.substringBefore('/')

    private fun TaskBounds.covers(x: Int, y: Int) = x >= left && x < right && y >= top && y < bottom

    companion object {
        /** WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS. */
        const val APPEARANCE_LIGHT_STATUS_BARS = 8

        /** WindowInsetsController.APPEARANCE_LIGHT_CAPTION_BARS. */
        const val APPEARANCE_LIGHT_CAPTION_BARS = 256

        private const val LIGHT_BARS = APPEARANCE_LIGHT_STATUS_BARS or APPEARANCE_LIGHT_CAPTION_BARS

        /** Reports kept between two reads of the tasks; older ones are outdated by then. */
        private const val MAX_PENDING = 64
    }
}

package io.github.thisisthepy.dsa.agent.tasks

/**
 * What the framework says of a task's activities, as far as Back is concerned: how many the
 * task holds ([activities], finishing ones gone), its top one ([top], by class name), whether
 * that one shows anything ([topShows]: false for a theme with android:windowNoDisplay, as a
 * trampoline that starts another activity and finishes uses), and whether the task is visible
 * ([visible]).
 */
data class TaskStack(
    val activities: Int,
    val top: String?,
    val topShows: Boolean = true,
    val visible: Boolean = true,
)

/**
 * Whether Back in a task lands on a screen the user saw, which is when the host draws the back
 * arrow in the title bar instead of the app's icon.
 *
 * The framework only tells a task's activity count and its top activity, so the agent keeps
 * its own picture of each stack from every change it reads ([update]): each activity that
 * comes on top is pushed, each that goes is popped. An activity below the top counts only
 * when the user saw it as a screen: the user touched, clicked, scrolled or typed in the task
 * while it was on top ([interacted]), or it stayed on top and visible for [SEEN_AFTER_MS]. A
 * splash or launch activity that starts the app's main screen right away and stays under it
 * (as many apps leave theirs) is never counted, nor is one whose theme draws nothing, nor one
 * that was never on top (an app that starts two activities at once). Back on the main screen
 * of such an app would go to the splash or end the app, so the title bar keeps the icon there.
 *
 * A task the agent first reads already past its first activity (the agent started after it)
 * counts the activities below its top as seen, since nothing says otherwise.
 *
 * What the app itself does with Back (an OnBackPressedDispatcher callback that closes a drawer)
 * lives in the app's process. The framework holds it for its back gesture, and hands it out
 * only by starting a back navigation, which is not something to do on every task change.
 */
class BackStacks {
    private class Screen(val activity: String?, val since: Long, var seen: Boolean, val shows: Boolean)

    private val stacks = HashMap<Int, MutableList<Screen>>()

    /**
     * The task's stack as the framework describes it at [now] (milliseconds). Returns whether
     * Back in the task lands on a screen the user saw.
     */
    @Synchronized
    fun update(taskId: Int, stack: TaskStack, now: Long): Boolean {
        val count = stack.activities.coerceAtLeast(0)
        val screens = stacks[taskId] ?: mutableListOf<Screen>().also {
            stacks[taskId] = it
            // Read for the first time: what is below the top is taken as seen.
            repeat((count - 1).coerceAtLeast(0)) { _ -> it.add(Screen(null, now, seen = true, shows = true)) }
            if (count > 0) it.add(Screen(stack.top, now, seen = false, shows = stack.topShows))
        }
        screens.lastOrNull()?.let { settle(it, now, stack.visible) }
        when {
            count > screens.size -> {
                // Those between the old top and the new one were never on top.
                while (screens.size < count - 1) screens.add(Screen(null, now, seen = false, shows = false))
                screens.add(Screen(stack.top, now, seen = false, shows = stack.topShows))
            }
            count < screens.size -> {
                while (screens.size > count) screens.removeAt(screens.size - 1)
                replaceTop(screens, stack, now)
            }
            else -> replaceTop(screens, stack, now)
        }
        return canGoBack(screens)
    }

    /** The user acted on the task (a press, a scroll, a key other than Back): its top is a screen they saw. */
    @Synchronized
    fun interacted(taskId: Int) {
        val top = stacks[taskId]?.lastOrNull() ?: return
        if (top.shows) top.seen = true
    }

    /** Forgets the tasks that are gone. */
    @Synchronized
    fun retain(taskIds: Collection<Int>) {
        stacks.keys.retainAll(taskIds.toSet())
    }

    /** The top that came back or was swapped in for another: a screen the user has yet to see. */
    private fun replaceTop(screens: MutableList<Screen>, stack: TaskStack, now: Long) {
        val top = screens.lastOrNull() ?: return
        if (stack.top != null && top.activity != stack.top) {
            val known = top.activity == null && top.seen
            screens[screens.size - 1] = Screen(stack.top, now, seen = known, shows = stack.topShows)
        }
    }

    /** The top has been on screen since it came; long enough, it counts as seen. */
    private fun settle(top: Screen, now: Long, visible: Boolean) {
        if (top.shows && visible && now - top.since >= SEEN_AFTER_MS) top.seen = true
    }

    private fun canGoBack(screens: List<Screen>): Boolean =
        screens.dropLast(1).any { it.seen && it.shows }

    companion object {
        /**
         * How long a screen stays on top, visible, before it counts as one the user saw without
         * acting on it: longer than a splash screen waits, shorter than reading a page.
         */
        const val SEEN_AFTER_MS = 4_000L
    }
}

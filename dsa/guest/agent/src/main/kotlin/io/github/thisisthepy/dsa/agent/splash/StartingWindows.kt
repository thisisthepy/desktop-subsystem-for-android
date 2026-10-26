package io.github.thisisthepy.dsa.agent.splash

/** A starting window on screen. */
interface StartingWindow {
    fun remove()
}

/**
 * The starting window of each task. A task has at most one: a new one replaces the old. The
 * window manager asks for a removal by task, also for a task the agent drew nothing for.
 */
class StartingWindows {
    private val windows = mutableMapOf<Int, StartingWindow>()

    fun tasks(): Set<Int> = windows.keys.toSet()

    /**
     * Shows the window [create] makes for [taskId], after removing the task's old one. A
     * null window (it could not be added) is not kept. Returns what failed to be removed.
     */
    fun add(taskId: Int, create: () -> StartingWindow?): List<Throwable> {
        val errors = remove(taskId)
        create()?.let { windows[taskId] = it }
        return errors
    }

    /** Removes the task's window, if it has one. Returns the error removing it threw, if any. */
    fun remove(taskId: Int): List<Throwable> {
        val window = windows.remove(taskId) ?: return emptyList()
        return listOfNotNull(runCatching { window.remove() }.exceptionOrNull())
    }

    /** Removes every window; one that fails does not keep the others. */
    fun clear(): List<Throwable> = windows.keys.toList().flatMap { remove(it) }
}

/** The surface calls [TaskLeashes] makes. */
interface LeashSurfaces<L> {
    /** Puts [leash] at ([x], [y]) in its parent, and removes its crop when [clearCrop]. */
    fun place(leash: L, x: Int, y: Int, clearCrop: Boolean)

    fun release(leash: L)
}

/**
 * The surface (leash) of each organized task. The window manager leaves an organized task's
 * surface position and crop to its organizer, so the agent puts it where the window manager
 * would: at the task's position in its parent. The crop the surface had before it was
 * organized holds the task's size at that moment, and nothing would update it, so it is
 * removed; a task's windows lie inside its bounds. Visibility stays with the window manager.
 */
class TaskLeashes<L>(private val surfaces: LeashSurfaces<L>) {
    private class Entry<L>(val leash: L, var x: Int, var y: Int)

    private val tasks = mutableMapOf<Int, Entry<L>>()

    fun appeared(taskId: Int, leash: L, x: Int, y: Int) {
        tasks.remove(taskId)?.let { surfaces.release(it.leash) }
        tasks[taskId] = Entry(leash, x, y)
        surfaces.place(leash, x, y, clearCrop = true)
    }

    fun changed(taskId: Int, x: Int, y: Int) {
        val entry = tasks[taskId] ?: return
        if (entry.x == x && entry.y == y) return
        entry.x = x
        entry.y = y
        surfaces.place(entry.leash, x, y, clearCrop = false)
    }

    fun vanished(taskId: Int) {
        tasks.remove(taskId)?.let { surfaces.release(it.leash) }
    }
}

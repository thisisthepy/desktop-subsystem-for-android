package io.github.thisisthepy.dsa.agent.splash

/** A starting window on screen. */
interface StartingWindow {
    fun remove()

    /**
     * Draws the window's content again at [densityDpi], in place: its icon at the pixel size
     * it has at that density, loaded from bitmaps fine enough for it.
     */
    fun redraw(densityDpi: Int) {}
}

/**
 * The starting window of each task. A task has at most one: a new one replaces the old. The
 * window manager asks for a removal by task, also for a task the agent drew nothing for.
 *
 * Each window is drawn at its task's density. A task takes the density of its host window's
 * monitor only once that window is open, which is often after the starting window was drawn,
 * and again whenever the window moves to a monitor of another scale. A starting window drawn
 * at the old density would show its icon scaled, so it is drawn again at the new one.
 */
class StartingWindows {
    private class Entry(val window: StartingWindow, var densityDpi: Int)

    private val windows = mutableMapOf<Int, Entry>()

    fun tasks(): Set<Int> = windows.keys.toSet()

    /** The density the task's window was last drawn at, if it has one. */
    fun densityOf(taskId: Int): Int? = windows[taskId]?.densityDpi

    /**
     * Shows the window [create] makes for [taskId], drawn at [densityDpi], after removing the
     * task's old one. A null window (it could not be added) is not kept. Returns what failed
     * to be removed.
     */
    fun add(taskId: Int, densityDpi: Int, create: () -> StartingWindow?): List<Throwable> {
        val errors = remove(taskId)
        create()?.let { windows[taskId] = Entry(it, densityDpi) }
        return errors
    }

    /**
     * The task now draws at [densityDpi]. Its window, if it has one drawn at another density,
     * is drawn again at this one. Returns the error drawing it threw, if any; the window then
     * stays as it was, and is tried again on the next change.
     */
    fun densityChanged(taskId: Int, densityDpi: Int): List<Throwable> {
        if (densityDpi <= 0) return emptyList()
        val entry = windows[taskId] ?: return emptyList()
        if (entry.densityDpi == densityDpi) return emptyList()
        val error = runCatching { entry.window.redraw(densityDpi) }.exceptionOrNull()
        if (error == null) entry.densityDpi = densityDpi
        return listOfNotNull(error)
    }

    /** Removes the task's window, if it has one. Returns the error removing it threw, if any. */
    fun remove(taskId: Int): List<Throwable> {
        val entry = windows.remove(taskId) ?: return emptyList()
        return listOfNotNull(runCatching { entry.window.remove() }.exceptionOrNull())
    }

    /** Removes every window; one that fails does not keep the others. */
    fun clear(): List<Throwable> = windows.keys.toList().flatMap { remove(it) }
}

/** The surface calls [TaskLeashes] makes. */
interface LeashSurfaces<L> {
    /** Puts [leash] at ([x], [y]) in its parent and crops it to [width] by [height] from there. */
    fun place(leash: L, x: Int, y: Int, width: Int, height: Int)

    fun release(leash: L)
}

/** Where a task's surface goes in its parent, and its size, in display pixels. */
data class LeashPlace(val x: Int, val y: Int, val width: Int, val height: Int)

/**
 * The surface (leash) of each organized task. The window manager leaves an organized task's
 * surface position and crop to its organizer, so the agent sets both as the window manager
 * would: at the task's position in its parent, cropped to the task's size.
 *
 * The crop is what the input dispatcher hit-tests by. An app's activity window is touch
 * modal, and the window manager bounds the area it takes pointer input in by its task's
 * surface crop. A task surface with no crop is unbounded, so the topmost task would take
 * every click, wheel turn and hover on the display, at points outside its own window, and
 * the task under the pointer would get none. The crop follows each change of size, so a
 * resized task takes input across its new bounds. A task whose size is not known keeps the
 * crop it has. Visibility stays with the window manager.
 */
class TaskLeashes<L>(private val surfaces: LeashSurfaces<L>) {
    private class Entry<L>(val leash: L, var place: LeashPlace)

    private val tasks = mutableMapOf<Int, Entry<L>>()

    fun appeared(taskId: Int, leash: L, place: LeashPlace) {
        tasks.remove(taskId)?.let { surfaces.release(it.leash) }
        tasks[taskId] = Entry(leash, place)
        surfaces.place(leash, place.x, place.y, place.width, place.height)
    }

    fun changed(taskId: Int, place: LeashPlace) {
        val entry = tasks[taskId] ?: return
        if (entry.place == place) return
        entry.place = place
        surfaces.place(entry.leash, place.x, place.y, place.width, place.height)
    }

    fun vanished(taskId: Int) {
        tasks.remove(taskId)?.let { surfaces.release(it.leash) }
    }
}

package io.github.thisisthepy.dsa.agent.accessibility

/**
 * The agent's latest focus trace lines, kept in memory so the host's diagnostics archive
 * (`dsa logs collect`) carries them as `android/focus-trace.txt` beside the host service's and
 * the window's lines of the same moves. Android's log alone loses them: its buffer is shared
 * with every app and the system, and a busy minute overwrites the agent's lines before anyone
 * reads them. Only the last [capacity] lines are kept, so it never grows.
 */
class FocusTraceLog(private val capacity: Int = CAPACITY) {
    private val lines = ArrayDeque<String>()

    @Synchronized
    fun add(line: String) {
        if (lines.size == capacity) lines.removeFirst()
        lines.addLast(line)
    }

    /** The kept lines, oldest first, one per line; empty when no focus moved. */
    @Synchronized
    fun text(): String = if (lines.isEmpty()) "" else lines.joinToString("\n", postfix = "\n")

    companion object {
        /** About 2000 moves, a few hundred KiB at most. */
        const val CAPACITY = 2000
    }
}

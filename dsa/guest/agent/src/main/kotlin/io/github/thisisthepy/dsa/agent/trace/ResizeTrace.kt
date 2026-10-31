package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.windowing.Size
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/**
 * The agent's part of the resize trace: when a ResizeTask arrived, when the agent had
 * applied it to the framework, and when the task's bounds read back at the new size.
 *
 * Lines look like the host's, so the logs can be merged on their `utc` time:
 * `resize-trace task=7 size=800x600 stage=agent.received utc=2026-10-26T06:23:03.123Z mono=1234.000 live=true`.
 * `mono` is this process's monotonic clock in milliseconds. The per-stage lines are logged
 * only when [detailed] says so, at most once per task, stage and size in a row. A summary
 * line (`stage=agent.summary`) is always logged when a task's bounds take the size asked
 * for, at most once per task every [SUMMARY_INTERVAL_MILLIS], with the number left out.
 */
class ResizeTrace(
    private val log: Log,
    private val detailed: () -> Boolean = { false },
    private val monoMillis: () -> Double = { System.nanoTime() / 1_000_000.0 },
    private val wallMillis: () -> Long = System::currentTimeMillis,
) {
    private class Pending(val size: Size, val receivedAt: Double) {
        var appliedAt: Double? = null
    }

    private val pending = HashMap<Int, Pending>()
    private val stages = HashMap<Pair<Int, String>, Size>()
    private val summaries = HashMap<Int, Pair<Double, Int>>()

    /** The host asked for task [taskId] to take [size]. */
    fun received(taskId: Int, size: Size, live: Boolean) {
        pending[taskId] = Pending(size, monoMillis())
        stage(taskId, "agent.received", size, "live=$live")
    }

    /**
     * The agent has handed every size it was asked for to the framework (the arranger's
     * pass, which applies them in window container transactions, returned). [tookMillis] is
     * how long the pass took.
     */
    fun applied(tookMillis: Double) {
        val now = monoMillis()
        for ((taskId, resize) in pending) {
            if (resize.appliedAt != null) continue
            resize.appliedAt = now
            stage(taskId, "agent.applied", resize.size, "took=${fmt(tookMillis)}")
        }
    }

    /** Task [taskId] reads back with bounds of [width] x [height]. */
    fun boundsRead(taskId: Int, width: Int, height: Int) {
        val resize = pending[taskId] ?: return
        if (resize.size.width != width || resize.size.height != height) return
        pending.remove(taskId)
        val now = monoMillis()
        val applied = resize.appliedAt
        val appliedPart = if (applied == null) "applied=none" else "applied=${fmt(applied - resize.receivedAt)}"
        stage(taskId, "agent.bounds", resize.size, "since_received=${fmt(now - resize.receivedAt)}")
        summary(taskId, resize.size, "$appliedPart bounds=${fmt(now - resize.receivedAt)}")
    }

    /** The task has ended. */
    fun forget(taskId: Int) {
        pending.remove(taskId)
        summaries.remove(taskId)
        stages.keys.removeAll { it.first == taskId }
    }

    private fun stage(taskId: Int, stage: String, size: Size, detail: String) {
        if (!detailed()) return
        if (stages.put(taskId to stage, size) == size) return
        log.info(line(taskId, stage, size, detail))
    }

    private fun summary(taskId: Int, size: Size, detail: String) {
        val now = monoMillis()
        val last = summaries[taskId]
        if (last != null && now - last.first < SUMMARY_INTERVAL_MILLIS) {
            summaries[taskId] = last.first to last.second + 1
            return
        }
        summaries[taskId] = now to 0
        val skipped = last?.second ?: 0
        val full = if (skipped > 0) "$detail skipped=$skipped" else detail
        log.info(line(taskId, "agent.summary", size, full))
    }

    private fun line(taskId: Int, stage: String, size: Size, detail: String): String {
        val utc = UTC.format(Instant.ofEpochMilli(wallMillis()))
        val mono = String.format(java.util.Locale.ROOT, "%.3f", monoMillis())
        val tail = if (detail.isEmpty()) "" else " $detail"
        return "$TAG task=$taskId size=${size.width}x${size.height} stage=$stage utc=$utc mono=$mono$tail"
    }

    private fun fmt(millis: Double) = String.format(java.util.Locale.ROOT, "%.1f", millis)

    companion object {
        const val TAG = "resize-trace"
        const val SUMMARY_INTERVAL_MILLIS = 500.0
        private val UTC: DateTimeFormatter =
            DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)
    }
}

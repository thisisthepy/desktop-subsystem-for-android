package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.windowing.Size
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/**
 * The agent's part of the resize trace: when a ResizeTask arrived, how long it waited for the
 * agent's thread (`queued=`), when the agent had applied it to the framework, and when the
 * task's bounds read back at the new size.
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
    private class Pending(val size: Size, val receivedAt: Double, val queuedMillis: Double?) {
        var appliedAt: Double? = null

        /** The size the agent gave the task for [size], when it is another one. */
        var given: Size? = null

        /** The size the task's bounds read back at once the resize has landed. */
        val landing: Size get() = given ?: size
    }

    private val pending = HashMap<Int, Pending>()
    private val stages = HashMap<Pair<Int, String>, Size>()
    private val summaries = HashMap<Int, Pair<Double, Int>>()

    /** The size each task was last given in place of the one asked, as last logged. */
    private val clamps = HashMap<Int, Size>()

    /**
     * The host asked for task [taskId] to take [size]. [queuedMillis], when known, is how long
     * the request waited for the agent's thread after it arrived. The same size asked for again
     * before the bounds read back at it (the host reconfigures a task at the size it gave it
     * live) keeps the first request's time, so the summary measures from the first.
     */
    fun received(taskId: Int, size: Size, live: Boolean, queuedMillis: Double? = null) {
        val waiting = pending[taskId]
        if (waiting == null || waiting.size != size) pending[taskId] = Pending(size, monoMillis(), queuedMillis)
        val queued = if (queuedMillis == null) "" else " queued=${fmt(queuedMillis)}"
        stage(taskId, "agent.received", size, "live=$live$queued")
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

    /**
     * The agent asked the framework to give task [taskId] [width] x [height]. When that is
     * not the size the host asked for (the agent keeps a task within its smallest size and the
     * display), the resize lands at the size given: its summary carries that size, with the
     * one asked as `asked=`, and the guest's log says so once for each size given in a row.
     */
    fun asked(taskId: Int, width: Int, height: Int) {
        val resize = pending[taskId] ?: return
        val given = Size(width, height)
        if (given == resize.size) {
            resize.given = null
            clamps.remove(taskId)
            return
        }
        resize.given = given
        if (clamps.put(taskId, given) == given) return
        log.info(
            "task $taskId takes ${width}x$height, not the ${resize.size.width}x${resize.size.height} " +
                "the host asked for: a task stays within its smallest size and the display",
        )
    }

    /** Task [taskId] reads back with bounds of [width] x [height]. */
    fun boundsRead(taskId: Int, width: Int, height: Int) {
        val resize = pending[taskId] ?: return
        val landing = resize.landing
        if (landing.width != width || landing.height != height) return
        pending.remove(taskId)
        val now = monoMillis()
        val applied = resize.appliedAt
        val appliedPart = if (applied == null) "applied=none" else "applied=${fmt(applied - resize.receivedAt)}"
        stage(taskId, "agent.bounds", landing, "since_received=${fmt(now - resize.receivedAt)}")
        val queuedPart = resize.queuedMillis?.let { " queued=${fmt(it)}" } ?: ""
        val askedPart = if (resize.given == null) "" else " asked=${resize.size.width}x${resize.size.height}"
        summary(taskId, landing, "$appliedPart bounds=${fmt(now - resize.receivedAt)}$queuedPart$askedPart")
    }

    /** The task has ended. */
    fun forget(taskId: Int) {
        pending.remove(taskId)
        summaries.remove(taskId)
        clamps.remove(taskId)
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

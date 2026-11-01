package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Collections
import java.util.Locale

/**
 * The app's part of the input trace: for each frame an app drew around a press, how long its
 * main thread and its render thread took over each stage. The input trace's other lines stop
 * where the app takes the event and start again where the composer gets its frame; these say
 * what happened in between.
 *
 * A second after a press on one of an app's tasks is released, it reads the frame timings
 * the app's renderer keeps for its last 120 frames (what `dumpsys gfxinfo <package>
 * framestats` prints) and logs each frame not logged before, one line each:
 *
 * `input-trace stage=app.frame pkg=com.android.settings window=0 utc=... mono=1234.567 late=2.1
 * input=0.3 animation=0.1 traversal=4.0 record=1.2 main=5.6 sync_wait=0.1 sync=0.4 issue=3.0
 * swap=0.5 render=3.9 dequeue=0.2 queue=0.1 gpu=2.5 total=11.0 flags=0`
 *
 * `mono` is when the frame was meant to start (its intended vsync), on the clock the other
 * trace lines' `mono` uses. The main thread's stages are `late` (from the vsync to taking
 * input), `input`, `animation`, `traversal` (measure and layout) and `record` (drawing the
 * views into a display list), which add up to `main`. The render thread's are `sync`, `issue`
 * (sending the drawing to the GPU) and `swap`, which add up to `render`; `sync_wait` is how
 * long the main thread's frame waited for it. `dequeue` and `queue` are the buffer queue's
 * share of `issue` and `swap`, and `gpu` is from issuing to the GPU finishing. All in
 * milliseconds; `-` where the app did not record the stage. A frame that waited for an
 * activity to start shows it as `late`.
 *
 * With [surfaceFlinger], SurfaceFlinger's timings of the app's layers are read at the same time
 * ([SurfaceFlingerTrace]).
 *
 * The read runs on [scheduler]'s thread, which must not be one input or the agent's work waits
 * on. Off unless the boot asks for the input trace.
 */
class FrameStatsTrace(
    private val log: Log,
    private val enabled: Boolean,
    private val scheduler: Scheduler,
    private val dump: (packageName: String) -> String?,
    private val delayMillis: Long = DELAY_MILLIS,
    private val monoMillis: () -> Double = { System.nanoTime() / 1_000_000.0 },
    private val wallMillis: () -> Long = System::currentTimeMillis,
    private val surfaceFlinger: SurfaceFlingerTrace? = null,
) {
    private val pending: MutableSet<String> = Collections.synchronizedSet(mutableSetOf())

    /** The intended vsync of the last frame logged for each package, read on [scheduler]'s thread. */
    private val logged = mutableMapOf<String, Long>()

    /** A press on a task of [packageName] (null when the task's package is unknown) was released. */
    fun released(packageName: String?) {
        if (!enabled || packageName == null || !pending.add(packageName)) return
        scheduler.postDelayed(delayMillis) {
            pending.remove(packageName)
            surfaceFlinger?.read(packageName)
            val text = try {
                dump(packageName)
            } catch (e: Exception) {
                log.warn("could not read the frame timings of $packageName", e)
                null
            } ?: return@postDelayed
            val after = logged[packageName] ?: Long.MIN_VALUE
            val frames = parse(text).filter { it.intendedVsync > after }.sortedBy { it.intendedVsync }
            for (frame in frames) log.info(line(packageName, frame))
            frames.lastOrNull()?.let { logged[packageName] = it.intendedVsync }
        }
    }

    private fun line(packageName: String, frame: Frame): String {
        val mono = frame.intendedVsync / NANOS_PER_MILLI
        val utc = UTC.format(Instant.ofEpochMilli(wallMillis() - (monoMillis() - mono).toLong()))
        val stages = STAGES.joinToString(" ") { (name, span) -> "$name=${span(frame)}" }
        return "${InputTrace.TAG} stage=app.frame pkg=$packageName window=${frame.window} utc=$utc " +
            "mono=${String.format(Locale.ROOT, "%.3f", mono)} $stages flags=${frame["Flags"] ?: 0}"
    }

    /** One frame's timings, by the column names the renderer prints, in nanoseconds. */
    data class Frame(val window: Int, val columns: Map<String, Long>) {
        operator fun get(name: String): Long? = columns[name]

        val intendedVsync: Long get() = columns["IntendedVsync"] ?: 0
    }

    companion object {
        const val DELAY_MILLIS = 1_000L
        private const val NANOS_PER_MILLI = 1_000_000.0
        private const val MARKER = "---PROFILEDATA---"
        private val UTC: DateTimeFormatter =
            DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)

        /** From one timestamp to another; `-` when either was not recorded. */
        private fun between(from: String, to: String): (Frame) -> String = { frame ->
            val start = frame[from] ?: 0
            val end = frame[to] ?: 0
            if (start <= 0 || end <= 0 || end < start) "-" else millis(end - start)
        }

        /** A duration the renderer records as one; `-` when it was not recorded. */
        private fun duration(name: String): (Frame) -> String = { frame ->
            val value = frame[name] ?: -1
            if (value < 0) "-" else millis(value)
        }

        private fun millis(nanos: Long) = String.format(Locale.ROOT, "%.1f", nanos / NANOS_PER_MILLI)

        private val STAGES: List<Pair<String, (Frame) -> String>> = listOf(
            "late" to between("IntendedVsync", "HandleInputStart"),
            "input" to between("HandleInputStart", "AnimationStart"),
            "animation" to between("AnimationStart", "PerformTraversalsStart"),
            "traversal" to between("PerformTraversalsStart", "DrawStart"),
            "record" to between("DrawStart", "SyncQueued"),
            "main" to between("HandleInputStart", "SyncQueued"),
            "sync_wait" to between("SyncQueued", "SyncStart"),
            "sync" to between("SyncStart", "IssueDrawCommandsStart"),
            "issue" to between("IssueDrawCommandsStart", "SwapBuffers"),
            "swap" to between("SwapBuffers", "FrameCompleted"),
            "render" to between("SyncStart", "FrameCompleted"),
            "dequeue" to duration("DequeueBufferDuration"),
            "queue" to duration("QueueBufferDuration"),
            "gpu" to between("IssueDrawCommandsStart", "GpuCompleted"),
            "total" to between("IntendedVsync", "FrameCompleted"),
        )

        /**
         * The frames in a framestats dump. Each of the app's windows has a block between two
         * `---PROFILEDATA---` lines: a header of column names, then one line per frame.
         */
        fun parse(dump: String): List<Frame> {
            val frames = mutableListOf<Frame>()
            var window = -1
            var header: List<String>? = null
            var inside = false
            for (raw in dump.lineSequence()) {
                val line = raw.trim()
                if (line == MARKER) {
                    inside = !inside
                    header = null
                    if (inside) window++
                    continue
                }
                if (!inside || line.isEmpty()) continue
                val cells = line.split(',').map(String::trim).let { if (it.last().isEmpty()) it.dropLast(1) else it }
                if (header == null) {
                    header = cells
                    continue
                }
                val values = cells.map { it.toLongOrNull() }
                if (values.size != header.size || values.any { it == null }) continue
                frames += Frame(window, header.zip(values.map { it!! }).toMap())
            }
            return frames
        }
    }
}

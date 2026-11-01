package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.Log
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Locale

/**
 * SurfaceFlinger's part of the input trace: when each frame of an app's layers was meant to be
 * shown, when its buffer was ready, and when SurfaceFlinger actually presented it. The app's
 * own lines ([FrameStatsTrace]) end when its render thread has queued the buffer; the
 * hwcomposer's begin when SurfaceFlinger hands it the layers. These say what happened in
 * between.
 *
 * Read alongside the app's frame timings: it lists SurfaceFlinger's layers
 * (`dumpsys SurfaceFlinger --list`), keeps those whose name contains the package name, and
 * reads each one's timings (`dumpsys SurfaceFlinger --latency <layer>`). Each frame not logged
 * before is one line:
 *
 * `input-trace stage=sf.frame pkg=com.android.settings layer=com.android.settings/...#12 utc=...
 * mono=1234.567 desired=1234.567 ready=1236.100 actual=1250.900`
 *
 * All times are milliseconds on the guest's `CLOCK_MONOTONIC`, the clock of the other guest
 * lines' `mono`; `mono` is `desired`. Whitespace in a layer's name becomes `_`, so the line
 * splits on spaces.
 */
class SurfaceFlingerTrace(
    private val log: Log,
    private val list: () -> String?,
    private val latency: (layer: String) -> String?,
    private val monoMillis: () -> Double = { System.nanoTime() / 1_000_000.0 },
    private val wallMillis: () -> Long = System::currentTimeMillis,
) {
    /** The desired present time of the last frame logged for each layer. */
    private val logged = mutableMapOf<String, Long>()

    /** Logs the new frames of [packageName]'s layers. Runs on the caller's thread. */
    fun read(packageName: String) {
        val layers = try {
            list()?.let { layersOf(it, packageName) }
        } catch (e: Exception) {
            log.warn("could not list SurfaceFlinger's layers", e)
            null
        } ?: return
        for (layer in layers) {
            val text = try {
                latency(layer)
            } catch (e: Exception) {
                log.warn("could not read SurfaceFlinger's timings of $layer", e)
                null
            } ?: continue
            val after = logged[layer] ?: Long.MIN_VALUE
            val frames = newFrames(parseLatency(text), after)
            for (frame in frames) log.info(line(packageName, layer, frame))
            frames.lastOrNull()?.let { logged[layer] = it.desired }
        }
    }

    private fun line(packageName: String, layer: String, frame: Frame): String {
        val mono = frame.desired / NANOS_PER_MILLI
        val utc = UTC.format(Instant.ofEpochMilli(wallMillis() - (monoMillis() - mono).toLong()))
        return "${InputTrace.TAG} stage=sf.frame pkg=$packageName layer=${layer.replace(WHITESPACE, "_")} " +
            "utc=$utc mono=${ms(frame.desired)} desired=${ms(frame.desired)} ready=${ms(frame.ready)} " +
            "actual=${ms(frame.actual)}"
    }

    /** One frame of a layer, in nanoseconds on `CLOCK_MONOTONIC`. */
    data class Frame(val desired: Long, val actual: Long, val ready: Long)

    companion object {
        private const val NANOS_PER_MILLI = 1_000_000.0
        private val WHITESPACE = Regex("\\s+")
        private val UTC: DateTimeFormatter =
            DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)

        private fun ms(nanos: Long) = String.format(Locale.ROOT, "%.3f", nanos / NANOS_PER_MILLI)

        /** The layers in a `--list` dump whose names contain [packageName], each once. */
        fun layersOf(list: String, packageName: String): List<String> =
            list.lineSequence()
                .map(String::trim)
                .filter { it.isNotEmpty() && it.contains(packageName) }
                .distinct()
                .toList()

        /**
         * The frames in a `--latency` dump: the refresh period on the first line, then one frame
         * a line, `desired actual ready`. Slots never used are all zero and are left out; a frame
         * still on its way has [Long.MAX_VALUE] in a column and is kept, for [newFrames] to wait on.
         */
        fun parseLatency(dump: String): List<Frame> {
            val frames = mutableListOf<Frame>()
            for ((index, raw) in dump.lineSequence().withIndex()) {
                if (index == 0) continue
                val cells = raw.trim().split(WHITESPACE)
                if (cells.size != 3) continue
                val parsed = cells.map(String::toLongOrNull)
                if (parsed.any { it == null }) continue
                val values = parsed.map { it!! }
                if (values.all { it == 0L }) continue
                frames += Frame(desired = values[0], actual = values[1], ready = values[2])
            }
            return frames
        }

        /**
         * The frames meant to be shown after [after], in order, up to the first still on its way,
         * which is logged once it has been presented.
         */
        fun newFrames(frames: List<Frame>, after: Long): List<Frame> =
            frames.filter { it.desired > after }
                .sortedBy { it.desired }
                .takeWhile { it.desired != Long.MAX_VALUE && it.actual != Long.MAX_VALUE && it.ready != Long.MAX_VALUE }
    }
}

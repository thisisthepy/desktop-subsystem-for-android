package io.github.thisisthepy.dsa.agent.platform

import android.os.IBinder
import android.os.ParcelFileDescriptor
import java.io.ByteArrayOutputStream

/**
 * Reads an app's frame timings, as `dumpsys gfxinfo <package> framestats` does: the activity
 * manager's graphics service asks each of the package's processes to write what its renderer
 * recorded for its last frames. It needs the DUMP permission. SurfaceFlinger's layer timings
 * come the same way, from SurfaceFlinger's own service.
 *
 * The output comes back through a pipe, read on a thread of its own so that a long dump cannot
 * fill the pipe and stop the writer.
 */
object AndroidFrameStats {
    fun dump(packageName: String): String? = dumpService("gfxinfo", arrayOf(packageName, "framestats"))

    /** SurfaceFlinger's layers, one name a line, as `dumpsys SurfaceFlinger --list` prints them. */
    fun surfaceFlingerLayers(): String? = dumpService("SurfaceFlinger", arrayOf("--list"))

    /** SurfaceFlinger's frame timings of [layer], as `dumpsys SurfaceFlinger --latency <layer>`. */
    fun surfaceFlingerLatency(layer: String): String? =
        dumpService("SurfaceFlinger", arrayOf("--latency", layer))

    /** What the service [name] writes when dumped with [args]; null when there is no such service. */
    private fun dumpService(name: String, args: Array<String>): String? {
        val service = Class.forName("android.os.ServiceManager")
            .getMethod("getService", String::class.java)
            .invoke(null, name) as IBinder? ?: return null
        val (read, write) = ParcelFileDescriptor.createPipe()
        val out = ByteArrayOutputStream()
        val reader = Thread({
            ParcelFileDescriptor.AutoCloseInputStream(read).use { it.copyTo(out) }
        }, "dsa-framestats")
        reader.start()
        try {
            write.use { service.dump(it.fileDescriptor, args) }
        } finally {
            reader.join(READ_TIMEOUT_MILLIS)
        }
        return out.toString(Charsets.UTF_8.name())
    }

    private const val READ_TIMEOUT_MILLIS = 5_000L
}

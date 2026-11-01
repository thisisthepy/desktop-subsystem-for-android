package io.github.thisisthepy.dsa.agent.platform

import android.os.IBinder
import android.os.ParcelFileDescriptor
import java.io.ByteArrayOutputStream

/**
 * Reads an app's frame timings, as `dumpsys gfxinfo <package> framestats` does: the activity
 * manager's graphics service asks each of the package's processes to write what its renderer
 * recorded for its last frames. It needs the DUMP permission.
 *
 * The output comes back through a pipe, read on a thread of its own so that a long dump cannot
 * fill the pipe and stop the writer.
 */
object AndroidFrameStats {
    fun dump(packageName: String): String? {
        val service = Class.forName("android.os.ServiceManager")
            .getMethod("getService", String::class.java)
            .invoke(null, "gfxinfo") as IBinder? ?: return null
        val (read, write) = ParcelFileDescriptor.createPipe()
        val out = ByteArrayOutputStream()
        val reader = Thread({
            ParcelFileDescriptor.AutoCloseInputStream(read).use { it.copyTo(out) }
        }, "dsa-framestats")
        reader.start()
        try {
            write.use { service.dump(it.fileDescriptor, arrayOf(packageName, "framestats")) }
        } finally {
            reader.join(READ_TIMEOUT_MILLIS)
        }
        return out.toString(Charsets.UTF_8.name())
    }

    private const val READ_TIMEOUT_MILLIS = 5_000L
}

package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.os.DropBoxManager
import android.os.IBinder
import android.os.ParcelFileDescriptor
import io.github.thisisthepy.dsa.agent.diagnostics.DiagnosticsSource
import io.github.thisisthepy.dsa.agent.diagnostics.DropboxEntry
import java.io.ByteArrayOutputStream

/**
 * The agent's part of the diagnostics, from the system: the dropbox, which the agent reads with
 * READ_LOGS and PACKAGE_USAGE_STATS, and the system services' dumps, which it asks for with DUMP
 * as `dumpsys` does. Each dump comes back through a pipe, read on a thread of its own so that a
 * long dump cannot fill the pipe and stop the writer.
 */
class AndroidDiagnostics(context: Context) : DiagnosticsSource {
    private val dropbox = context.getSystemService(DropBoxManager::class.java)

    override fun dropbox(): List<DropboxEntry> {
        val box = dropbox ?: return emptyList()
        val entries = mutableListOf<DropboxEntry>()
        var after = 0L
        while (true) {
            val entry = box.getNextEntry(null, after) ?: break
            entry.use {
                after = it.timeMillis
                val data = it.inputStream?.use { stream -> stream.readBytes() }
                    ?: it.getText(MAX_TEXT)?.toByteArray(Charsets.UTF_8)
                    ?: ByteArray(0)
                entries += DropboxEntry(it.tag ?: "", it.timeMillis, data)
            }
        }
        return entries
    }

    override fun dump(service: String, args: List<String>): ByteArray? {
        val binder = Class.forName("android.os.ServiceManager")
            .getMethod("getService", String::class.java)
            .invoke(null, service) as IBinder? ?: return null
        val (read, write) = ParcelFileDescriptor.createPipe()
        val out = ByteArrayOutputStream()
        val reader = Thread({
            ParcelFileDescriptor.AutoCloseInputStream(read).use { it.copyTo(out) }
        }, "dsa-diagnostics-dump")
        reader.start()
        try {
            write.use { binder.dump(it.fileDescriptor, args.toTypedArray()) }
        } finally {
            reader.join(READ_TIMEOUT_MILLIS)
        }
        return out.toByteArray()
    }

    private companion object {
        const val READ_TIMEOUT_MILLIS = 30_000L
        const val MAX_TEXT = 16 * 1024 * 1024
    }
}

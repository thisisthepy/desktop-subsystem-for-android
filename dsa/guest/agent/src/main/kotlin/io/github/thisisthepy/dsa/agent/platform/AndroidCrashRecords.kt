package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.DropBoxManager
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.diagnostics.CrashRecordSource
import io.github.thisisthepy.dsa.agent.diagnostics.NativeLibraryCrashes

/**
 * The crash records the system writes to its dropbox, as they come.
 *
 * The dropbox announces each new entry (ACTION_DROPBOX_ENTRY_ADDED) to receivers that hold
 * READ_LOGS and PACKAGE_USAGE_STATS, as the agent does. The announcements of a burst are
 * coalesced, so each one reads every entry of the crash tags written since the last read.
 */
class AndroidCrashRecords(private val context: Context, private val log: Log) : CrashRecordSource {
    private val dropbox = context.getSystemService(DropBoxManager::class.java)

    /** The time of the newest entry read, per tag; entries before the agent started are old news. */
    private val after = HashMap<String, Long>()

    override fun listen(onRecord: (tag: String, text: String) -> Unit) {
        val box = dropbox ?: return log.warn("the system has no dropbox; crashes of apps are not reported")
        val started = System.currentTimeMillis()
        for (tag in NativeLibraryCrashes.TAGS) after[tag] = started
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                for (tag in NativeLibraryCrashes.TAGS) read(box, tag, onRecord)
            }
        }
        // Only the system sends it.
        context.registerReceiver(receiver, IntentFilter(DropBoxManager.ACTION_DROPBOX_ENTRY_ADDED), Context.RECEIVER_NOT_EXPORTED)
    }

    @Synchronized
    private fun read(box: DropBoxManager, tag: String, onRecord: (tag: String, text: String) -> Unit) {
        var time = after[tag] ?: return
        try {
            while (true) {
                val entry = box.getNextEntry(tag, time) ?: break
                entry.use {
                    time = it.timeMillis
                    onRecord(tag, it.getText(MAX_TEXT).orEmpty())
                }
            }
        } catch (e: Exception) {
            log.warn("could not read the dropbox's $tag records", e)
        }
        after[tag] = time
    }

    private companion object {
        /** A crash record's stack trace is a few KiB; the header and the cause come first. */
        const val MAX_TEXT = 64 * 1024
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.content.Context
import android.os.ParcelFileDescriptor
import android.os.ServiceManager
import io.github.thisisthepy.dsa.agent.memory.AppProcess
import io.github.thisisthepy.dsa.agent.memory.BackgroundProcesses
import io.github.thisisthepy.dsa.agent.memory.ProcessPss
import io.github.thisisthepy.dsa.agent.memory.ProcessRecord
import io.github.thisisthepy.dsa.agent.memory.ProcessRecords
import java.lang.reflect.Field
import java.util.concurrent.atomic.AtomicReference

/**
 * The app processes through the activity manager, for ending the ones that stay cached.
 *
 * The list of processes is `getRunningAppProcesses`, which answers for every process because
 * the agent holds REAL_GET_TASKS. Each entry's process state and flags (persistent, holds
 * activities) are fields the SDK hides; the agent is a platform app and reads them by
 * reflection. Without them the state is taken from the entry's importance and every cached
 * process counts as holding activities, so nothing is ended on a guess.
 *
 * The activity manager's records (which packages a process loaded code from, the score it
 * applied) are what `dumpsys activity processes` prints, read the same way through the
 * service's dump, which DUMP allows.
 *
 * Ending is `ActivityManager.killBackgroundProcesses`: it ends the package's processes at the
 * service level or below and leaves the package as it was. Unlike a force stop it cancels no
 * job, alarm or notification and does not mark the package stopped, so scheduled work and push
 * messages start the app again when they are due. For another app's package it takes
 * KILL_ALL_BACKGROUND_PROCESSES, which the agent holds as a privileged app.
 */
class AndroidBackgroundProcesses(context: Context) : BackgroundProcesses {
    private val activities = context.getSystemService(ActivityManager::class.java)

    override fun running(): List<AppProcess> =
        activities.runningAppProcesses.orEmpty().map { info ->
            val flags = intField(FLAGS, info)
            val state = intField(PROCESS_STATE, info) ?: stateOf(info.importance)
            AppProcess(
                pid = info.pid,
                uid = info.uid,
                name = info.processName,
                packages = info.pkgList.orEmpty().toList(),
                state = state,
                persistent = flags != null && (flags and FLAG_PERSISTENT) != 0,
                hasActivities = if (flags != null) (flags and FLAG_HAS_ACTIVITIES) != 0 else state >= STATE_CACHED_ACTIVITY,
            )
        }

    override fun records(): List<ProcessRecord>? = dump("activity", "processes")?.let(ProcessRecords::parse)

    override fun memory(pids: List<Int>): List<ProcessPss> {
        if (pids.isEmpty()) return emptyList()
        val info = activities.getProcessMemoryInfo(pids.toIntArray())
        return pids.indices.mapNotNull { i ->
            val memory = info.getOrNull(i) ?: return@mapNotNull null
            ProcessPss(pid = pids[i], name = "", pssKb = memory.totalPss.toLong(), swapPssKb = SwapPss.of(memory))
        }
    }

    @Suppress("DEPRECATION")
    override fun foregroundServicePackages(): Set<String> =
        activities.getRunningServices(Int.MAX_VALUE).orEmpty()
            .filter { it.foreground }
            .map { it.service.packageName }
            .toSet()

    override fun endBackground(packageName: String) {
        activities.killBackgroundProcesses(packageName)
    }

    /**
     * What the [service]'s dump prints for [args], as `dumpsys` shows it; null when the service
     * is not up or the dump does not finish within [DUMP_WAIT_MILLIS]. The dump is asked for
     * one way and read on a thread of its own, so a dump larger than the pipe holds never waits
     * on this thread, and a service that does not answer holds nothing up.
     */
    private fun dump(service: String, vararg args: String): String? {
        val binder = ServiceManager.getService(service) ?: return null
        val (read, write) = ParcelFileDescriptor.createPipe()
        val text = AtomicReference<String?>(null)
        val reader = Thread({
            try {
                ParcelFileDescriptor.AutoCloseInputStream(read).use { text.set(it.readBytes().decodeToString()) }
            } catch (e: java.io.IOException) {
                // Closed after the wait below ran out: the dump counts as unread.
            }
        }, "dsa-process-records")
        reader.isDaemon = true
        reader.start()
        try {
            binder.dumpAsync(write.fileDescriptor, arrayOf(*args))
        } finally {
            // The service has its own copy of the descriptor; the text ends when it closes it.
            write.close()
        }
        reader.join(DUMP_WAIT_MILLIS)
        if (reader.isAlive) {
            runCatching { read.close() }
            return null
        }
        return text.get()
    }

    private companion object {
        const val DUMP_WAIT_MILLIS = 3000L

        // RunningAppProcessInfo.FLAG_PERSISTENT and FLAG_HAS_ACTIVITIES, hidden from the SDK.
        const val FLAG_PERSISTENT = 1 shl 1
        const val FLAG_HAS_ACTIVITIES = 1 shl 2

        // ActivityManager.PROCESS_STATE_SERVICE, PROCESS_STATE_CACHED_ACTIVITY and
        // PROCESS_STATE_TOP, hidden from the SDK.
        const val STATE_SERVICE = 10
        const val STATE_CACHED_ACTIVITY = 16
        const val STATE_TOP = 2

        val FLAGS: Field? = field("flags")
        val PROCESS_STATE: Field? = field("processState")

        fun field(name: String): Field? = runCatching {
            ActivityManager.RunningAppProcessInfo::class.java.getField(name)
        }.getOrNull()

        fun intField(field: Field?, info: ActivityManager.RunningAppProcessInfo): Int? =
            field?.let { runCatching { it.getInt(info) }.getOrNull() }

        /** The process state an importance stands for, at its most important. */
        fun stateOf(importance: Int): Int = when {
            importance >= ActivityManager.RunningAppProcessInfo.IMPORTANCE_CACHED -> STATE_CACHED_ACTIVITY
            importance >= ActivityManager.RunningAppProcessInfo.IMPORTANCE_SERVICE -> STATE_SERVICE
            else -> STATE_TOP
        }
    }
}

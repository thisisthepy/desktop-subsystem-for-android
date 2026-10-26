package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.content.Context
import io.github.thisisthepy.dsa.agent.memory.MemorySource
import io.github.thisisthepy.dsa.agent.memory.ProcessPss
import java.io.File

/**
 * The guest's memory as the kernel and the activity manager see it.
 *
 * The activity manager answers for every process it runs because the agent holds
 * REAL_GET_TASKS; without it, it would answer for the agent's own process alone. Native
 * daemons are not among them, and the kernel's totals cover them.
 */
class AndroidMemorySource(context: Context) : MemorySource {
    private val activities = context.getSystemService(ActivityManager::class.java)

    override fun meminfo(): String = File("/proc/meminfo").readText()

    override fun zramMmStat(): String? {
        val file = File("/sys/devices/virtual/block/zram0/mm_stat")
        return try {
            file.readText()
        } catch (e: Exception) {
            // No zram, or the policy keeps the agent from reading it: the report goes without.
            null
        }
    }

    override fun processes(): List<ProcessPss> {
        val running = activities.runningAppProcesses ?: return emptyList()
        if (running.isEmpty()) return emptyList()
        val info = activities.getProcessMemoryInfo(running.map { it.pid }.toIntArray())
        return running.indices.mapNotNull { i ->
            val memory = info.getOrNull(i) ?: return@mapNotNull null
            ProcessPss(
                pid = running[i].pid,
                name = running[i].processName,
                pssKb = memory.totalPss.toLong(),
                swapPssKb = memory.totalSwappedOutPss.toLong(),
            )
        }
    }
}

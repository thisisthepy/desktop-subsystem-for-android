package io.github.thisisthepy.dsa.agent.platform

import android.content.pm.ApplicationInfo
import io.github.thisisthepy.dsa.agent.install.NativeLibraries
import java.io.File
import java.util.zip.ZipFile

/**
 * The APKs an installed app has on the device, and what they say about its native code.
 *
 * The base APK and the splits are world-readable files under `/data/app`, and the agent sees
 * every package (QUERY_ALL_PACKAGES). Only their central directories are read, and what was
 * read of a file is kept until the file changes, so listing the apps again reads nothing new.
 */
class AndroidApks {
    private class Read(val length: Long, val modified: Long, val entries: List<String>)

    private val read = HashMap<String, Read>()

    /** The app's base APK and its splits, in that order. */
    fun files(info: ApplicationInfo): List<File> =
        (listOfNotNull(info.sourceDir) + (info.splitSourceDirs?.toList() ?: emptyList())).map(::File)

    /** The native libraries of the app's APKs together; empty when there are none or they cannot be read. */
    @Synchronized
    fun libraries(info: ApplicationInfo): Map<String, List<String>> {
        // An updated app's APKs move to a new folder; what was read of the old ones goes now and then.
        if (read.size > MAX_KEPT) read.clear()
        return NativeLibraries.of(files(info).map { entries(it) })
    }

    private fun entries(file: File): List<String> {
        val length = file.length()
        val modified = file.lastModified()
        read[file.path]?.let { if (it.length == length && it.modified == modified) return it.entries }
        val names = try {
            ZipFile(file).use { zip -> zip.entries().asSequence().map { it.name }.filter { it.startsWith("lib/") }.toList() }
        } catch (e: Exception) {
            emptyList()
        }
        read[file.path] = Read(length, modified, names)
        return names
    }

    companion object {
        /** The most APKs whose entries are kept. */
        private const val MAX_KEPT = 1024

        /**
         * The ABI the app's native code runs as: ApplicationInfo.primaryCpuAbi, a field the SDK
         * hides, which the platform-signed agent may read. Empty for an app without native code
         * or when the field cannot be read.
         */
        fun primaryAbi(info: ApplicationInfo): String = try {
            ApplicationInfo::class.java.getField("primaryCpuAbi").get(info) as String? ?: ""
        } catch (e: Exception) {
            ""
        }
    }
}

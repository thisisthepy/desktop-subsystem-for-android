package io.github.thisisthepy.dsa.agent.platform

import io.github.thisisthepy.dsa.agent.RecordingLog
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The expansion files of an installed app land in its folder under Android/obb/, whole. */
class AndroidExpansionFilesTest {
    @Test
    fun fr11_an_expansion_file_is_copied_into_the_apps_folder_under_android_obb() {
        val root = Files.createTempDirectory("dsa-obb").toFile()
        try {
            val staging = File(root, "cache/expansion-files")
            val storage = File(root, "storage")
            val files = AndroidExpansionFiles(staging, storage, RecordingLog())
            val content = ByteArray(70_000) { (it % 251).toByte() }
            files.stage("main.42.com.example.notes.obb").use { it.write(content) }
            files.stage("patch.42.com.example.notes.obb").use { it.write(byteArrayOf(1, 2, 3)) }

            files.place("com.example.notes", listOf("main.42.com.example.notes.obb"))
            val placed = File(storage, "Android/obb/com.example.notes/main.42.com.example.notes.obb")
            assertArrayEquals(content, placed.readBytes())
            assertFalse("no half file is left", File(placed.parentFile, "main.42.com.example.notes.obb.part").exists())
            assertFalse("the cache lets the file go", File(staging, "main.42.com.example.notes.obb").exists())

            files.discard(listOf("patch.42.com.example.notes.obb"))
            assertFalse(File(staging, "patch.42.com.example.notes.obb").exists())
            assertTrue(placed.exists())
        } finally {
            root.deleteRecursively()
        }
    }
}

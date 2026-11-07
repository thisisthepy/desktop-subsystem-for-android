package io.github.thisisthepy.dsa.agent.install

import io.github.thisisthepy.dsa.agent.tasks.LauncherApp
import io.github.thisisthepy.dsa.agent.tasks.Messages
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

class NativeLibrariesTest {
    @Test
    fun fr25_the_libraries_of_an_apk_and_its_splits_are_merged_by_abi() {
        val base = listOf("AndroidManifest.xml", "classes.dex", "res/raw/lib.so")
        val x86 = listOf("lib/x86_64/libmain.so")
        val arm = listOf("lib/arm64-v8a/libmain.so", "lib/arm64-v8a/libunity.so", "lib/arm64-v8a/libmain.so")
        assertEquals(
            mapOf("arm64-v8a" to listOf("libmain.so", "libunity.so"), "x86_64" to listOf("libmain.so")),
            NativeLibraries.of(listOf(base, x86, arm)),
        )
        assertEquals(emptyMap<String, List<String>>(), NativeLibraries.of(listOf(base)))
    }

    @Test
    fun fr25_entries_that_are_no_libraries_are_left_out() {
        assertNull(NativeLibraries.library("lib/arm64-v8a/sub/libnested.so"))
        assertNull(NativeLibraries.library("lib/arm64-v8a/readme.txt"))
        assertNull(NativeLibraries.library("lib/ARM64/libupper.so"))
        assertNull(NativeLibraries.library("assets/lib/x86_64/libasset.so"))
        assertEquals("arm64-v8a" to "libok.so", NativeLibraries.library("lib/arm64-v8a/libok.so"))
    }

    @Test
    fun fr25_the_host_hears_each_apps_abi_and_libraries() {
        val app = LauncherApp(
            "com.example.game", "Game", ByteArray(0),
            primaryAbi = "x86_64",
            nativeLibraries = mapOf("arm64-v8a" to listOf("libmain.so"), "x86_64" to listOf("libmain.so")),
        )
        val sent = Envelope.parseFrom(Messages.installedApp(app)).installedApp
        assertEquals("x86_64", sent.primaryAbi)
        assertEquals(listOf("arm64-v8a", "x86_64"), sent.nativeLibrariesList.map { it.abi })
        assertEquals(listOf("libmain.so"), sent.nativeLibrariesList[0].namesList)
        // A change of ABI is a change the host is told of.
        val arm = LauncherApp("com.example.game", "Game", ByteArray(0), primaryAbi = "arm64-v8a", nativeLibraries = app.nativeLibraries)
        assertFalse(app.sameAs(arm))
    }
}

package io.github.thisisthepy.dsa.agent.diagnostics

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Crash records as Android's ActivityManager writes them to the dropbox. */
private fun javaCrash(packageName: String, cause: String) = """
    Process: $packageName
    PID: 4242
    UID: 10123
    Flags: 0x38c83e44
    Package: $packageName v31 (3.1)
    Foreground: Yes
    Build: generic/aosp_cf_x86_64_phone/vsoc_x86_64:16/BP2A/1:user/release-keys

    $cause
    	at java.lang.Runtime.loadLibrary0(Runtime.java:1081)
    	at java.lang.System.loadLibrary(System.java:1765)
    	at com.unity3d.player.UnityPlayer.<clinit>(UnityPlayer.java:42)
""".trimIndent()

private const val MISSING_LIBRARY =
    "java.lang.UnsatisfiedLinkError: dlopen failed: library \"libunity.so\" not found"

private const val NATIVE_CRASH = """Process: com.example.engine
PID: 5151
Package: com.example.engine v7 (0.7)
Abort message: 'dlopen failed: cannot locate symbol "AAsset_read" referenced by "libengine.so"'
"""

class NativeLibraryCrashesTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()

    /** The dropbox, which the test writes records into. */
    private class FakeCrashRecords : CrashRecordSource {
        var onRecord: ((String, String) -> Unit)? = null

        override fun listen(onRecord: (tag: String, text: String) -> Unit) {
            this.onRecord = onRecord
        }

        fun write(tag: String, text: String) = onRecord!!(tag, text)
    }

    private val records = FakeCrashRecords()
    private var now = 1_000_000L

    private fun started(arm64: Boolean = true): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            crashRecords = records,
            arm64Supported = { arm64 },
            clock = { now },
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun write(tag: String, text: String) {
        records.write(tag, text)
        scheduler.runDue()
    }

    private fun reported() = daemon.sentMessages().filter { it.hasNativeLibraryCrash() }.map { it.nativeLibraryCrash }

    @Test
    fun fr25_only_apps_that_ended_for_want_of_a_native_library_are_reported() {
        started()

        write("data_app_crash", javaCrash("com.example.game", MISSING_LIBRARY))
        // Another crash, a crash of the system, an ANR and the agent's own crash are not.
        write("data_app_crash", javaCrash("com.example.notes", "java.lang.NullPointerException: Attempt to invoke a virtual method"))
        write("system_app_crash", javaCrash("com.android.settings", MISSING_LIBRARY))
        write("data_app_anr", javaCrash("com.example.slow", MISSING_LIBRARY))
        write("data_app_crash", javaCrash(AGENT, MISSING_LIBRARY))
        // A native crash that names a library that did not load is.
        write("data_app_native_crash", NATIVE_CRASH)

        val crashes = reported()
        assertEquals(listOf("com.example.game", "com.example.engine"), crashes.map { it.packageName })
        assertEquals(MISSING_LIBRARY, crashes[0].detail)
        assertTrue(crashes[1].detail.contains("dlopen failed"))
        assertTrue(crashes.all { it.arm64Supported })
    }

    @Test
    fun fr25_an_app_that_keeps_crashing_is_reported_once_every_few_minutes() {
        started(arm64 = false)

        write("data_app_crash", javaCrash("com.example.game", MISSING_LIBRARY))
        now += 60_000
        write("data_app_crash", javaCrash("com.example.game", MISSING_LIBRARY))
        assertEquals(1, reported().size)
        assertTrue("the host is told no translator is in place", !reported().single().arm64Supported)

        now += NativeLibraryCrashes.REPEAT_MILLIS
        write("data_app_crash", javaCrash("com.example.game", MISSING_LIBRARY))
        assertEquals(2, reported().size)
    }

    @Test
    fun fr25_a_chromium_build_made_to_run_as_32_bit_code_is_reported() {
        started()

        // Chrome's 32-bit build, which a 64-bit-only Android starts as its WebView-only
        // 64-bit code, ends at start with this.
        val cause = "java.lang.RuntimeException: Unable to start activity ComponentInfo{com.android.chrome/" +
            "org.chromium.chrome.browser.firstrun.FirstRunActivity}: java.lang.RuntimeException: Starting in " +
            "64-bit mode requires the 64-bit native library. If the device is 64-bit only, see alternatives " +
            "here: https://crbug.com/1303857#c7."
        write("data_app_crash", javaCrash("com.android.chrome", cause))

        val crash = reported().single()
        assertEquals("com.android.chrome", crash.packageName)
        assertTrue(crash.detail.contains("64-bit native library"))
    }

    @Test
    fun fr25_a_record_that_names_no_package_is_dropped() {
        assertNull(NativeLibraryCrashes.parse("data_app_crash", MISSING_LIBRARY))
        assertNull(NativeLibraryCrashes.parse("data_app_crash", "Package: not a package\n$MISSING_LIBRARY"))
        assertEquals(
            "com.example.remote",
            NativeLibraryCrashes.parse("data_app_crash", "Process: com.example.remote:service\n$MISSING_LIBRARY")?.packageName,
        )
    }
}

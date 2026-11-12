package io.github.thisisthepy.dsa.agent.diagnostics

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.NativeLibraryCrash

/** The system's crash records as they are written, which the agent reads with READ_LOGS. */
fun interface CrashRecordSource {
    /** Calls [onRecord], from any thread, with the tag and the text of each new record. */
    fun listen(onRecord: (tag: String, text: String) -> Unit)

    companion object {
        val NONE = CrashRecordSource { }
    }
}

/** An app that ended because one of its native libraries did not load. */
data class NativeCrash(val packageName: String, val detail: String)

/**
 * Tells the host when an app ends because it could not load a native library, so the host can
 * offer to install the app again as ARM, or tell the user to install the app's 64-bit build
 * (NativeLibraryCrash).
 *
 * An app whose x86_64 code leaves out a library its ARM code has dies the moment it loads it,
 * with an UnsatisfiedLinkError whose message says "dlopen failed". A Chromium build made to run
 * as 32-bit code, such as Chrome's for devices with both 32-bit and 64-bit code, ends at start
 * on this 64-bit-only Android saying "Starting in 64-bit mode requires the 64-bit native
 * library", since its 64-bit library holds only WebView. Android writes each crash of
 * a user app to its dropbox as `data_app_crash` (Java) or `data_app_native_crash` (native), with
 * a header naming the package; only those records, and of them only the ones that name a library
 * that did not load, are reported. An app that keeps crashing is reported once every
 * [REPEAT_MILLIS], and the agent's own crashes never.
 *
 * Every method runs on the agent's scheduler thread.
 */
class NativeLibraryCrashes(
    private val ownPackage: String,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    /** Whether the guest's ABI list holds arm64-v8a this boot. */
    private val arm64Supported: () -> Boolean,
    private val clock: () -> Long,
) {
    private val reported = HashMap<String, Long>()

    /** Takes one crash record of the system. */
    fun record(tag: String, text: String) {
        val crash = parse(tag, text) ?: return
        if (crash.packageName == ownPackage) return
        val now = clock()
        val last = reported[crash.packageName]
        if (last != null && now - last < REPEAT_MILLIS) return
        log.info("${crash.packageName} ended because a native library did not load: ${crash.detail}")
        val message = NativeLibraryCrash.newBuilder()
            .setPackageName(crash.packageName)
            .setDetail(crash.detail)
            .setArm64Supported(arm64Supported())
        if (send(Envelope.newBuilder().setNativeLibraryCrash(message).build().toByteArray())) {
            reported[crash.packageName] = now
        }
    }

    companion object {
        /** How long an app that crashed the same way is not reported again. */
        const val REPEAT_MILLIS = 5 * 60 * 1000L

        /** The dropbox tags of the crashes of apps the user installed. */
        val TAGS = setOf("data_app_crash", "data_app_native_crash")

        /** The longest detail sent. */
        private const val MAX_DETAIL = 512

        /** The markers of a native library that did not load. */
        private val MARKERS = listOf(
            "java.lang.UnsatisfiedLinkError",
            "dlopen failed",
            "requires the 64-bit native library",
        )

        /**
         * The app a crash record of [tag] says ended because a native library did not load, or
         * null for any other record. The record's header names the package (`Package: <name>
         * v<code> (<version>)`, or `Process: <name>` when it has no package line).
         */
        fun parse(tag: String, text: String): NativeCrash? {
            if (tag !in TAGS) return null
            val lines = text.lineSequence().map { it.trim() }.toList()
            val detail = lines.firstOrNull { line -> MARKERS.any { it in line } } ?: return null
            val packageName = header(lines, "Package:") ?: header(lines, "Process:") ?: return null
            return NativeCrash(packageName, detail.take(MAX_DETAIL))
        }

        private fun header(lines: List<String>, name: String): String? {
            val value = lines.firstOrNull { it.startsWith(name) }?.removePrefix(name)?.trim() ?: return null
            // A process name may carry its own suffix after a colon (com.example:remote).
            val packageName = value.substringBefore(' ').substringBefore(':')
            // An app's package has at least two parts.
            val parts = packageName.split('.')
            val valid = parts.size >= 2 && parts.all { part ->
                part.isNotEmpty() && part.all { it.isLetterOrDigit() || it == '_' }
            }
            return if (valid) packageName else null
        }
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.ResultReceiver
import java.io.FileDescriptor
import java.io.FileInputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * Runs a system service's shell command over binder, as `cmd <service> <args>` would, and
 * returns what it printed, trimmed. Throws with that output when the command fails.
 *
 * A service checks the caller's permission for its commands, not that the caller is the shell,
 * so this works on a user build for the commands the agent's permissions cover. The guest's
 * SELinux policy lets system_server use the descriptors such a call hands it. ServiceManager
 * and IBinder.shellCommand are not in the SDK, so they are called by reflection.
 */
internal fun serviceShell(service: String, vararg args: String): String {
    val binder = Class.forName("android.os.ServiceManager")
        .getMethod("getService", String::class.java)
        .invoke(null, service) as IBinder?
        ?: error("the $service service is not up")
    val shellCallback = Class.forName("android.os.ShellCallback")
    val command = IBinder::class.java.getMethod(
        "shellCommand",
        FileDescriptor::class.java,
        FileDescriptor::class.java,
        FileDescriptor::class.java,
        Array<String>::class.java,
        shellCallback,
        ResultReceiver::class.java,
    )
    val input = ParcelFileDescriptor.open(java.io.File("/dev/null"), ParcelFileDescriptor.MODE_READ_ONLY)
    val (read, write) = ParcelFileDescriptor.createPipe()
    // The service reports the command's exit code through a one-way call, which may
    // arrive just after the command's call returns.
    val done = CountDownLatch(1)
    val code = AtomicInteger(0)
    val result = object : ResultReceiver(null) {
        override fun onReceiveResult(resultCode: Int, resultData: android.os.Bundle?) {
            code.set(resultCode)
            done.countDown()
        }
    }
    try {
        command.invoke(
            binder,
            input.fileDescriptor,
            write.fileDescriptor,
            write.fileDescriptor,
            arrayOf(*args),
            null,
            result,
        )
    } finally {
        input.close()
        write.close()
    }
    val output = FileInputStream(read.fileDescriptor).use { it.readBytes().decodeToString().trim() }
    read.close()
    done.await(SHELL_RESULT_WAIT_MILLIS, TimeUnit.MILLISECONDS)
    // The commands print "Unknown option" and the like, and some exit 0 all the same.
    if (code.get() != 0 || output.startsWith("Error") || output.contains("Unknown") || output.contains("Unable")) {
        error("$service ${args.joinToString(" ")} failed (code ${code.get()}): $output")
    }
    return output
}

private const val SHELL_RESULT_WAIT_MILLIS = 2000L

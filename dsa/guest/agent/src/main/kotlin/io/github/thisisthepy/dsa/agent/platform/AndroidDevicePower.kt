package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import android.os.ResultReceiver
import android.os.SystemClock
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.power.Battery
import io.github.thisisthepy.dsa.agent.power.DevicePower
import java.io.FileDescriptor
import java.io.FileInputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * The framework's power controls, all behind the DEVICE_POWER permission the platform key
 * gives the agent.
 *
 * The screen goes off and on through PowerManager.goToSleep and wakeUp, and battery saver
 * through setPowerSaveModeEnabled; none is in the SDK, so they are called by reflection.
 * The device idle controller has no binder method that forces deep idle, and the battery
 * service none that sets the battery, so the agent runs their shell commands
 * (`deviceidle force-idle`, `battery set ...`, `activity kill-all`) over binder, as `dumpsys` and `cmd` do. Both
 * work on a user build: they check the caller's permission, not that it is the shell. The
 * guest's SELinux policy lets system_server use the descriptors such a call hands it.
 */
class AndroidDevicePower(private val context: Context, private val log: Log) : DevicePower {
    private val powerManager = context.getSystemService(PowerManager::class.java)

    override fun screenOff() {
        PowerManager::class.java.getMethod("goToSleep", Long::class.javaPrimitiveType)
            .invoke(powerManager, SystemClock.uptimeMillis())
    }

    override fun screenOn() {
        val now = SystemClock.uptimeMillis()
        try {
            PowerManager::class.java
                .getMethod("wakeUp", Long::class.javaPrimitiveType, Int::class.javaPrimitiveType, String::class.java)
                .invoke(powerManager, now, WAKE_REASON_APPLICATION, WAKE_DETAILS)
        } catch (e: NoSuchMethodException) {
            PowerManager::class.java.getMethod("wakeUp", Long::class.javaPrimitiveType).invoke(powerManager, now)
        }
    }

    override fun forceIdle() {
        // Deep idle may be off on a device with no battery; `force-idle` then only prints
        // that it cannot and exits 0, so it is turned on first and the result is checked.
        shell("deviceidle", "enable", "deep")
        shell("deviceidle", "force-idle", "deep")
        val deadline = SystemClock.uptimeMillis() + IDLE_WAIT_MILLIS
        while (!isDeviceIdle()) {
            if (SystemClock.uptimeMillis() >= deadline) {
                error("deviceidle force-idle deep was accepted but the device is not idle after $IDLE_WAIT_MILLIS ms")
            }
            Thread.sleep(100)
        }
        log.info("device is in deep idle")
    }

    override fun unforceIdle() {
        shell("deviceidle", "unforce")
    }

    override fun killCachedApps() {
        shell("activity", "kill-all")
    }

    override fun isDeviceIdle(): Boolean = powerManager.isDeviceIdleMode

    override fun listenIdle(onChange: () -> Unit) {
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) = onChange()
        }
        context.registerReceiver(
            receiver,
            IntentFilter(PowerManager.ACTION_DEVICE_IDLE_MODE_CHANGED),
            Context.RECEIVER_NOT_EXPORTED,
        )
    }

    override fun setBattery(battery: Battery) {
        val status = when {
            !battery.present -> BatteryManager.BATTERY_STATUS_UNKNOWN
            battery.charging -> BatteryManager.BATTERY_STATUS_CHARGING
            battery.acOnline && battery.percent >= 100 -> BatteryManager.BATTERY_STATUS_FULL
            battery.acOnline -> BatteryManager.BATTERY_STATUS_NOT_CHARGING
            else -> BatteryManager.BATTERY_STATUS_DISCHARGING
        }
        // Each `set` stops the battery service from following the device's own measurements,
        // which a VM does not have, until a `battery reset`.
        shell("battery", "set", "present", if (battery.present) "1" else "0")
        shell("battery", "set", "level", battery.percent.toString())
        shell("battery", "set", "status", status.toString())
        shell("battery", "set", "usb", "0")
        shell("battery", "set", "wireless", "0")
        shell("battery", "set", "ac", if (battery.acOnline) "1" else "0")
    }

    override fun setPowerSaver(on: Boolean) {
        PowerManager::class.java.getMethod("setPowerSaveModeEnabled", Boolean::class.javaPrimitiveType)
            .invoke(powerManager, on)
    }

    /**
     * Runs a system service's shell command, as `cmd <service> <args>` would, and throws with
     * what it printed when it fails.
     */
    private fun shell(service: String, vararg args: String) {
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
        done.await(RESULT_WAIT_MILLIS, TimeUnit.MILLISECONDS)
        // The commands print "Unknown option" and the like, and some exit 0 all the same.
        if (code.get() != 0 || output.startsWith("Error") || output.contains("Unknown") || output.contains("Unable")) {
            error("$service ${args.joinToString(" ")} failed (code ${code.get()}): $output")
        }
        if (output.isNotEmpty()) log.info("$service ${args.joinToString(" ")}: $output")
    }

    private companion object {
        // PowerManager.WAKE_REASON_APPLICATION
        const val WAKE_REASON_APPLICATION = 2
        const val WAKE_DETAILS = "io.github.thisisthepy.dsa.agent:window shown"
        const val RESULT_WAIT_MILLIS = 2000L
        const val IDLE_WAIT_MILLIS = 3000L
    }
}

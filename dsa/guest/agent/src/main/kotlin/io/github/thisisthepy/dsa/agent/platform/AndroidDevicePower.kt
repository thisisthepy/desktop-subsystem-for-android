package io.github.thisisthepy.dsa.agent.platform

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.PowerManager
import android.os.SystemClock
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.power.Battery
import io.github.thisisthepy.dsa.agent.power.BatteryStatus
import io.github.thisisthepy.dsa.agent.power.DevicePower

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
        val status = when (battery.status) {
            BatteryStatus.UNKNOWN -> BatteryManager.BATTERY_STATUS_UNKNOWN
            BatteryStatus.CHARGING -> BatteryManager.BATTERY_STATUS_CHARGING
            BatteryStatus.DISCHARGING -> BatteryManager.BATTERY_STATUS_DISCHARGING
            BatteryStatus.NOT_CHARGING -> BatteryManager.BATTERY_STATUS_NOT_CHARGING
            BatteryStatus.FULL -> BatteryManager.BATTERY_STATUS_FULL
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
     * Sets the lowest and highest refresh rate the framework may choose (the settings the
     * "smooth display" switch of a phone writes) both to [hz]. The display manager follows
     * them at once and has SurfaceFlinger switch the display to the hwcomposer's configuration
     * at that rate. They are hidden system settings, which WRITE_SECURE_SETTINGS lets the
     * agent write.
     */
    override fun setRefreshRate(hz: Int) {
        val resolver = context.contentResolver
        val rate = hz.toFloat()
        // Each step keeps the lowest at or below the highest: going down the lowest moves
        // first, going up the highest.
        if (rate < currentRate(resolver, MIN_REFRESH_RATE)) {
            Settings.System.putFloat(resolver, MIN_REFRESH_RATE, rate)
            Settings.System.putFloat(resolver, PEAK_REFRESH_RATE, rate)
        } else {
            Settings.System.putFloat(resolver, PEAK_REFRESH_RATE, rate)
            Settings.System.putFloat(resolver, MIN_REFRESH_RATE, rate)
        }
    }

    private fun currentRate(resolver: android.content.ContentResolver, name: String): Float =
        Settings.System.getFloat(resolver, name, 0f)

    /**
     * Sets the guest's wall clock through the alarm manager (SET_TIME), which also moves the
     * kernel's clock and tells apps the time changed, so their alarms are right again.
     */
    override fun setTime(unixTimeMillis: Long) {
        context.getSystemService(android.app.AlarmManager::class.java).setTime(unixTimeMillis)
    }

    /**
     * Runs a system service's shell command, as `cmd <service> <args>` would, and throws with
     * what it printed when it fails.
     */
    private fun shell(service: String, vararg args: String) {
        val output = serviceShell(service, *args)
        if (output.isNotEmpty()) log.info("$service ${args.joinToString(" ")}: $output")
    }

    private companion object {
        // PowerManager.WAKE_REASON_APPLICATION
        const val WAKE_REASON_APPLICATION = 2
        const val WAKE_DETAILS = "io.github.thisisthepy.dsa.agent:window shown"
        const val IDLE_WAIT_MILLIS = 3000L
        // Settings.System.MIN_REFRESH_RATE and PEAK_REFRESH_RATE, hidden from the SDK.
        const val MIN_REFRESH_RATE = "min_refresh_rate"
        const val PEAK_REFRESH_RATE = "peak_refresh_rate"
    }
}

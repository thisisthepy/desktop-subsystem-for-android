package io.github.thisisthepy.dsa.agent.power

/** The guest's battery as the agent sets it: the host computer's, as the host reported it. */
data class Battery(
    /** Whether the device is on AC power. */
    val acOnline: Boolean,
    /** Whether the device has a battery. */
    val present: Boolean,
    /** The battery's charge, from 0 to 100. */
    val percent: Int,
    /** Whether the battery is charging. */
    val charging: Boolean,
)

/** The framework's power controls the agent uses: the screen, Doze and the battery. */
interface DevicePower {
    /** Turns the display off, as the power button does. */
    fun screenOff()

    /** Turns the display on and wakes the device. */
    fun screenOn()

    /** Puts the device into Doze's deep idle state now, whatever its battery says. */
    fun forceIdle()

    /** Lets the device leave the idle state it was forced into. */
    fun unforceIdle()

    /** Ends the cached processes of apps, which start again when something needs them. */
    fun killCachedApps()

    /** Whether the device is in Doze's deep idle state now. */
    fun isDeviceIdle(): Boolean

    /** Calls [onChange], on any thread, whenever the device enters or leaves deep idle. */
    fun listenIdle(onChange: () -> Unit)

    /** Makes the battery service report [battery] instead of what the device measures. */
    fun setBattery(battery: Battery)

    /** Turns battery saver on or off. */
    fun setPowerSaver(on: Boolean)

    companion object {
        /** For an agent without power controls, as in tests that do not look at power. */
        val NONE = object : DevicePower {
            override fun screenOff() = Unit
            override fun screenOn() = Unit
            override fun forceIdle() = Unit
            override fun unforceIdle() = Unit
            override fun killCachedApps() = Unit
            override fun isDeviceIdle() = false
            override fun listenIdle(onChange: () -> Unit) = Unit
            override fun setBattery(battery: Battery) = Unit
            override fun setPowerSaver(on: Boolean) = Unit
        }
    }
}

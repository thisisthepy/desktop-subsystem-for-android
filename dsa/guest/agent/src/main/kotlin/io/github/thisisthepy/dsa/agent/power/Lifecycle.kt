package io.github.thisisthepy.dsa.agent.power

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.DeviceIdle
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PowerState

/**
 * The guest's side of the lifecycle. The guest has no screen and no battery of its own, so
 * Doze never starts by itself: the host says when no task window is shown, and the agent
 * turns the display off and forces the device into deep idle. When the host says a window is
 * shown again, the agent lets the device out of idle and turns the display on. The host's
 * battery and battery saver become the guest's.
 *
 * The agent tells the host whether the device is in deep idle whenever it changes, so the
 * host knows when the guest is dozing. The host takes it not to be when the agent connects.
 *
 * Every method runs on the agent's one thread.
 */
class Lifecycle(
    private val power: DevicePower,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
) {
    private var screenOn = true
    private var reported = false

    /** Starts following the device's idle state; [post] runs work on the agent's thread. */
    fun start(post: (() -> Unit) -> Unit) {
        try {
            power.listenIdle { post(::report) }
        } catch (e: Exception) {
            log.warn("cannot follow the device's idle state; the host hears of it only when the screen changes", e)
        }
    }

    /**
     * The host takes the screen to be on and the device not to be idle when the agent
     * connects, so a screen left off by the last connection comes on, and an idle device is
     * reported.
     */
    fun connected() {
        reported = false
        if (!screenOn) screen(true) else report()
    }

    /** Acts on the frame if it is the host's word on the screen or its power. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.SCREEN_STATE -> screen(envelope.screenState.on)
            Envelope.BodyCase.POWER_STATE -> follow(envelope.powerState)
            else -> return false
        }
        return true
    }

    private fun screen(on: Boolean) {
        if (on) {
            attempt("letting the device out of Doze") { power.unforceIdle() }
            attempt("turning the screen on") { power.screenOn() }
            log.info("screen on: a task window is shown")
        } else {
            attempt("turning the screen off") { power.screenOff() }
            attempt("putting the device into Doze") {
                power.forceIdle()
                log.info("putting the device into Doze: done")
            }
            log.info("screen off: no task window is shown")
        }
        screenOn = on
        report()
    }

    private fun follow(state: PowerState) {
        val battery = if (state.batteryPresent) {
            Battery(
                acOnline = state.acOnline,
                present = true,
                percent = state.batteryPercent.coerceIn(0, 100),
                charging = state.charging,
            )
        } else {
            // A host without a battery: a full battery on AC power.
            Battery(acOnline = true, present = false, percent = 100, charging = false)
        }
        attempt("setting the battery state") { power.setBattery(battery) }
        attempt("setting battery saver") { power.setPowerSaver(state.powerSaver) }
    }

    /** Tells the host whether the device is in deep idle, if that changed since it was told. */
    private fun report() {
        val idle = try {
            power.isDeviceIdle()
        } catch (e: Exception) {
            log.warn("reading the device's idle state failed", e)
            return
        }
        if (idle == reported) return
        val frame = Envelope.newBuilder().setDeviceIdle(DeviceIdle.newBuilder().setIdle(idle)).build().toByteArray()
        if (send(frame)) reported = idle
    }

    private fun attempt(what: String, action: () -> Unit) {
        try {
            action()
        } catch (e: Exception) {
            log.warn("$what failed", e)
        }
    }
}

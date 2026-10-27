package io.github.thisisthepy.dsa.agent.power

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
import io.github.thisisthepy.dsa.control.v1.PowerState
import io.github.thisisthepy.dsa.control.v1.ScreenState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The framework's power controls: records what the agent asked of them. */
class FakePower : DevicePower {
    val calls = mutableListOf<String>()
    var idle = false
    val batteries = mutableListOf<Battery>()
    var saver: Boolean? = null
    var failing = false
    private var onIdle: (() -> Unit)? = null

    override fun screenOff() {
        calls += "screenOff"
    }

    override fun screenOn() {
        calls += "screenOn"
    }

    override fun forceIdle() {
        calls += "forceIdle"
        if (failing) throw IllegalStateException("deviceidle force-idle failed")
        // The device idle controller enters deep idle at once and says so.
        idle = true
        onIdle?.invoke()
    }

    override fun unforceIdle() {
        calls += "unforceIdle"
        idle = false
        onIdle?.invoke()
    }

    override fun isDeviceIdle() = idle

    override fun listenIdle(onChange: () -> Unit) {
        onIdle = onChange
    }

    override fun setBattery(battery: Battery) {
        batteries += battery
    }

    override fun setPowerSaver(on: Boolean) {
        saver = on
    }

    /** The device leaves deep idle for a maintenance window, or enters it again. */
    fun idleChanged(idle: Boolean) {
        this.idle = idle
        onIdle?.invoke()
    }
}

class LifecycleTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val power = FakePower()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            power = power,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun screen(on: Boolean) {
        daemon.deliver(Envelope.newBuilder().setScreenState(ScreenState.newBuilder().setOn(on)).build().toByteArray())
        scheduler.runDue()
    }

    private fun idleReports(): List<Boolean> =
        daemon.sentMessages().filter { it.hasDeviceIdle() }.map { it.deviceIdle.idle }

    @Test
    fun fr10_screen_off_from_the_host_turns_the_display_off_and_forces_doze() {
        started()
        // Nothing is said while the device is awake, as the host takes it to be.
        assertEquals(emptyList<Boolean>(), idleReports())

        screen(false)

        assertEquals(listOf("screenOff", "forceIdle"), power.calls)
        assertEquals(listOf(true), idleReports())
    }

    @Test
    fun fr10_screen_on_from_the_host_lets_the_device_out_of_doze_and_wakes_it() {
        started()
        screen(false)
        power.calls.clear()

        screen(true)

        assertEquals(listOf("unforceIdle", "screenOn"), power.calls)
        assertEquals(listOf(true, false), idleReports())
    }

    @Test
    fun fr10_maintenance_windows_in_doze_are_reported_as_they_happen() {
        started()
        screen(false)
        power.idleChanged(false)
        scheduler.runDue()
        power.idleChanged(true)
        scheduler.runDue()
        assertEquals(listOf(true, false, true), idleReports())
    }

    @Test
    fun fr10_a_reconnecting_agent_turns_the_screen_it_left_off_back_on() {
        started()
        screen(false)
        locator.up = null
        daemon.die()
        scheduler.runDue()
        power.calls.clear()

        val restarted = FakeEndpoint()
        locator.up = restarted
        scheduler.advance(1000)

        // The host takes the screen to be on after a new connection.
        assertEquals(listOf("unforceIdle", "screenOn"), power.calls)
        assertEquals(emptyList<Boolean>(), restarted.sentMessages().filter { it.hasDeviceIdle() }.map { it.deviceIdle.idle })
    }

    @Test
    fun fr10_the_hosts_battery_becomes_the_guests() {
        started()
        val onBattery = PowerState.newBuilder()
            .setAcOnline(false)
            .setBatteryPresent(true)
            .setBatteryPercent(57)
            .setPowerSaver(true)
            .build()
        daemon.deliver(Envelope.newBuilder().setPowerState(onBattery).build().toByteArray())
        scheduler.runDue()

        assertEquals(listOf(Battery(acOnline = false, present = true, percent = 57, charging = false)), power.batteries)
        assertEquals(true, power.saver)
    }

    @Test
    fun fr10_a_host_without_a_battery_is_a_full_battery_on_ac_power() {
        started()
        val desktop = PowerState.newBuilder().setAcOnline(true).setBatteryPresent(false).build()
        daemon.deliver(Envelope.newBuilder().setPowerState(desktop).build().toByteArray())
        scheduler.runDue()

        assertEquals(listOf(Battery(acOnline = true, present = false, percent = 100, charging = false)), power.batteries)
        assertEquals(false, power.saver)
    }

    @Test
    fun nfr5_a_power_control_that_fails_is_logged_and_the_agent_goes_on() {
        started()
        power.failing = true

        screen(false)

        assertTrue(log.lines.any { it.startsWith("warn: putting the device into Doze failed") })
        assertEquals(emptyList<Boolean>(), idleReports())
        // The agent still acts on the next word from the host.
        screen(true)
        assertEquals("screenOn", power.calls.last())
    }
}

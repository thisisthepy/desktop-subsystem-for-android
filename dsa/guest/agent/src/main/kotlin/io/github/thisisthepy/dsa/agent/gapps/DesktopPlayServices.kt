package io.github.thisisthepy.dsa.agent.gapps

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/** The package manager's enabled state of other apps' components, as the agent reaches it. */
interface ComponentStates {
    /** Whether [packageName] is installed. */
    fun installed(packageName: String): Boolean

    /**
     * Whether the component is turned off (disabled). Null when the package has no component
     * of that name, which happens as a Play services version drops or renames one.
     */
    fun isOff(packageName: String, className: String): Boolean?

    /**
     * Turns the components off in one call, without stopping the package's processes: the
     * activity manager ends the services among them, and the rest of the process runs on.
     */
    fun turnOff(packageName: String, classNames: List<String>)

    /**
     * Calls [onChange] with the package's name, on any thread, when a package is installed,
     * updated, or has a component turned on or off.
     */
    fun listen(onChange: (String) -> Unit)

    companion object {
        val NONE = object : ComponentStates {
            override fun installed(packageName: String) = false
            override fun isOff(packageName: String, className: String): Boolean? = null
            override fun turnOff(packageName: String, classNames: List<String>) = Unit
            override fun listen(onChange: (String) -> Unit) = Unit
        }
    }
}

/** A component of Google Play services the agent keeps off, and what it does on a phone. */
data class UnusedComponent(val className: String, val feature: String)

/**
 * Keeps off the parts of Google Play services that run all the time on a phone and have no
 * use on a desktop: crisis alerts, the emergency location service, motion sickness assist,
 * nearby device presence, Cast device suggestions, remote gestures from a watch, the
 * hardware activity recognition provider, Find My Device's secure lock, the cross-device
 * service, Find My Device's Bluetooth offline beacon, the GNSS assistance download, and the
 * logging library's debug dump. The beacon advertises over Bluetooth and the assistance data
 * feeds a satellite receiver; a desktop's Android has neither radio, so both only hold memory.
 * Location for apps comes from the fused and network location services, which stay on. The
 * debug dump only answers `dumpsys` for Play services' own logging. Each is a service Play services starts at boot and keeps running in its main
 * process, so each costs resident memory for as long as Android runs. None of them carries
 * push messages, accounts, the Play Store, location for apps, Play Integrity or backup; those
 * run in other services of the same process, which stay as they are.
 *
 * The state is put back whenever it may have been lost: when the agent starts (at every
 * boot), when Play services is installed or updated, and when its components change. Play
 * services turns its own components on and off as its modules download and its server flags
 * change, and each of those changes reaches the agent as a component change of the package,
 * so a component it turns back on is off again a moment later ([SETTLE_MILLIS], which lets a
 * burst of changes end first). The services are turned off without ending the process, so
 * push connections and sign-ins survive.
 *
 * Should Play services keep turning a component back on, the agent stops after
 * [MAX_REAPPLY] times within [REAPPLY_WINDOW_MILLIS] and logs it, rather than fight it
 * forever.
 *
 * Without Google Play services installed this does nothing. Not thread safe: the agent calls
 * it from its scheduler's thread.
 */
class DesktopPlayServices(
    private val states: ComponentStates,
    private val scheduler: Scheduler,
    private val log: Log,
    private val clock: () -> Long,
    private val components: List<UnusedComponent> = UNUSED,
) {
    private var pending = false

    /** Whether anything was turned off yet, since the agent started. */
    private var turnedOffOnce = false

    /** When each component was found turned back on, within the last window. */
    private val reapplied = mutableMapOf<String, ArrayDeque<Long>>()
    private val givenUp = mutableSetOf<String>()

    /** Turns the components off now, and again whenever Play services changes. */
    fun start() {
        try {
            states.listen { packageName ->
                if (packageName == PACKAGE) scheduler.post(::changed)
            }
        } catch (e: Exception) {
            log.warn("listening to Google Play services' changes failed; components it turns back on stay on until Android starts again", e)
        }
        apply(first = true)
    }

    private fun changed() {
        if (pending) return
        pending = true
        scheduler.postDelayed(SETTLE_MILLIS) {
            pending = false
            apply(first = false)
        }
    }

    private fun apply(first: Boolean) {
        val on = try {
            if (!states.installed(PACKAGE)) return
            components.filter { it.className !in givenUp && states.isOff(PACKAGE, it.className) == false }
        } catch (e: Exception) {
            log.warn("could not read which Google Play services components are on", e)
            return
        }
        if (on.isEmpty()) return
        val now = clock()
        val turning = if (first) on else on.filter { component -> allowedAgain(component, now) }
        if (turning.isEmpty()) return
        try {
            states.turnOff(PACKAGE, turning.map { it.className })
        } catch (e: Exception) {
            log.warn("could not turn off Google Play services components a desktop does not use", e)
            return
        }
        val what = turning.joinToString { "${it.feature} (${it.className.substringAfterLast('.')})" }
        if (!turnedOffOnce) {
            turnedOffOnce = true
            log.info("turned off Google Play services components a desktop does not use: $what")
        } else {
            log.info("turned off again Google Play services components it had turned back on: $what")
        }
    }

    /** Whether [component], found on again, may be turned off once more. */
    private fun allowedAgain(component: UnusedComponent, now: Long): Boolean {
        val times = reapplied.getOrPut(component.className) { ArrayDeque() }
        while (times.isNotEmpty() && now - times.first() >= REAPPLY_WINDOW_MILLIS) times.removeFirst()
        if (times.size >= MAX_REAPPLY) {
            givenUp += component.className
            log.warn(
                "Google Play services turned ${component.feature} (${component.className}) back on " +
                    "$MAX_REAPPLY times within ${REAPPLY_WINDOW_MILLIS / 60_000} minutes; leaving it on " +
                    "until Android starts again",
            )
            return false
        }
        times.addLast(now)
        return true
    }

    companion object {
        const val PACKAGE = "com.google.android.gms"

        /** How long a burst of Play services' component changes has to end before a check. */
        const val SETTLE_MILLIS = 2_000L

        /** How many times in [REAPPLY_WINDOW_MILLIS] a component is turned off again at most. */
        const val MAX_REAPPLY = 5
        const val REAPPLY_WINDOW_MILLIS = 60 * 60_000L

        /** What is kept off, read from Play services 26.43 on Android 17. */
        val UNUSED = listOf(
            UnusedComponent("com.google.android.gms.crisisalerts.init.CrisisAlertsPersistentService", "crisis alerts"),
            UnusedComponent("com.google.android.gms.thunderbird.EmergencyPersistentService", "emergency location service"),
            UnusedComponent("com.google.android.gms.thunderbird.EmergencyLocationService", "emergency location service"),
            UnusedComponent(
                "com.google.android.gms.motionsicknessassist.persistent.MotionSicknessAssistPersistentService",
                "motion sickness assist",
            ),
            UnusedComponent(
                "com.google.android.gms.motionsicknessassist.service.MotionSicknessAssistService",
                "motion sickness assist",
            ),
            UnusedComponent(
                "com.google.android.gms.motionsicknessassist.service.MotionAssistSysUiService",
                "motion sickness assist",
            ),
            UnusedComponent(
                "com.google.android.gms.motionsicknessassist.settings.MotionSicknessAssistTileService",
                "motion sickness assist",
            ),
            UnusedComponent("com.google.android.gms.nearby.presence.service.LifeCycleService", "nearby device presence"),
            UnusedComponent(
                "com.google.android.gms.cast.devicesuggestions.SuggestionsProviderChimeraService",
                "Cast device suggestions",
            ),
            UnusedComponent("com.google.android.gms.remotegestures.api.RemoteGesturesService", "remote gestures"),
            UnusedComponent(
                "com.google.android.location.internal.server.HardwareArProviderService",
                "hardware activity recognition",
            ),
            UnusedComponent("com.google.android.gms.mdm.services.SecureLockService", "Find My Device secure lock"),
            UnusedComponent("com.google.android.gms.dtdi.lifecycle.LifecycleService", "cross-device services"),
            UnusedComponent(
                "com.google.android.gms.findmydevice.spot.bleadvertising.OfflineBeaconService_Persistent",
                "Find My Device Bluetooth offline beacon",
            ),
            UnusedComponent(
                "com.google.android.gms.location.gnssassistance.GnssAssistanceService",
                "GNSS assistance download",
            ),
            UnusedComponent("com.google.android.gms.clearcut.debug.ClearcutDebugDumpService", "logging debug dump"),
        )
    }
}

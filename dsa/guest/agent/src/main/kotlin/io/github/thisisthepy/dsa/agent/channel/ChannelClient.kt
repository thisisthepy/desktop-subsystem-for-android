package io.github.thisisthepy.dsa.agent.channel

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/** One connection to the guest's control daemon. Calls throw when the daemon is gone. */
interface ChannelEndpoint {
    /** Sends one control frame (one encoded Envelope) to the host. */
    fun send(frame: ByteArray)

    /** Delivers every frame for the agent to [onFrame], on any thread. */
    fun subscribe(onFrame: (ByteArray) -> Unit)

    /** Calls [onGone], on any thread, once the daemon behind this endpoint dies. */
    fun onGone(onGone: () -> Unit)
}

/** Finds the control daemon's service; null while it is not up. */
fun interface ChannelLocator {
    fun find(): ChannelEndpoint?
}

/**
 * Keeps the agent connected to the guest's control daemon.
 *
 * The daemon may start after the agent, and init restarts it when it exits. Until its
 * service is found the client looks again every [retryMillis]; once found it subscribes and
 * calls [onConnected]. When the daemon dies it starts looking again. Frames for the agent
 * reach [onFrame] on the scheduler's thread, unless [intercept] takes them first.
 *
 * Every method runs on the scheduler's thread.
 */
class ChannelClient(
    private val locator: ChannelLocator,
    private val scheduler: Scheduler,
    private val log: Log,
    private val onFrame: (ByteArray) -> Unit,
    private val onConnected: () -> Unit,
    private val retryMillis: Long = 1000,
    /**
     * Sees every frame first, on the thread the daemon delivers it on, so a frame that must
     * not wait behind the scheduler's work need not. True when it took the frame.
     */
    private val intercept: (ByteArray) -> Boolean = { false },
) {
    private var endpoint: ChannelEndpoint? = null
    private var generation = 0
    private var started = false

    val connected: Boolean get() = endpoint != null

    fun start() {
        if (started) return
        started = true
        connect()
    }

    /** Sends a frame; false when there is no connection or the daemon died meanwhile. */
    fun send(frame: ByteArray): Boolean {
        val current = endpoint ?: return false
        return try {
            current.send(frame)
            true
        } catch (e: Exception) {
            log.warn("the control daemon did not take a frame; looking for it again", e)
            lost(generation)
            false
        }
    }

    private fun connect() {
        val found = try {
            locator.find()
        } catch (e: Exception) {
            log.warn("looking up the control daemon failed", e)
            null
        }
        if (found == null) {
            scheduler.postDelayed(retryMillis) { connect() }
            return
        }
        val mine = ++generation
        try {
            found.onGone { scheduler.post { lost(mine) } }
            found.subscribe { frame ->
                if (!intercept(frame)) scheduler.post { if (mine == generation) onFrame(frame) }
            }
        } catch (e: Exception) {
            log.warn("subscribing to the control daemon failed; trying again", e)
            scheduler.postDelayed(retryMillis) { connect() }
            return
        }
        endpoint = found
        log.info("connected to the control daemon")
        onConnected()
    }

    private fun lost(which: Int) {
        if (which != generation || endpoint == null) return
        endpoint = null
        generation++
        log.warn("the control daemon is gone; waiting for it to come back")
        scheduler.postDelayed(retryMillis) { connect() }
    }
}

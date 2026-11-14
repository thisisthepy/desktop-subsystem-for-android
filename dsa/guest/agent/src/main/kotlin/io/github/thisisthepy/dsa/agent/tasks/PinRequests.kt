package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.Log

/**
 * An app's request to pin one of its shortcuts, as Android hands it to the default launcher
 * (LauncherApps.PinItemRequest).
 */
interface PinRequest {
    /** The app that asks. */
    val packageName: String

    /** The app's own name, for the question the host asks. */
    val appLabel: String

    /** The shortcut to pin, with its icon. */
    val shortcut: LauncherShortcut

    /** Pins the shortcut in Android. False when Android no longer takes the request. */
    fun accept(): Boolean
}

/**
 * The pin requests the host has been asked about and has not answered.
 *
 * Each request gets an ID and goes to the host as PinShortcutRequest; the host asks the user.
 * A PinShortcutAnswer that accepts it pins the shortcut, and the host makes a desktop shortcut
 * for it; any other answer lets it go. A request with no answer lapses after
 * [LAPSE_MILLIS], and a lost connection drops every one: the host that was asked is gone.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class PinRequests(
    /** Sends one control frame; false when it could not be sent. */
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    /** Runs work after a delay, on the agent's thread. */
    private val later: (Long, () -> Unit) -> Unit,
) {
    private val waiting = HashMap<Int, PinRequest>()
    private var lastId = 0

    /** Asks the host about [request]. False when it could not be asked, so the app's request lapses. */
    fun requested(request: PinRequest): Boolean {
        if (request.packageName.isEmpty() || request.shortcut.id.isEmpty()) {
            log.warn("dropped a pin request that names no app or no shortcut")
            return false
        }
        lastId = if (lastId == Int.MAX_VALUE) 1 else lastId + 1
        val id = lastId
        val frame = Messages.pinShortcutRequest(id, request.packageName, request.appLabel, request.shortcut)
        if (!send(frame)) {
            log.warn("could not ask the host to pin ${request.shortcut.id} of ${request.packageName}; the request lapses")
            return false
        }
        waiting[id] = request
        log.info("asked the host to pin ${request.shortcut.id} of ${request.packageName} (request $id)")
        later(LAPSE_MILLIS) {
            if (waiting.remove(id) != null) log.info("pin request $id had no answer and lapsed")
        }
        return true
    }

    /** The host's answer to request [requestId]. */
    fun answered(requestId: Int, accepted: Boolean) {
        val request = waiting.remove(requestId)
        if (request == null) {
            log.warn("the host answered pin request $requestId, which is not waiting")
            return
        }
        if (!accepted) {
            log.info("the user declined to pin ${request.shortcut.id} of ${request.packageName}")
            return
        }
        val pinned = try {
            request.accept()
        } catch (e: Exception) {
            log.warn("pinning ${request.shortcut.id} of ${request.packageName} failed", e)
            false
        }
        if (pinned) {
            log.info("pinned ${request.shortcut.id} of ${request.packageName}")
        } else {
            log.warn("Android no longer takes the pin request for ${request.shortcut.id} of ${request.packageName}")
        }
    }

    /** Drops every request, for a lost connection. */
    fun disconnected() {
        waiting.clear()
    }

    /** How many requests wait for an answer. */
    val pending: Int get() = waiting.size

    companion object {
        /** How long a request waits for the user's answer. */
        const val LAPSE_MILLIS = 5 * 60 * 1000L
    }
}

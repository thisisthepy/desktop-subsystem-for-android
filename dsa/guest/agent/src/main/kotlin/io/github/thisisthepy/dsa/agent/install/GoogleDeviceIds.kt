package io.github.thisisthepy.dsa.agent.install

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.GoogleDeviceId

/** Google's services framework, as the agent asks it for the device's Android ID. */
fun interface GoogleDeviceIdPort {
    /** The Google Services Framework Android ID in decimal, or null while there is none. */
    fun read(): String?

    companion object {
        val NONE = GoogleDeviceIdPort { null }
    }
}

/**
 * Answers the host's GoogleDeviceIdQuery with the Google Services Framework Android ID, which
 * Google's page for uncertified devices asks for before Google's apps let the user sign in. It
 * exists only once the user has installed Google Play and its services framework has started;
 * until then, and when asking fails, the answer is empty, and a failure is logged.
 */
class GoogleDeviceIds(
    private val port: GoogleDeviceIdPort,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
) {
    /** Takes a GoogleDeviceIdQuery frame. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.GOOGLE_DEVICE_ID_QUERY) return false
        val id = try {
            port.read()?.trim()?.takeIf { it.isNotEmpty() && it.all(Char::isDigit) }
        } catch (e: Exception) {
            log.warn("could not read the Google Services Framework Android ID", e)
            null
        }
        val answer = GoogleDeviceId.newBuilder().setId(id ?: "")
        send(Envelope.newBuilder().setGoogleDeviceId(answer).build().toByteArray())
        return true
    }
}

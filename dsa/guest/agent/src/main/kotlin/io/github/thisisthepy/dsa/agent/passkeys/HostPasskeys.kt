package io.github.thisisthepy.dsa.agent.passkeys

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.channel.HostCalls
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PasskeyOperation
import io.github.thisisthepy.dsa.control.v1.PasskeyRequest
import io.github.thisisthepy.dsa.control.v1.PasskeyStatus
import java.security.MessageDigest

/**
 * What Android's credential manager (system_server) says of the app that called it, in its
 * CallingAppInfo. The app has no say in any of it.
 */
class CallerFacts(
    val packageName: String,
    /**
     * The DER form of each certificate the app is signed with now: every signer of an app
     * with several, otherwise the newest of its signing certificate history.
     */
    val signingCertificates: List<ByteArray>,
    /** The web origin the credential manager reports for the call; null when it reports none. */
    val origin: String?,
    /** The app's name, from the package manager, for the host's prompt. */
    val appLabel: String,
    /** The app is part of the image (a system app that was not updated). */
    val fromImage: Boolean,
)

/** Which passkey ceremony. */
enum class PasskeyCeremony { CREATE, GET }

/** How a ceremony that did not succeed failed, which picks the exception the app gets. */
enum class PasskeyFailure {
    /** The person canceled the host's prompt. */
    CANCELED,

    /** The request is not allowed: the relying party does not list the app, or no passkey fits. */
    NOT_ALLOWED,

    /** This computer has no passkeys, or they are turned off. */
    UNAVAILABLE,

    /** Anything else, the host's authenticator failing or the host not answering. */
    ERROR,
}

/** What became of a passkey request. */
sealed class PasskeyResult {
    /** WebAuthn's RegistrationResponseJSON (create) or AuthenticationResponseJSON (get). */
    data class Done(val responseJson: String) : PasskeyResult()

    data class Failed(val failure: PasskeyFailure, val message: String) : PasskeyResult()
}

/**
 * The agent's credential provider, apart from Android: it hands an app's passkey request to the
 * host, whose own authenticator (Windows Hello, macOS passkeys) makes or uses the passkey after
 * the host has checked that the app may speak for the relying party.
 *
 * # Trust boundary
 *
 * The host decides which relying party a request may name from who the caller is, so nothing
 * that names the caller may come from the caller:
 * - the package and its signing certificates come only from the credential manager's
 *   CallingAppInfo (system_server reads them from the package manager), never from the app's
 *   request JSON or from anything the app put in its intent;
 * - a web origin is passed on only as the credential manager reports it, and only for a
 *   browser trusted to report one ([PrivilegedBrowsers], or a browser the image itself
 *   carries). The credential manager reports any origin an app holding the normal permission
 *   CREDENTIAL_MANAGER_SET_ORIGIN sets, so an origin from any other app is refused, as is an
 *   origin written into the request JSON, which is never read for it;
 * - a client data hash is passed on only with such an origin: only a browser makes its own
 *   client data.
 *
 * The request JSON goes to the host as it came; the host reads the relying party ID from it and
 * checks it against the caller, which is the point of the check.
 */
class HostPasskeys(
    private val calls: HostCalls,
    private val log: Log,
    private val waitMillis: Long = WAIT_MILLIS,
    private val privileged: (String, List<String>) -> Boolean = PrivilegedBrowsers::listed,
) {
    /**
     * Asks the host for [ceremony] on behalf of [caller]. [clientDataHash] is what the
     * framework's request carries, if anything. Blocks until the host answers.
     */
    fun request(
        ceremony: PasskeyCeremony,
        caller: CallerFacts,
        requestJson: String,
        clientDataHash: ByteArray?,
    ): PasskeyResult {
        if (requestJson.isEmpty() || requestJson.length > MAX_REQUEST_CHARS) {
            return PasskeyResult.Failed(PasskeyFailure.ERROR, "the passkey request is empty or too large")
        }
        if (caller.signingCertificates.isEmpty()) {
            return PasskeyResult.Failed(PasskeyFailure.NOT_ALLOWED, "Android did not say what the app is signed with")
        }
        val signers = caller.signingCertificates.map { sha256(it) }
        val origin = caller.origin?.takeIf { it.isNotEmpty() }
        if (origin != null && !trustedBrowser(caller, signers)) {
            log.warn("${caller.packageName} named a web origin for a passkey, but it is no browser trusted to")
            return PasskeyResult.Failed(
                PasskeyFailure.NOT_ALLOWED,
                "${caller.appLabel.ifEmpty { caller.packageName }} may not ask for passkeys on behalf of websites",
            )
        }
        val hash = clientDataHash?.takeIf { it.isNotEmpty() }
        if (hash != null && origin == null) {
            return PasskeyResult.Failed(
                PasskeyFailure.NOT_ALLOWED,
                "only a browser naming its website may hand over its own client data",
            )
        }
        val id = calls.nextId()
        val message = PasskeyRequest.newBuilder()
            .setRequestId(id)
            .setOperation(
                if (ceremony == PasskeyCeremony.CREATE) {
                    PasskeyOperation.PASSKEY_OPERATION_CREATE
                } else {
                    PasskeyOperation.PASSKEY_OPERATION_GET
                },
            )
            .setRequestJson(requestJson)
            .setPackage(caller.packageName)
            .addAllSignerSha256(signers.map { ByteString.copyFrom(it) })
            .setOrigin(origin.orEmpty())
            .setAppLabel(caller.appLabel)
        if (hash != null) message.setClientDataHash(ByteString.copyFrom(hash))
        val answer = calls.call(id, Envelope.newBuilder().setPasskeyRequest(message).build(), waitMillis)
            ?.takeIf { it.hasPasskeyResponse() }?.passkeyResponse
            ?: return PasskeyResult.Failed(PasskeyFailure.ERROR, "this computer did not answer the passkey request")
        val why = answer.message.ifEmpty { "this computer could not complete the passkey request" }
        return when (answer.status) {
            PasskeyStatus.PASSKEY_STATUS_OK ->
                if (answer.responseJson.isEmpty()) {
                    PasskeyResult.Failed(PasskeyFailure.ERROR, "this computer answered without a passkey")
                } else {
                    PasskeyResult.Done(answer.responseJson)
                }
            PasskeyStatus.PASSKEY_STATUS_CANCELED -> PasskeyResult.Failed(PasskeyFailure.CANCELED, why)
            PasskeyStatus.PASSKEY_STATUS_REJECTED,
            PasskeyStatus.PASSKEY_STATUS_NOT_ALLOWED,
            -> PasskeyResult.Failed(PasskeyFailure.NOT_ALLOWED, why)
            PasskeyStatus.PASSKEY_STATUS_UNAVAILABLE -> PasskeyResult.Failed(PasskeyFailure.UNAVAILABLE, why)
            else -> PasskeyResult.Failed(PasskeyFailure.ERROR, why)
        }
    }

    /** A browser trusted to name the website it calls for. */
    private fun trustedBrowser(caller: CallerFacts, signers: List<ByteArray>): Boolean =
        caller.fromImage || privileged(caller.packageName, signers.map(::hex))

    companion object {
        /** The longest the host lets a ceremony wait for the person, and a little more. */
        const val WAIT_MILLIS = 11 * 60_000L

        /** The host takes no larger request. */
        const val MAX_REQUEST_CHARS = 64 * 1024

        /** The type of a passkey in Android's credential manager. */
        const val TYPE_PUBLIC_KEY_CREDENTIAL = "androidx.credentials.TYPE_PUBLIC_KEY_CREDENTIAL"

        /** Where androidx.credentials keeps the request and the response in a request's data. */
        const val BUNDLE_KEY_REQUEST_JSON = "androidx.credentials.BUNDLE_KEY_REQUEST_JSON"
        const val BUNDLE_KEY_CLIENT_DATA_HASH = "androidx.credentials.BUNDLE_KEY_CLIENT_DATA_HASH"
        const val BUNDLE_KEY_REGISTRATION_RESPONSE_JSON = "androidx.credentials.BUNDLE_KEY_REGISTRATION_RESPONSE_JSON"
        const val BUNDLE_KEY_AUTHENTICATION_RESPONSE_JSON = "androidx.credentials.BUNDLE_KEY_AUTHENTICATION_RESPONSE_JSON"

        /** androidx.credentials' type of a WebAuthn NotAllowedError, as an app reads it. */
        const val NOT_ALLOWED_ERROR = "androidx.credentials.TYPE_NOT_ALLOWED_ERROR"
        const val CREATE_DOM_EXCEPTION = "androidx.credentials.TYPE_CREATE_PUBLIC_KEY_CREDENTIAL_DOM_EXCEPTION"
        const val GET_DOM_EXCEPTION = "androidx.credentials.TYPE_GET_PUBLIC_KEY_CREDENTIAL_DOM_EXCEPTION"

        /**
         * The exception type an app gets for [failure] of [ceremony], or null for the
         * framework's own types, which the platform's provider names from the SDK.
         */
        fun domExceptionType(ceremony: PasskeyCeremony, failure: PasskeyFailure): String? =
            if (failure == PasskeyFailure.NOT_ALLOWED) {
                (if (ceremony == PasskeyCeremony.CREATE) CREATE_DOM_EXCEPTION else GET_DOM_EXCEPTION) +
                    "/" + NOT_ALLOWED_ERROR
            } else {
                null
            }

        fun sha256(data: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(data)

        fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }
    }
}

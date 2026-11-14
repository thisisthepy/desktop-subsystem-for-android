package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.credentials.CreateCredentialException
import android.credentials.CreateCredentialResponse
import android.credentials.Credential
import android.credentials.GetCredentialException
import android.credentials.GetCredentialResponse
import android.os.Bundle
import android.service.credentials.CallingAppInfo
import android.service.credentials.CreateCredentialRequest
import android.service.credentials.CredentialProviderService
import android.service.credentials.GetCredentialRequest
import io.github.thisisthepy.dsa.agent.passkeys.CallerFacts
import io.github.thisisthepy.dsa.agent.passkeys.HostPasskeys
import io.github.thisisthepy.dsa.agent.passkeys.PasskeyCeremony
import io.github.thisisthepy.dsa.agent.passkeys.PasskeyFailure
import io.github.thisisthepy.dsa.agent.passkeys.PasskeyResult

/**
 * What the credential selector opens when the person picks "this computer" for a passkey. It
 * shows nothing: it reads the request the credential manager put in its intent, hands it to
 * the host ([HostPasskeys]), whose authenticator shows its own prompt, and returns the result
 * to the credential manager, which gives it to the app.
 *
 * Who the app is comes only from the request's CallingAppInfo, which system_server fills in;
 * see [HostPasskeys] for why nothing the app says of itself is used.
 */
class PasskeyActivity : Activity() {
    private var answered = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState != null) {
            // A request is answered once; a recreated activity has nothing to ask.
            finish()
            return
        }
        val ceremony = try {
            PasskeyCeremony.valueOf(intent.getStringExtra(EXTRA_CEREMONY).orEmpty())
        } catch (e: IllegalArgumentException) {
            finish()
            return
        }
        val passkeys = AgentApplication.agent?.passkeys
        val request = read(ceremony)
        if (passkeys == null || request == null) {
            fail(ceremony, PasskeyFailure.ERROR, if (passkeys == null) "DSA's agent is not running" else "the request could not be read")
            return
        }
        val (caller, json, hash) = request
        Thread({
            val result = try {
                passkeys.request(ceremony, caller, json, hash)
            } catch (e: Exception) {
                AndroidLog.warn("a passkey request failed", e)
                PasskeyResult.Failed(PasskeyFailure.ERROR, "the passkey request failed")
            }
            runOnUiThread {
                when (result) {
                    is PasskeyResult.Done -> succeed(ceremony, result.responseJson)
                    is PasskeyResult.Failed -> fail(ceremony, result.failure, result.message)
                }
            }
        }, "dsa-passkey").start()
    }

    /** The calling app, the request JSON and the client data hash, from the framework's request. */
    private fun read(ceremony: PasskeyCeremony): Triple<CallerFacts, String, ByteArray?>? {
        return when (ceremony) {
            PasskeyCeremony.CREATE -> {
                val request = intent.getParcelableExtra(
                    CredentialProviderService.EXTRA_CREATE_CREDENTIAL_REQUEST,
                    CreateCredentialRequest::class.java,
                ) ?: return null
                if (request.type != HostPasskeys.TYPE_PUBLIC_KEY_CREDENTIAL) return null
                val data = request.data
                val json = data.getString(HostPasskeys.BUNDLE_KEY_REQUEST_JSON) ?: return null
                Triple(caller(request.callingAppInfo), json, data.getByteArray(HostPasskeys.BUNDLE_KEY_CLIENT_DATA_HASH))
            }
            PasskeyCeremony.GET -> {
                val request = intent.getParcelableExtra(
                    CredentialProviderService.EXTRA_GET_CREDENTIAL_REQUEST,
                    GetCredentialRequest::class.java,
                ) ?: return null
                val option = request.credentialOptions.firstOrNull { it.type == HostPasskeys.TYPE_PUBLIC_KEY_CREDENTIAL }
                    ?: return null
                val data = option.credentialRetrievalData
                val json = data.getString(HostPasskeys.BUNDLE_KEY_REQUEST_JSON) ?: return null
                Triple(caller(request.callingAppInfo), json, data.getByteArray(HostPasskeys.BUNDLE_KEY_CLIENT_DATA_HASH))
            }
        }
    }

    /** What the credential manager says of the calling app, and the package manager of its name. */
    private fun caller(info: CallingAppInfo): CallerFacts {
        val signing = info.signingInfo
        val certificates = if (signing.hasMultipleSigners()) {
            signing.apkContentsSigners.toList()
        } else {
            listOfNotNull(signing.signingCertificateHistory.lastOrNull())
        }
        val app: ApplicationInfo? = try {
            packageManager.getApplicationInfo(info.packageName, 0)
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
        val fromImage = app != null && (app.flags and ApplicationInfo.FLAG_SYSTEM) != 0 &&
            (app.flags and ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) == 0
        return CallerFacts(
            packageName = info.packageName,
            signingCertificates = certificates.map { it.toByteArray() },
            origin = info.origin,
            appLabel = app?.let { packageManager.getApplicationLabel(it).toString() }.orEmpty(),
            fromImage = fromImage,
        )
    }

    private fun succeed(ceremony: PasskeyCeremony, json: String) {
        val data = Bundle()
        val result = Intent()
        when (ceremony) {
            PasskeyCeremony.CREATE -> {
                data.putString(HostPasskeys.BUNDLE_KEY_REGISTRATION_RESPONSE_JSON, json)
                result.putExtra(CredentialProviderService.EXTRA_CREATE_CREDENTIAL_RESPONSE, CreateCredentialResponse(data))
            }
            PasskeyCeremony.GET -> {
                data.putString(HostPasskeys.BUNDLE_KEY_AUTHENTICATION_RESPONSE_JSON, json)
                val credential = Credential(HostPasskeys.TYPE_PUBLIC_KEY_CREDENTIAL, data)
                result.putExtra(CredentialProviderService.EXTRA_GET_CREDENTIAL_RESPONSE, GetCredentialResponse(credential))
            }
        }
        answer(result)
    }

    private fun fail(ceremony: PasskeyCeremony, failure: PasskeyFailure, message: String) {
        val result = Intent()
        val dom = HostPasskeys.domExceptionType(ceremony, failure)
        when (ceremony) {
            PasskeyCeremony.CREATE -> {
                val type = dom ?: when (failure) {
                    PasskeyFailure.CANCELED -> CreateCredentialException.TYPE_USER_CANCELED
                    PasskeyFailure.UNAVAILABLE -> CreateCredentialException.TYPE_NO_CREATE_OPTIONS
                    else -> CreateCredentialException.TYPE_UNKNOWN
                }
                result.putExtra(CredentialProviderService.EXTRA_CREATE_CREDENTIAL_EXCEPTION, CreateCredentialException(type, message))
            }
            PasskeyCeremony.GET -> {
                val type = dom ?: when (failure) {
                    PasskeyFailure.CANCELED -> GetCredentialException.TYPE_USER_CANCELED
                    PasskeyFailure.UNAVAILABLE -> GetCredentialException.TYPE_NO_CREDENTIAL
                    else -> GetCredentialException.TYPE_UNKNOWN
                }
                result.putExtra(CredentialProviderService.EXTRA_GET_CREDENTIAL_EXCEPTION, GetCredentialException(type, message))
            }
        }
        answer(result)
    }

    private fun answer(result: Intent) {
        if (answered) return
        answered = true
        setResult(RESULT_OK, result)
        finish()
    }

    companion object {
        const val EXTRA_CEREMONY = "io.github.thisisthepy.dsa.agent.extra.PASSKEY_CEREMONY"
    }
}

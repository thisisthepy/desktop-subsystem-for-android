package io.github.thisisthepy.dsa.agent.passkeys

import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.channel.HostCalls
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PasskeyOperation
import io.github.thisisthepy.dsa.control.v1.PasskeyRequest
import io.github.thisisthepy.dsa.control.v1.PasskeyResponse
import io.github.thisisthepy.dsa.control.v1.PasskeyStatus
import java.security.MessageDigest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HostPasskeysTest {
    private val log = RecordingLog()
    private val requests = mutableListOf<PasskeyRequest>()
    private var status = PasskeyStatus.PASSKEY_STATUS_OK
    private var responseJson = """{"id":"abc","response":{}}"""
    private lateinit var calls: HostCalls

    /** The host, faked: it records each request and answers it at once. */
    private fun passkeys(privileged: (String, List<String>) -> Boolean = { _, _ -> false }): HostPasskeys {
        calls = HostCalls({ frame ->
            val request = Envelope.parseFrom(frame).passkeyRequest
            requests += request
            calls.handle(
                Envelope.newBuilder().setPasskeyResponse(
                    PasskeyResponse.newBuilder().setRequestId(request.requestId).setStatus(status)
                        .setResponseJson(if (status == PasskeyStatus.PASSKEY_STATUS_OK) responseJson else "")
                        .setMessage("the host's reason"),
                ).build(),
            )
            true
        }, log)
        return HostPasskeys(calls, log, waitMillis = 1000, privileged = privileged)
    }

    private val certificate = byteArrayOf(0x30, 0x03, 0x02, 0x01, 0x01)
    private val json = """{"rp":{"id":"bank.example"},"challenge":"AAAA"}"""

    private fun app(origin: String? = null, fromImage: Boolean = false) = CallerFacts(
        packageName = "org.example.bank",
        signingCertificates = listOf(certificate),
        origin = origin,
        appLabel = "Bank",
        fromImage = fromImage,
    )

    @Test
    fun fr36_passkey_request_names_the_caller_as_android_reports_it() {
        val result = passkeys().request(PasskeyCeremony.CREATE, app(), json, null)
        assertEquals(PasskeyResult.Done(responseJson), result)
        val request = requests.single()
        assertEquals(PasskeyOperation.PASSKEY_OPERATION_CREATE, request.operation)
        assertEquals("org.example.bank", request.getPackage())
        assertEquals("Bank", request.appLabel)
        assertEquals(json, request.requestJson)
        assertArrayEquals(MessageDigest.getInstance("SHA-256").digest(certificate), request.getSignerSha256(0).toByteArray())
        assertEquals("an app names no origin", "", request.origin)
        assertTrue(request.clientDataHash.isEmpty)
    }

    @Test
    fun fr36_passkey_origin_is_passed_on_only_for_a_trusted_browser() {
        // An app that set an origin through the credential manager, but is no trusted browser.
        val refused = passkeys().request(PasskeyCeremony.GET, app(origin = "https://bank.example"), json, byteArrayOf(1, 2))
        assertEquals(PasskeyFailure.NOT_ALLOWED, (refused as PasskeyResult.Failed).failure)
        assertTrue("nothing reached the host", requests.isEmpty())
        // A listed browser, signed with its listed certificate.
        val hex = HostPasskeys.hex(HostPasskeys.sha256(certificate))
        val browser = passkeys { name, signers -> name == "org.example.bank" && signers == listOf(hex) }
        val hash = ByteArray(32) { 7 }
        browser.request(PasskeyCeremony.GET, app(origin = "https://bank.example"), json, hash)
        val request = requests.single()
        assertEquals(PasskeyOperation.PASSKEY_OPERATION_GET, request.operation)
        assertEquals("https://bank.example", request.origin)
        assertArrayEquals(hash, request.clientDataHash.toByteArray())
        // The image's own browser is trusted too.
        requests.clear()
        passkeys().request(PasskeyCeremony.GET, app(origin = "https://bank.example", fromImage = true), json, null)
        assertEquals("https://bank.example", requests.single().origin)
    }

    @Test
    fun fr36_passkey_client_data_hash_without_an_origin_is_refused() {
        val result = passkeys().request(PasskeyCeremony.GET, app(), json, ByteArray(32))
        assertEquals(PasskeyFailure.NOT_ALLOWED, (result as PasskeyResult.Failed).failure)
        assertTrue(requests.isEmpty())
    }

    @Test
    fun fr36_passkey_requests_without_a_signer_or_too_large_are_refused() {
        val unsigned = CallerFacts("org.example.bank", emptyList(), null, "Bank", false)
        assertTrue(passkeys().request(PasskeyCeremony.CREATE, unsigned, json, null) is PasskeyResult.Failed)
        val huge = "{\"x\":\"" + "a".repeat(HostPasskeys.MAX_REQUEST_CHARS) + "\"}"
        assertTrue(passkeys().request(PasskeyCeremony.CREATE, app(), huge, null) is PasskeyResult.Failed)
        assertTrue(requests.isEmpty())
    }

    @Test
    fun fr36_passkey_host_answers_become_the_apps_failures() {
        val expected = mapOf(
            PasskeyStatus.PASSKEY_STATUS_CANCELED to PasskeyFailure.CANCELED,
            PasskeyStatus.PASSKEY_STATUS_REJECTED to PasskeyFailure.NOT_ALLOWED,
            PasskeyStatus.PASSKEY_STATUS_NOT_ALLOWED to PasskeyFailure.NOT_ALLOWED,
            PasskeyStatus.PASSKEY_STATUS_UNAVAILABLE to PasskeyFailure.UNAVAILABLE,
            PasskeyStatus.PASSKEY_STATUS_ERROR to PasskeyFailure.ERROR,
        )
        for ((answer, failure) in expected) {
            status = answer
            val result = passkeys().request(PasskeyCeremony.GET, app(), json, null) as PasskeyResult.Failed
            assertEquals(failure, result.failure)
            assertEquals("the host's reason", result.message)
        }
        assertEquals(
            "androidx.credentials.TYPE_CREATE_PUBLIC_KEY_CREDENTIAL_DOM_EXCEPTION/androidx.credentials.TYPE_NOT_ALLOWED_ERROR",
            HostPasskeys.domExceptionType(PasskeyCeremony.CREATE, PasskeyFailure.NOT_ALLOWED),
        )
        assertEquals(
            "androidx.credentials.TYPE_GET_PUBLIC_KEY_CREDENTIAL_DOM_EXCEPTION/androidx.credentials.TYPE_NOT_ALLOWED_ERROR",
            HostPasskeys.domExceptionType(PasskeyCeremony.GET, PasskeyFailure.NOT_ALLOWED),
        )
        assertNull(HostPasskeys.domExceptionType(PasskeyCeremony.GET, PasskeyFailure.CANCELED))
    }

    @Test
    fun fr36_passkey_host_that_does_not_answer_fails_the_request() {
        val silent = HostPasskeys(HostCalls({ true }, log), log, waitMillis = 20)
        val result = silent.request(PasskeyCeremony.CREATE, app(), json, null) as PasskeyResult.Failed
        assertEquals(PasskeyFailure.ERROR, result.failure)
    }

    @Test
    fun fr36_passkey_privileged_browsers_are_matched_by_package_and_certificate() {
        val chrome = "f0fd6c5b410f25cb25c3b53346c8972fae30f8ee7411df910480ad6b2d60db83"
        assertTrue(PrivilegedBrowsers.listed("com.android.chrome", listOf(chrome)))
        assertTrue(PrivilegedBrowsers.listed("com.android.chrome", listOf(chrome.uppercase())))
        assertTrue("another package with Chrome's certificate", !PrivilegedBrowsers.listed("org.example.fake", listOf(chrome)))
        assertTrue("Chrome's name with another certificate", !PrivilegedBrowsers.listed("com.android.chrome", listOf("00".repeat(32))))
        assertTrue(!PrivilegedBrowsers.listed("com.android.chrome", emptyList()))
    }

    @Test
    fun fr36_passkey_provider_stays_enabled_and_takes_the_primary_place_only_when_free() {
        val ours = "io.github.thisisthepy.dsa.agent/io.github.thisisthepy.dsa.agent.platform.HostCredentialProvider"
        val google = "com.google.android.gms/com.google.android.gms.auth.api.credentials.credman.service.PasswordAndPasskeyService"
        assertEquals(ours, ProviderSetting.enabled(null, ours))
        assertEquals("$google:$ours", ProviderSetting.enabled(google, ours))
        assertNull(ProviderSetting.enabled("$google:$ours", ours))
        assertEquals(ours, ProviderSetting.primary("", ours))
        assertNull("a provider the user chose stays first", ProviderSetting.primary(google, ours))
    }
}

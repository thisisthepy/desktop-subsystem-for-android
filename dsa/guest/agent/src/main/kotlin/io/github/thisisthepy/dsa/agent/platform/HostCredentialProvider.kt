package io.github.thisisthepy.dsa.agent.platform

import android.app.PendingIntent
import android.app.slice.Slice
import android.app.slice.SliceSpec
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.credentials.ClearCredentialStateException
import android.credentials.CreateCredentialException
import android.credentials.GetCredentialException
import android.database.ContentObserver
import android.graphics.drawable.Icon
import android.net.Uri
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.os.OutcomeReceiver
import android.provider.Settings
import android.service.credentials.BeginCreateCredentialRequest
import android.service.credentials.BeginCreateCredentialResponse
import android.service.credentials.BeginGetCredentialOption
import android.service.credentials.BeginGetCredentialRequest
import android.service.credentials.BeginGetCredentialResponse
import android.service.credentials.ClearCredentialStateRequest
import android.service.credentials.CreateEntry
import android.service.credentials.CredentialEntry
import android.service.credentials.CredentialProviderService
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.R
import io.github.thisisthepy.dsa.agent.passkeys.HostPasskeys
import io.github.thisisthepy.dsa.agent.passkeys.PasskeyCeremony
import io.github.thisisthepy.dsa.agent.passkeys.ProviderSetting

/**
 * The guest's passkey provider: "this computer" in Android's credential selector, for every
 * app that makes or uses a passkey through Android's credential manager. Choosing it opens
 * [PasskeyActivity], which hands the request to the host (whose own authenticator performs the
 * ceremony) and gives the app the result. It offers passkeys only: passwords and other
 * credentials stay with the providers the user has.
 *
 * Only the credential manager may bind it (BIND_CREDENTIAL_PROVIDER_SERVICE). It is a system
 * provider: the agent is a privileged system app holding PROVIDE_DEFAULT_ENABLED_CREDENTIAL_SERVICE,
 * so the credential manager lists it whatever another provider writes into the settings. The
 * agent also keeps it in the enabled list ([AndroidCredentialSetup]).
 *
 * The entries carry nothing of the request: the activity reads the request, and the calling
 * app's identity, from what the credential manager hands it then.
 */
class HostCredentialProvider : CredentialProviderService() {
    override fun onBeginCreateCredential(
        request: BeginCreateCredentialRequest,
        cancellationSignal: CancellationSignal,
        callback: OutcomeReceiver<BeginCreateCredentialResponse, CreateCredentialException>,
    ) {
        if (request.type != HostPasskeys.TYPE_PUBLIC_KEY_CREDENTIAL || AgentApplication.agent == null) {
            callback.onError(CreateCredentialException(CreateCredentialException.TYPE_NO_CREATE_OPTIONS))
            return
        }
        val entry = CreateEntry(createSlice(pendingIntent(PasskeyCeremony.CREATE, 0)))
        callback.onResult(BeginCreateCredentialResponse.Builder().addCreateEntry(entry).build())
    }

    override fun onBeginGetCredential(
        request: BeginGetCredentialRequest,
        cancellationSignal: CancellationSignal,
        callback: OutcomeReceiver<BeginGetCredentialResponse, GetCredentialException>,
    ) {
        val builder = BeginGetCredentialResponse.Builder()
        if (AgentApplication.agent != null) {
            request.beginGetCredentialOptions
                .filter { it.type == HostPasskeys.TYPE_PUBLIC_KEY_CREDENTIAL }
                .forEachIndexed { index, option ->
                    val slice = credentialSlice(option, pendingIntent(PasskeyCeremony.GET, index + 1))
                    builder.addCredentialEntry(CredentialEntry(option, slice))
                }
        }
        callback.onResult(builder.build())
    }

    override fun onClearCredentialState(
        request: ClearCredentialStateRequest,
        cancellationSignal: CancellationSignal,
        callback: OutcomeReceiver<Void?, ClearCredentialStateException>,
    ) {
        // The host keeps no sign-in state for the guest's apps.
        callback.onResult(null)
    }

    /**
     * Opens [PasskeyActivity]. Mutable, because the credential manager adds the request to it;
     * the intent names the activity, so nothing else can receive it.
     */
    private fun pendingIntent(ceremony: PasskeyCeremony, requestCode: Int): PendingIntent {
        val intent = Intent(this, PasskeyActivity::class.java).putExtra(PasskeyActivity.EXTRA_CEREMONY, ceremony.name)
        return PendingIntent.getActivity(
            this,
            requestCode + if (ceremony == PasskeyCeremony.CREATE) 0 else 1000,
            intent,
            PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    private fun icon(): Icon = Icon.createWithResource("android", android.R.drawable.ic_lock_lock)

    /**
     * A create entry in the slice form androidx.credentials' CreateEntry writes, which the
     * credential selector reads: the account name, the icon, the pending intent and whether
     * it may be picked without asking.
     */
    private fun createSlice(pendingIntent: PendingIntent): Slice {
        val builder = Slice.Builder(Uri.EMPTY, SliceSpec(CREATE_SPEC, REVISION))
            .addText(getString(R.string.passkey_provider_account), null, listOf(CREATE_ACCOUNT_NAME))
            .addText(getString(R.string.passkey_provider_note), null, listOf(CREATE_NOTE))
            .addIcon(icon(), null, listOf(CREATE_ICON))
        return builder
            .addAction(pendingIntent, Slice.Builder(builder).addHints(listOf(CREATE_PENDING_INTENT)).build(), null)
            .addText(FALSE, null, listOf(CREATE_AUTO_SELECT_ALLOWED))
            .build()
    }

    /**
     * A credential entry in the slice form androidx.credentials' PublicKeyCredentialEntry
     * writes: the option it answers, the type's name, the title, the icon and the pending
     * intent. The host's authenticator shows its own list of the passkeys it holds.
     */
    private fun credentialSlice(option: BeginGetCredentialOption, pendingIntent: PendingIntent): Slice {
        val builder = Slice.Builder(Uri.EMPTY, SliceSpec(HostPasskeys.TYPE_PUBLIC_KEY_CREDENTIAL, REVISION))
            .addText(option.id, null, listOf(ENTRY_OPTION_ID))
            .addText(FALSE, null, listOf(ENTRY_IS_DEFAULT_ICON_PREFERRED))
            .addText(getString(R.string.passkey_type_name), null, listOf(ENTRY_TYPE_DISPLAY_NAME))
            .addText(getString(R.string.passkey_provider_account), null, listOf(ENTRY_TITLE))
            .addText(getString(R.string.passkey_provider_note), null, listOf(ENTRY_SUBTITLE))
            .addText(FALSE, null, listOf(ENTRY_AUTO_ALLOWED))
            .addIcon(icon(), null, listOf(ENTRY_ICON))
        return builder
            .addAction(pendingIntent, Slice.Builder(builder).addHints(listOf(ENTRY_PENDING_INTENT)).build(), null)
            .build()
    }

    private companion object {
        const val REVISION = 1
        const val FALSE = "false"
        const val CREATE_SPEC = "CreateEntry"
        const val CREATE_ACCOUNT_NAME = "androidx.credentials.provider.createEntry.SLICE_HINT_USER_PROVIDER_ACCOUNT_NAME"
        const val CREATE_NOTE = "androidx.credentials.provider.createEntry.SLICE_HINT_NOTE"
        const val CREATE_ICON = "androidx.credentials.provider.createEntry.SLICE_HINT_PROFILE_ICON"
        const val CREATE_PENDING_INTENT = "androidx.credentials.provider.createEntry.SLICE_HINT_PENDING_INTENT"
        const val CREATE_AUTO_SELECT_ALLOWED = "androidx.credentials.provider.createEntry.SLICE_HINT_AUTO_SELECT_ALLOWED"
        const val ENTRY = "androidx.credentials.provider.credentialEntry."
        const val ENTRY_OPTION_ID = ENTRY + "SLICE_HINT_OPTION_ID"
        const val ENTRY_IS_DEFAULT_ICON_PREFERRED = ENTRY + "SLICE_HINT_IS_DEFAULT_ICON_PREFERRED"
        const val ENTRY_TYPE_DISPLAY_NAME = ENTRY + "SLICE_HINT_TYPE_DISPLAY_NAME"
        const val ENTRY_TITLE = ENTRY + "SLICE_HINT_USER_NAME"
        const val ENTRY_SUBTITLE = ENTRY + "SLICE_HINT_CREDENTIAL_TYPE_DISPLAY_NAME"
        const val ENTRY_AUTO_ALLOWED = ENTRY + "SLICE_HINT_AUTO_ALLOWED"
        const val ENTRY_ICON = ENTRY + "SLICE_HINT_PROFILE_ICON"
        const val ENTRY_PENDING_INTENT = ENTRY + "SLICE_HINT_PENDING_INTENT"
    }
}

/**
 * Keeps the agent's credential provider enabled in the credential manager's settings, at
 * start and whenever another app rewrites them.
 */
class AndroidCredentialSetup(private val context: Context, private val log: Log) {
    private val component: String =
        ComponentName(context, HostCredentialProvider::class.java).flattenToString()

    fun start() {
        apply()
        val observer = object : ContentObserver(Handler(Looper.getMainLooper())) {
            override fun onChange(selfChange: Boolean) = apply()
        }
        val resolver = context.contentResolver
        resolver.registerContentObserver(Settings.Secure.getUriFor(ENABLED), false, observer)
        resolver.registerContentObserver(Settings.Secure.getUriFor(PRIMARY), false, observer)
    }

    private fun apply() {
        try {
            val resolver = context.contentResolver
            ProviderSetting.enabled(Settings.Secure.getString(resolver, ENABLED), component)?.let {
                Settings.Secure.putString(resolver, ENABLED, it)
                log.info("the host's passkeys are a credential provider again")
            }
            ProviderSetting.primary(Settings.Secure.getString(resolver, PRIMARY), component)?.let {
                Settings.Secure.putString(resolver, PRIMARY, it)
            }
        } catch (e: Exception) {
            log.warn("could not enable the host's passkeys as a credential provider", e)
        }
    }

    private companion object {
        /** Settings.Secure.CREDENTIAL_SERVICE and CREDENTIAL_SERVICE_PRIMARY, which the SDK hides. */
        const val ENABLED = "credential_service"
        const val PRIMARY = "credential_service_primary"
    }
}

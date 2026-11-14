package io.github.thisisthepy.dsa.agent.platform

import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.language.LanguageDefaults
import io.github.thisisthepy.dsa.agent.language.LanguageServicesSetup

/**
 * Makes the agent's text-to-speech engine, speech recognition service and spell checker the
 * guest's defaults at every boot, through the secure settings Android reads them from, unless
 * the user picked another one that is still installed ([LanguageDefaults]). The image has no
 * other engine, recognizer or spell checker, so Android would find only these in any case;
 * writing the settings keeps them chosen on a user's disk whatever an earlier image left there.
 *
 * It needs WRITE_SECURE_SETTINGS, which the agent holds as a privileged platform app.
 */
class AndroidLanguageServicesSetup(private val context: Context) : LanguageServicesSetup {
    override fun useHostServices() {
        val resolver = context.contentResolver
        val writes = LanguageDefaults.writes(
            ownPackage = context.packageName,
            recognizer = ComponentName(context, HostRecognitionService::class.java).flattenToString(),
            spellChecker = ComponentName(context, HostSpellCheckerService::class.java).flattenToString(),
            current = { key -> Settings.Secure.getString(resolver, key) },
            installed = { packageName ->
                try {
                    context.packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
                    true
                } catch (e: PackageManager.NameNotFoundException) {
                    false
                }
            },
        )
        for ((key, value) in writes) Settings.Secure.putString(resolver, key, value)
    }
}

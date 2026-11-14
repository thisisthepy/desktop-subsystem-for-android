package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.provider.Settings
import io.github.thisisthepy.dsa.agent.network.PrivateDnsPort

/**
 * Android's private DNS default through Settings.Global. `private_dns_default_mode` is a hidden
 * global setting, which the agent may write because it holds WRITE_SECURE_SETTINGS as a
 * privileged platform app.
 */
class AndroidPrivateDns(context: Context) : PrivateDnsPort {
    private val resolver = context.contentResolver

    override fun defaultMode(): String? = Settings.Global.getString(resolver, PRIVATE_DNS_DEFAULT_MODE)

    override fun setDefaultMode(mode: String) {
        Settings.Global.putString(resolver, PRIVATE_DNS_DEFAULT_MODE, mode)
    }

    private companion object {
        const val PRIVATE_DNS_DEFAULT_MODE = "private_dns_default_mode"
    }
}

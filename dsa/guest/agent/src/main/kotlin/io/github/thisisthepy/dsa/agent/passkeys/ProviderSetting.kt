package io.github.thisisthepy.dsa.agent.passkeys

/**
 * The credential manager's lists of enabled and primary credential providers
 * (Settings.Secure credential_service and credential_service_primary): flattened component
 * names joined with ':'. The agent keeps its provider in the enabled list, so another provider
 * that rewrites the lists (Google Play services, when the user installs it) does not turn the
 * host's passkeys off. It takes the primary place only when nothing holds it, so a provider the
 * user chose stays first.
 */
object ProviderSetting {
    /** [list] with [component] in it; null when it is in it already. */
    fun enabled(list: String?, component: String): String? {
        val entries = entries(list)
        if (component in entries) return null
        return (entries + component).joinToString(":")
    }

    /** [component] as the primary provider if [list] names none; null to leave it. */
    fun primary(list: String?, component: String): String? =
        if (entries(list).isEmpty()) component else null

    private fun entries(list: String?): List<String> =
        list.orEmpty().split(':').map { it.trim() }.filter { it.isNotEmpty() }
}

package io.github.thisisthepy.dsa.agent.network

import io.github.thisisthepy.dsa.agent.Log

/**
 * The guest's Wi-Fi, as far as joining the host's network needs it. The VM's network adapter
 * appears as a virt_wifi radio that sees one open network, [HOST_NETWORK]; its address,
 * route and DNS servers come by DHCP from the host.
 */
interface WifiPort {
    fun isEnabled(): Boolean

    fun enable()

    /** The SSIDs of the saved networks, without quotes. */
    fun savedNetworks(): List<String>

    /** Saves an open network, so the framework joins it now and whenever it is in range. */
    fun saveOpenNetwork(ssid: String): Boolean

    /** Asks the framework to join the saved network [ssid] now. */
    fun connect(ssid: String)

    companion object {
        val NONE = object : WifiPort {
            override fun isEnabled() = true
            override fun enable() {}
            override fun savedNetworks() = listOf(HOST_NETWORK)
            override fun saveOpenNetwork(ssid: String) = true
            override fun connect(ssid: String) {}
        }
    }
}

/** The SSID the virt_wifi radio reports for the host's network. */
const val HOST_NETWORK = "VirtWifi"

/**
 * Android's private DNS default: the mode its resolver uses while the user has chosen none
 * (`private_dns_default_mode`; the user's own choice, `private_dns_mode`, is never touched).
 */
interface PrivateDnsPort {
    /** The default mode, or null while Android's own default (opportunistic) applies. */
    fun defaultMode(): String?

    fun setDefaultMode(mode: String)

    companion object {
        val NONE = object : PrivateDnsPort {
            override fun defaultMode(): String? = PRIVATE_DNS_OFF
            override fun setDefaultMode(mode: String) {}
        }
    }
}

/** The private DNS mode that sends no query around the host's resolver. */
const val PRIVATE_DNS_OFF = "off"

/**
 * Turns Wi-Fi on and keeps the host's network saved, at every start of the agent, so apps
 * have a network from the first boot of a new disk and after a user removed the network in
 * Settings. There is no setup wizard and no SystemUI to do it, and Android leaves Wi-Fi off on
 * a new device. The framework then joins it, validates it and reports it to apps as an
 * unmetered Wi-Fi network; after a host network change or a resume from sleep it renews the
 * DHCP lease on the same network, which stays joined.
 *
 * The guest's DNS queries are answered by the host's resolver (the control daemon's DNS stub
 * hands them to the host), so the host's hosts file, VPN and DNS filters apply to apps. Android's
 * private DNS would by default try DNS-over-TLS to the network's servers and go around the host,
 * so its default is off; a user who picks a private DNS server in Settings still gets it.
 */
class NetworkSetup(
    private val wifi: WifiPort,
    private val log: Log,
    private val privateDns: PrivateDnsPort = PrivateDnsPort.NONE,
) {
    fun joinHostNetwork() {
        try {
            if (privateDns.defaultMode() != PRIVATE_DNS_OFF) {
                privateDns.setDefaultMode(PRIVATE_DNS_OFF)
                log.info("turned Android's private DNS default off, so the host's resolver answers every query")
            }
        } catch (e: Exception) {
            log.warn("could not turn Android's private DNS default off; DNS-over-TLS may go around the host's resolver", e)
        }
        if (!wifi.isEnabled()) {
            wifi.enable()
            log.info("turned Wi-Fi on, which carries the host's network")
        }
        if (HOST_NETWORK !in wifi.savedNetworks()) {
            if (wifi.saveOpenNetwork(HOST_NETWORK)) {
                log.info("saved the host's network")
            } else {
                log.warn("could not save the host's network; apps have no network until Wi-Fi joins it")
                return
            }
        }
        wifi.connect(HOST_NETWORK)
    }
}

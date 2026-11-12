package io.github.thisisthepy.dsa.agent.network

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope

/**
 * Tells every running process that Android's trusted certificates changed: the framework's
 * `KeyChain.ACTION_TRUST_STORE_CHANGED` broadcast, which the activity manager answers by having
 * each app's network security config and TrustManager forget the trust anchors they have looked
 * up.
 */
fun interface TrustStoreNotice {
    fun changed()

    companion object {
        val NONE = TrustStoreNotice { }
    }
}

/**
 * The root certificates a user or administrator added to the host. The control daemon adds
 * them to Android's system certificates, and takes away those the host no longer names, before
 * the agent hears of it (HostCertificates); an app that starts afterwards reads them as it reads
 * Android's own. A process already running has kept every anchor it looked up, so a root the
 * host took away would still be trusted there: the agent has every process forget them, and a
 * root added is found at the next look as well.
 *
 * Every method runs on the agent's scheduler thread.
 */
class HostCertificates(
    private val notice: TrustStoreNotice,
    private val log: Log,
) {
    /** Takes the host's HostCertificates. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.HOST_CERTIFICATES) return false
        val certificates = envelope.hostCertificates
        try {
            notice.changed()
            log.info(
                if (certificates.enabled) {
                    "Android trusts the host's ${certificates.certificatesCount} added root certificates"
                } else {
                    "Android trusts none of the host's root certificates"
                },
            )
        } catch (e: Exception) {
            log.warn("the running apps could not be told the trusted certificates changed", e)
        }
        return true
    }
}

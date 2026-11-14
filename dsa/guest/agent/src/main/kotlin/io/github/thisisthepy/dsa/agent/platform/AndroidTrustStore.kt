package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.Intent
import android.security.KeyChain
import io.github.thisisthepy.dsa.agent.network.TrustStoreNotice

/**
 * Sends `KeyChain.ACTION_TRUST_STORE_CHANGED`, the broadcast Android sends itself when a user
 * adds or removes a certificate. The activity manager answers it by having every running
 * process reset its network security config's certificate sources and its TrustManager's
 * anchors, so the next TLS handshake looks the system certificates up again. It is a protected
 * broadcast; the agent, a persistent app, may send it.
 */
class AndroidTrustStoreNotice(private val context: Context) : TrustStoreNotice {
    override fun changed() {
        context.sendBroadcast(Intent(KeyChain.ACTION_TRUST_STORE_CHANGED))
    }
}

package io.github.thisisthepy.dsa.agent.platform

import io.github.thisisthepy.dsa.agent.Log

/** The agent's messages, in the guest's log under the tag DsaAgent. */
object AndroidLog : Log {
    private const val TAG = "DsaAgent"

    override fun info(message: String) {
        android.util.Log.i(TAG, message)
    }

    override fun warn(message: String, error: Throwable?) {
        android.util.Log.w(TAG, message, error)
    }
}

package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.net.Uri
import io.github.thisisthepy.dsa.agent.install.GoogleDeviceIdPort

/**
 * The Google Services Framework's own record of the device's Android ID, from its gservices
 * provider: the row named `android_id`, whose value is the ID in decimal. The provider is
 * there only while Google Play is installed; without it the query returns null.
 */
class AndroidGoogleDeviceId(private val context: Context) : GoogleDeviceIdPort {
    override fun read(): String? {
        val cursor = context.contentResolver.query(GSERVICES, null, null, arrayOf(ANDROID_ID), null)
            ?: return null
        return cursor.use {
            if (it.moveToFirst() && it.columnCount >= 2) it.getString(1) else null
        }
    }

    private companion object {
        val GSERVICES: Uri = Uri.parse("content://com.google.android.gsf.gservices")
        const val ANDROID_ID = "android_id"
    }
}

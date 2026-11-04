package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.pm.PackageManager
import io.github.thisisthepy.dsa.agent.install.InstalledPackage
import io.github.thisisthepy.dsa.agent.install.PackageInfoPort
import java.security.MessageDigest

/**
 * The framework's package manager, asked what is installed of one package: its version and the
 * certificates it is signed with. For a package signed by one key the certificates are its
 * signing history (the current key and those it rotated from), since an update signed by any
 * of them is taken; for one signed by several keys at once, all of them.
 */
class AndroidPackageInfo(private val context: Context) : PackageInfoPort {
    override fun query(packageName: String): InstalledPackage? {
        val info = try {
            context.packageManager.getPackageInfo(packageName, PackageManager.GET_SIGNING_CERTIFICATES)
        } catch (e: PackageManager.NameNotFoundException) {
            return null
        }
        val signing = info.signingInfo
        val signatures = when {
            signing == null -> emptyArray()
            signing.hasMultipleSigners() -> signing.apkContentsSigners
            else -> signing.signingCertificateHistory
        } ?: emptyArray()
        val digest = MessageDigest.getInstance("SHA-256")
        return InstalledPackage(
            versionCode = info.longVersionCode,
            versionName = info.versionName ?: "",
            signerSha256 = signatures.map { digest.digest(it.toByteArray()) },
        )
    }
}

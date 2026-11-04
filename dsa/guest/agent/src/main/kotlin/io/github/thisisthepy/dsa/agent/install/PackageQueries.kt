package io.github.thisisthepy.dsa.agent.install

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.PackageState

/** What the package manager has of an installed package. */
data class InstalledPackage(
    /** PackageInfo.getLongVersionCode. */
    val versionCode: Long,
    val versionName: String,
    /** The SHA-256 digest of each certificate the package is signed with. */
    val signerSha256: List<ByteArray>,
)

/** The guest's package manager, as the agent asks it about one package. */
fun interface PackageInfoPort {
    /** What is installed of [packageName], or null when it is not installed. */
    fun query(packageName: String): InstalledPackage?

    companion object {
        val NONE = PackageInfoPort { null }
    }
}

/**
 * Answers the host's QueryPackage with what the package manager has of the package, so the
 * host's install window can tell a new install from an update, a downgrade or an APK signed
 * with another key before it sends the APK. A failure to ask is answered as not installed and
 * logged; Android still decides when the APK arrives.
 */
class PackageQueries(
    private val packages: PackageInfoPort,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
) {
    /** Takes a QueryPackage frame. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.QUERY_PACKAGE) return false
        val query = envelope.queryPackage
        val installed = try {
            packages.query(query.packageName)
        } catch (e: Exception) {
            log.warn("could not ask the package manager about ${query.packageName}", e)
            null
        }
        val state = PackageState.newBuilder().setRequestId(query.requestId)
        if (installed != null) {
            state.setInstalled(true)
                .setVersionCode(installed.versionCode)
                .setVersionName(installed.versionName)
            installed.signerSha256.forEach { state.addSignerSha256(ByteString.copyFrom(it)) }
        }
        send(Envelope.newBuilder().setPackageState(state).build().toByteArray())
        return true
    }
}

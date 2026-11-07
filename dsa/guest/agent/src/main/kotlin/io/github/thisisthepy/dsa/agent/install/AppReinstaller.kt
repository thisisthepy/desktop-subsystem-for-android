package io.github.thisisthepy.dsa.agent.install

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ReinstallApp
import io.github.thisisthepy.dsa.control.v1.ReinstallResult
import io.github.thisisthepy.dsa.control.v1.ReinstallStatus
import java.io.File

/** An installed app's APKs on the device, as installing it again needs them. */
class InstalledApks(
    val kind: AppKind,
    /** The base APK, then the splits. */
    val files: List<File>,
    /** The native libraries of [files] together: ABI to library names. */
    val libraries: Map<String, List<String>>,
)

/** The guest's package installer, as the agent uses it to install an app again with another ABI. */
interface PackageReinstallerPort {
    /** The APKs of [packageName] on the device; null when it is not installed. */
    fun apks(packageName: String): InstalledApks?

    /**
     * Installs [files] as [packageName] with its native code for [abi], replacing the installed
     * app, which keeps its data. [onResult] is called once, on any thread.
     */
    fun install(packageName: String, files: List<File>, abi: String, onResult: (InstallerResult) -> Unit)

    /** Uninstalls [packageName], data and all. [onResult] is called once, on any thread. */
    fun uninstall(packageName: String, onResult: (InstallerResult) -> Unit)

    /** Copies [files] where an uninstall leaves them, and returns the copies. */
    fun keepCopies(packageName: String, files: List<File>): List<File>

    /** Deletes copies [keepCopies] made. */
    fun dropCopies(files: List<File>)

    /** The ABI [packageName]'s native code runs as now; empty for none, or when it is not installed. */
    fun primaryAbi(packageName: String): String

    companion object {
        val NONE = object : PackageReinstallerPort {
            override fun apks(packageName: String): InstalledApks? = null
            override fun install(packageName: String, files: List<File>, abi: String, onResult: (InstallerResult) -> Unit) =
                throw IllegalStateException("this agent installs no apps again")
            override fun uninstall(packageName: String, onResult: (InstallerResult) -> Unit) =
                throw IllegalStateException("this agent installs no apps again")
            override fun keepCopies(packageName: String, files: List<File>): List<File> =
                throw IllegalStateException("this agent installs no apps again")
            override fun dropCopies(files: List<File>) = Unit
            override fun primaryAbi(packageName: String): String = ""
        }
    }
}

/**
 * Installs an app again from the APKs it has on the device with its native code for another
 * ABI, as the host's "Run as ARM" switch asks (ReinstallApp), and answers each request with how
 * it went (ReinstallResult of the same request ID).
 *
 * The app is replaced, as an update replaces it, so it keeps its data: Android keeps an app's
 * data across a replace install and takes the ABI from the session's override. When Android
 * refuses the replace in a way only an uninstall gets past, nothing changes and the host is told
 * the data would be erased; asked again with `erase_data`, the agent copies the APKs, uninstalls
 * the app and installs the copies. An app that is part of the Android image, the agent itself, a
 * package that is not installed, and an ABI the app's APKs hold no libraries for are refused at
 * once with their reason. A failure of the installer is answered, never thrown.
 *
 * Every method runs on the agent's scheduler thread; the installer's answers are moved there
 * with [post].
 */
class AppReinstaller(
    private val ownPackage: String,
    private val port: PackageReinstallerPort,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
    private val post: (() -> Unit) -> Unit,
    /** Called once an app is installed again, so the host's list of apps follows at once. */
    private val onReinstalled: () -> Unit = {},
) {
    /** The packages being installed again now; a second request for one waits for none. */
    private val busy = HashSet<String>()

    /** Takes a ReinstallApp frame. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        if (envelope.bodyCase != Envelope.BodyCase.REINSTALL_APP) return false
        val request = envelope.reinstallApp
        try {
            begin(request)
        } catch (e: Exception) {
            log.warn("could not install ${request.packageName} again", e)
            answer(request, ReinstallStatus.REINSTALL_STATUS_OTHER, "the package installer refused: ${e.message}")
        }
        return true
    }

    private fun begin(request: ReinstallApp) {
        val packageName = request.packageName
        val abi = request.abi
        if (packageName == ownPackage && packageName.isNotEmpty()) {
            return answer(request, ReinstallStatus.REINSTALL_STATUS_SYSTEM_APP, "the agent is part of the Android image")
        }
        if (abi != Abis.X86_64 && abi != Abis.ARM64) {
            return answer(request, ReinstallStatus.REINSTALL_STATUS_OTHER, "the ABI ${abi.take(32)} is neither ${Abis.X86_64} nor ${Abis.ARM64}")
        }
        val apks = if (packageName.isEmpty()) null else port.apks(packageName)
        when {
            apks == null ->
                return answer(request, ReinstallStatus.REINSTALL_STATUS_NOT_INSTALLED, "no app of this package is installed")
            apks.kind == AppKind.SYSTEM ->
                return answer(request, ReinstallStatus.REINSTALL_STATUS_SYSTEM_APP, "the app is part of the Android image")
            apks.libraries[abi].isNullOrEmpty() ->
                return answer(request, ReinstallStatus.REINSTALL_STATUS_NO_LIBRARIES, "the app's APKs on the device hold no $abi libraries")
            packageName in busy ->
                return answer(request, ReinstallStatus.REINSTALL_STATUS_OTHER, "the app is being installed again already")
        }
        val files = apks!!.files
        busy += packageName
        try {
            if (request.eraseData) {
                replaceAfterUninstall(request, files)
            } else {
                log.info("installing $packageName again for $abi, keeping its data")
                port.install(packageName, files, abi) { result -> post { replaced(request, result) } }
            }
        } catch (e: Exception) {
            busy -= packageName
            throw e
        }
    }

    private fun replaced(request: ReinstallApp, result: InstallerResult) {
        busy -= request.packageName
        when {
            result.status == PackageInstallerStatus.SUCCESS -> succeeded(request)
            needsUninstall(result) -> {
                log.warn("Android will not install ${request.packageName} again for ${request.abi} and keep its data: ${result.message}")
                answer(request, ReinstallStatus.REINSTALL_STATUS_DATA_WOULD_BE_ERASED, result.message.orEmpty())
            }
            else -> {
                log.warn("did not install ${request.packageName} again: ${result.message}")
                answer(request, ReinstallStatus.REINSTALL_STATUS_OTHER, result.message.orEmpty())
            }
        }
    }

    private fun replaceAfterUninstall(request: ReinstallApp, files: List<File>) {
        val packageName = request.packageName
        log.info("installing $packageName again for ${request.abi}; its data goes")
        val copies = port.keepCopies(packageName, files)
        val finish = { status: ReinstallStatus, message: String ->
            busy -= packageName
            port.dropCopies(copies)
            if (status == ReinstallStatus.REINSTALL_STATUS_SUCCESS) succeeded(request) else answer(request, status, message)
        }
        try {
            port.uninstall(packageName) { removed ->
                post {
                    if (removed.status != PackageInstallerStatus.SUCCESS) {
                        finish(ReinstallStatus.REINSTALL_STATUS_OTHER, "Android did not uninstall the app: ${removed.message.orEmpty()}")
                    } else {
                        try {
                            port.install(packageName, copies, request.abi) { installed ->
                                post {
                                    if (installed.status == PackageInstallerStatus.SUCCESS) {
                                        finish(ReinstallStatus.REINSTALL_STATUS_SUCCESS, "")
                                    } else {
                                        finish(ReinstallStatus.REINSTALL_STATUS_OTHER, "Android uninstalled the app but did not install it again: ${installed.message.orEmpty()}")
                                    }
                                }
                            }
                        } catch (e: Exception) {
                            log.warn("could not install $packageName again after uninstalling it", e)
                            finish(ReinstallStatus.REINSTALL_STATUS_OTHER, "Android uninstalled the app but did not install it again: ${e.message}")
                        }
                    }
                }
            }
        } catch (e: Exception) {
            port.dropCopies(copies)
            throw e
        }
    }

    /** The app is installed again: the host hears so once Android says which ABI it took. */
    private fun succeeded(request: ReinstallApp) {
        val now = port.primaryAbi(request.packageName)
        onReinstalled()
        if (now == request.abi) {
            log.info("installed ${request.packageName} again; it runs as $now")
            answer(request, ReinstallStatus.REINSTALL_STATUS_SUCCESS, "", now)
        } else {
            log.warn("installed ${request.packageName} again, but Android chose ${now.ifEmpty { "no ABI" }} over ${request.abi}")
            answer(request, ReinstallStatus.REINSTALL_STATUS_OTHER, "Android installed the app again but chose ${now.ifEmpty { "no ABI" }}", now)
        }
    }

    private fun answer(request: ReinstallApp, status: ReinstallStatus, message: String, primaryAbi: String = "") {
        val result = ReinstallResult.newBuilder()
            .setRequestId(request.requestId)
            .setPackageName(request.packageName)
            .setStatus(status)
            .setMessage(message)
            .setPrimaryAbi(primaryAbi)
        send(Envelope.newBuilder().setReinstallResult(result).build().toByteArray())
    }

    companion object {
        /** PackageManager.INSTALL_FAILED_UPDATE_INCOMPATIBLE: the installed app cannot be replaced by this one. */
        const val LEGACY_UPDATE_INCOMPATIBLE = -7

        /** PackageManager.INSTALL_FAILED_SHARED_USER_INCOMPATIBLE: the app shares a user whose ABI it must keep. */
        const val LEGACY_SHARED_USER_INCOMPATIBLE = -8

        /** Whether the installer refused a replace that an uninstall first would let through. */
        fun needsUninstall(result: InstallerResult): Boolean {
            if (result.status == PackageInstallerStatus.SUCCESS) return false
            val message = result.message.orEmpty()
            return result.legacyStatus == LEGACY_UPDATE_INCOMPATIBLE ||
                result.legacyStatus == LEGACY_SHARED_USER_INCOMPATIBLE ||
                "INSTALL_FAILED_UPDATE_INCOMPATIBLE" in message ||
                "INSTALL_FAILED_SHARED_USER_INCOMPATIBLE" in message
        }
    }
}

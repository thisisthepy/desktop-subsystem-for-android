package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import io.github.thisisthepy.dsa.agent.install.AppKind
import io.github.thisisthepy.dsa.agent.install.InstalledApks
import io.github.thisisthepy.dsa.agent.install.InstallerResult
import io.github.thisisthepy.dsa.agent.install.PackageReinstallerPort
import java.io.File

/**
 * Installs apps again from their APKs on the device through the framework's PackageInstaller,
 * with the session's ABI override, and uninstalls them when the user lets the data go. The
 * copies an uninstall needs go to the agent's cache, which only the agent can read.
 */
class AndroidPackageReinstaller(
    private val context: Context,
    private val installer: AndroidPackageInstaller,
    private val uninstaller: AndroidPackageUninstaller,
    private val apks: AndroidApks,
) : PackageReinstallerPort {
    private fun info(packageName: String): ApplicationInfo? = try {
        context.packageManager.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))
    } catch (e: PackageManager.NameNotFoundException) {
        null
    }

    override fun apks(packageName: String): InstalledApks? {
        val info = info(packageName) ?: return null
        val system = info.flags and (ApplicationInfo.FLAG_SYSTEM or ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) != 0
        return InstalledApks(
            kind = if (system) AppKind.SYSTEM else AppKind.USER,
            files = apks.files(info),
            libraries = apks.libraries(info),
        )
    }

    override fun install(packageName: String, files: List<File>, abi: String, onResult: (InstallerResult) -> Unit) =
        installer.installFiles(packageName, files, abi, onResult)

    override fun uninstall(packageName: String, onResult: (InstallerResult) -> Unit) =
        uninstaller.uninstall(packageName, onResult)

    override fun keepCopies(packageName: String, files: List<File>): List<File> {
        val dir = File(context.cacheDir, "reinstall/$packageName")
        dir.deleteRecursively()
        if (!dir.mkdirs()) throw java.io.IOException("cannot make $dir")
        return files.map { file -> File(dir, file.name).also { file.copyTo(it) } }
    }

    override fun dropCopies(files: List<File>) {
        files.map { it.parentFile }.distinct().forEach { it?.deleteRecursively() }
    }

    override fun primaryAbi(packageName: String): String = info(packageName)?.let { AndroidApks.primaryAbi(it) }.orEmpty()
}

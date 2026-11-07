package io.github.thisisthepy.dsa.agent.platform

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.install.ExpansionFiles
import io.github.thisisthepy.dsa.agent.install.InstallFileNames
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.OutputStream

/**
 * The expansion files of the apps the host installs: kept in [staging] (the agent's own cache)
 * while they arrive, then copied into the app's folder under Android/obb/ of the shared
 * storage at [storage] and dropped from the cache.
 *
 * The folders under Android/obb/ belong to their apps; an installer may write them while it
 * holds INSTALL_PACKAGES and the storage permissions, which the image grants the agent. A file
 * is written beside its final name first, so the app never sees half of one.
 */
class AndroidExpansionFiles(
    private val staging: File,
    private val storage: File,
    private val log: Log,
) : ExpansionFiles {
    override fun stage(name: String): OutputStream {
        if (!staging.isDirectory && !staging.mkdirs()) throw IOException("cannot make $staging")
        return FileOutputStream(File(staging, name))
    }

    override fun place(packageName: String, names: List<String>) {
        val folder = InstallFileNames.obbFolder(storage, packageName)
        if (!folder.isDirectory && !folder.mkdirs()) throw IOException("cannot make $folder")
        for (name in names) {
            val staged = File(staging, name)
            val partial = File(folder, "$name.part")
            staged.inputStream().use { input ->
                FileOutputStream(partial).use { output ->
                    input.copyTo(output)
                    output.fd.sync()
                }
            }
            val target = File(folder, name)
            if (target.exists() && !target.delete()) throw IOException("cannot replace $target")
            if (!partial.renameTo(target)) throw IOException("cannot name $partial $name")
            staged.delete()
            log.info("put $name (${target.length()} bytes) in $folder")
        }
    }

    override fun discard(names: List<String>) {
        for (name in names) File(staging, name).delete()
    }
}

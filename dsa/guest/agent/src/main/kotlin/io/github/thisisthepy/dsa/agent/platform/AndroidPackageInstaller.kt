package io.github.thisisthepy.dsa.agent.platform

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.install.InstallSession
import io.github.thisisthepy.dsa.agent.install.InstallerResult
import io.github.thisisthepy.dsa.agent.install.PackageInstallerPort
import java.io.OutputStream
import java.util.concurrent.atomic.AtomicInteger

/**
 * Install sessions of the framework's PackageInstaller.
 *
 * The agent is a privileged platform app holding INSTALL_PACKAGES, so a session commits without
 * asking the user. Should the installer still ask (STATUS_PENDING_USER_ACTION), the agent shows
 * its confirmation, which opens as a task window on the host, and waits for the final answer.
 * Each commit gets a broadcast action of its own, delivered only to this process, which carries
 * the installer's answer.
 */
class AndroidPackageInstaller(private val context: Context, private val log: Log) : PackageInstallerPort {
    private val installer = context.packageManager.packageInstaller

    override fun open(name: String, size: Long, abi: String): InstallSession {
        val session = Session(create(size, abi), name)
        try {
            session.startApk(SPLIT_NAME, size)
        } catch (e: Exception) {
            session.abandon()
            throw e
        }
        return session
    }

    /** A session of several APKs: the base and the splits of one app, each begun by name. */
    override fun openApks(name: String, size: Long, abi: String): InstallSession = Session(create(size, abi), name)

    private fun create(size: Long, abi: String): PackageInstaller.Session {
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setSize(size)
            setInstallReason(android.content.pm.PackageManager.INSTALL_REASON_USER)
            setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
        if (abi.isNotEmpty()) setAbiOverride(params, abi)
        val id = installer.createSession(params)
        return installer.openSession(id)
    }

    private inner class Session(
        private val session: PackageInstaller.Session,
        private val name: String,
    ) : InstallSession {
        /** The APK being written, while one is. */
        private var stream: OutputStream? = null

        override fun startApk(name: String, size: Long) {
            finishApk()
            stream = session.openWrite(name, 0, size)
        }

        /** Makes the APK written so far whole on disk, and closes it. */
        private fun finishApk() {
            val written = stream ?: return
            stream = null
            session.fsync(written)
            written.close()
        }

        override fun write(data: ByteArray) {
            val written = stream ?: throw IllegalStateException("no APK of the install session is begun")
            written.write(data)
        }

        override fun commit(onResult: (InstallerResult) -> Unit) {
            finishApk()
            val action = "${context.packageName}.INSTALL_RESULT.${NEXT.incrementAndGet()}"
            val receiver = object : BroadcastReceiver() {
                override fun onReceive(context: Context, intent: Intent) {
                    val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                    if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                        confirm(intent)
                        return
                    }
                    context.unregisterReceiver(this)
                    onResult(
                        InstallerResult(
                            status = status,
                            message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE),
                            packageName = intent.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME),
                            legacyStatus = intent.getIntExtra(EXTRA_LEGACY_STATUS, 0),
                        ),
                    )
                }
            }
            context.registerReceiver(receiver, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
            val callback = PendingIntent.getBroadcast(
                context,
                0,
                Intent(action).setPackage(context.packageName),
                // The installer fills the status in, so the intent must be mutable.
                PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            try {
                session.commit(callback.intentSender)
            } catch (e: Exception) {
                context.unregisterReceiver(receiver)
                throw e
            } finally {
                session.close()
            }
        }

        private fun confirm(intent: Intent) {
            val confirmation = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
            if (confirmation == null) {
                log.warn("Android asked to confirm the install of $name but gave nothing to show")
                return
            }
            log.info("Android asks the user to confirm the install of $name")
            context.startActivity(confirmation.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        }

        override fun abandon() {
            stream?.let {
                try {
                    it.close()
                } catch (e: Exception) {
                    log.warn("could not close the install session's stream", e)
                }
            }
            stream = null
            session.abandon()
        }
    }

    /**
     * Installs [files] (an app's base APK and its splits, by their file names) in one session,
     * as a replace of [packageName] when it is installed, with its native code for [abi].
     * [onResult] is called once, on any thread, when the installer has decided.
     */
    fun installFiles(packageName: String, files: List<java.io.File>, abi: String, onResult: (InstallerResult) -> Unit) {
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(packageName)
            setSize(files.sumOf { it.length() })
            setInstallReason(android.content.pm.PackageManager.INSTALL_REASON_USER)
            setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
        setAbiOverride(params, abi)
        val id = installer.createSession(params)
        val session = installer.openSession(id)
        try {
            for (file in files) {
                session.openWrite(file.name, 0, file.length()).use { out ->
                    file.inputStream().use { it.copyTo(out) }
                    session.fsync(out)
                }
            }
        } catch (e: Exception) {
            session.abandon()
            session.close()
            throw e
        }
        commit(session, packageName, onResult)
    }

    private fun commit(session: PackageInstaller.Session, name: String, onResult: (InstallerResult) -> Unit) {
        val action = "${context.packageName}.INSTALL_RESULT.${NEXT.incrementAndGet()}"
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                    val confirmation = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    if (confirmation == null) {
                        log.warn("Android asked to confirm the install of $name but gave nothing to show")
                    } else {
                        log.info("Android asks the user to confirm the install of $name")
                        context.startActivity(confirmation.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                    }
                    return
                }
                context.unregisterReceiver(this)
                onResult(
                    InstallerResult(
                        status = status,
                        message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE),
                        packageName = intent.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME),
                        legacyStatus = intent.getIntExtra(EXTRA_LEGACY_STATUS, 0),
                    ),
                )
            }
        }
        context.registerReceiver(receiver, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
        val callback = PendingIntent.getBroadcast(
            context,
            0,
            Intent(action).setPackage(context.packageName),
            // The installer fills the status in, so the intent must be mutable.
            PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        try {
            session.commit(callback.intentSender)
        } catch (e: Exception) {
            context.unregisterReceiver(receiver)
            throw e
        } finally {
            session.close()
        }
    }

    /**
     * Sets the session's ABI override, which `pm install --abi` sets too. The SDK hides the
     * field (SessionParams.abiOverride, a public field on Android 16 and 17); the agent is
     * signed with the platform key, so the framework lets it reach hidden fields. Without the
     * field the install would go ahead with the ABI Android chooses, so it is refused instead.
     */
    private fun setAbiOverride(params: PackageInstaller.SessionParams, abi: String) {
        val field = try {
            PackageInstaller.SessionParams::class.java.getField("abiOverride")
        } catch (e: NoSuchFieldException) {
            throw IllegalStateException("this Android's package installer takes no ABI override, so the app cannot be installed for $abi", e)
        }
        field.set(params, abi)
    }

    private companion object {
        /** The name of the one APK in a session of one. */
        const val SPLIT_NAME = "base.apk"

        /** PackageInstaller.EXTRA_LEGACY_STATUS, which the SDK hides: the INSTALL_FAILED_ code. */
        const val EXTRA_LEGACY_STATUS = "android.content.pm.extra.LEGACY_STATUS"

        val NEXT = AtomicInteger()
    }
}

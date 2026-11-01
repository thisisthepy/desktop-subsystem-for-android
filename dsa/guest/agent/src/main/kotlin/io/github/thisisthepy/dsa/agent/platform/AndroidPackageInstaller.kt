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

    override fun open(name: String, size: Long): InstallSession {
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setSize(size)
            setInstallReason(android.content.pm.PackageManager.INSTALL_REASON_USER)
            setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
        val id = installer.createSession(params)
        val session = installer.openSession(id)
        val stream = try {
            session.openWrite(SPLIT_NAME, 0, size)
        } catch (e: Exception) {
            session.abandon()
            throw e
        }
        return Session(session, stream, name)
    }

    private inner class Session(
        private val session: PackageInstaller.Session,
        private val stream: OutputStream,
        private val name: String,
    ) : InstallSession {
        private var closed = false

        override fun write(data: ByteArray) {
            stream.write(data)
        }

        override fun commit(onResult: (InstallerResult) -> Unit) {
            session.fsync(stream)
            stream.close()
            closed = true
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
            if (!closed) {
                try {
                    stream.close()
                } catch (e: Exception) {
                    log.warn("could not close the install session's stream", e)
                }
            }
            session.abandon()
        }
    }

    private companion object {
        /** The name of the one APK in each session. */
        const val SPLIT_NAME = "base.apk"

        /** PackageInstaller.EXTRA_LEGACY_STATUS, which the SDK hides: the INSTALL_FAILED_ code. */
        const val EXTRA_LEGACY_STATUS = "android.content.pm.extra.LEGACY_STATUS"

        val NEXT = AtomicInteger()
    }
}

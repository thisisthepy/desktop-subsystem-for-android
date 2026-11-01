package io.github.thisisthepy.dsa.agent.platform

import android.app.role.RoleManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Process
import android.os.UserHandle
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.links.BrowserRole
import io.github.thisisthepy.dsa.agent.links.GuestBrowser
import java.util.concurrent.Executor
import java.util.function.Consumer

/** The browser activities in the guest other than the agent's own, best first. */
private fun otherBrowsers(context: Context): List<ComponentName> {
    val probe = Intent(Intent.ACTION_VIEW, Uri.parse("https://example.com/"))
        .addCategory(Intent.CATEGORY_BROWSABLE)
    return context.packageManager
        .queryIntentActivities(probe, PackageManager.ResolveInfoFlags.of(PackageManager.MATCH_ALL.toLong()))
        .mapNotNull { it.activityInfo }
        .filter { it.packageName != context.packageName }
        .map { ComponentName(it.packageName, it.name) }
}

/**
 * Opens a link in a browser app in the guest, named outright, so the agent, which may hold the
 * browser role, never gets the link back.
 */
class AndroidGuestBrowser(private val context: Context, private val log: Log) : GuestBrowser {
    override fun open(url: String) {
        val browser = otherBrowsers(context).firstOrNull()
        if (browser == null) {
            log.warn("no browser app in the guest can open a link")
            return
        }
        context.startActivity(
            Intent(Intent.ACTION_VIEW, Uri.parse(url))
                .addCategory(Intent.CATEGORY_BROWSABLE)
                .setComponent(browser)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
}

/**
 * The guest's browser role, through the role manager. Its calls for other apps are system
 * APIs, not in the SDK, so they are made by reflection; MANAGE_ROLE_HOLDERS lets the agent make
 * them. The role is exclusive: making one app the browser takes the role from the last.
 */
class AndroidBrowserRole(private val context: Context, private val log: Log) : BrowserRole {
    private val roles: RoleManager = context.getSystemService(RoleManager::class.java)

    override fun take() {
        holdBy(context.packageName)
    }

    override fun giveBack() {
        val other = otherBrowsers(context).firstOrNull()?.packageName
        if (other != null) {
            holdBy(other)
        } else {
            log.info("no other browser app in the guest takes the browser role back")
            val clear = RoleManager::class.java.getMethod(
                "clearRoleHoldersAsUser",
                String::class.java,
                Int::class.javaPrimitiveType,
                UserHandle::class.java,
                Executor::class.java,
                Consumer::class.java,
            )
            clear.invoke(roles, RoleManager.ROLE_BROWSER, 0, Process.myUserHandle(), context.mainExecutor, done("no app"))
        }
    }

    private fun holdBy(packageName: String) {
        @Suppress("UNCHECKED_CAST")
        val holders = RoleManager::class.java.getMethod("getRoleHolders", String::class.java)
            .invoke(roles, RoleManager.ROLE_BROWSER) as List<String>
        if (holders == listOf(packageName)) return
        val add = RoleManager::class.java.getMethod(
            "addRoleHolderAsUser",
            String::class.java,
            String::class.java,
            Int::class.javaPrimitiveType,
            UserHandle::class.java,
            Executor::class.java,
            Consumer::class.java,
        )
        add.invoke(roles, RoleManager.ROLE_BROWSER, packageName, 0, Process.myUserHandle(), context.mainExecutor, done(packageName))
    }

    private fun done(holder: String) = Consumer<Boolean> { ok ->
        if (ok) {
            log.info("the guest's browser is now $holder")
        } else {
            log.warn("the role manager did not make $holder the guest's browser")
        }
    }
}

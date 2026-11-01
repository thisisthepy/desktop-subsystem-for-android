package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The guest has no SystemUI, so the agent's notification listener is what reads the guest's
 * notifications: one the notification manager binds, and that the agent may allow itself.
 */
class NotificationListenerManifestTest {
    private val android = "http://schemas.android.com/apk/res/android"

    private fun parse(path: String): Element = DocumentBuilderFactory.newInstance()
        .apply { isNamespaceAware = true }
        .newDocumentBuilder()
        .parse(File(path))
        .documentElement

    private val manifest = parse("src/main/AndroidManifest.xml")

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    @Test
    fun fr8_the_agent_declares_a_notification_listener_only_the_notification_manager_binds() {
        val service = manifest.children("service").singleOrNull { service ->
            service.children("action").any {
                it.attr("name") == "android.service.notification.NotificationListenerService"
            }
        }
        assertNotNull("the agent declares no notification listener", service)
        assertEquals(".platform.HostNotificationListener", service!!.attr("name"))
        assertEquals("android.permission.BIND_NOTIFICATION_LISTENER_SERVICE", service.attr("permission"))
        assertEquals("true", service.attr("exported"))
        // In the agent's process, which is persistent and so never frozen while it dozes.
        assertEquals("", service.attr("process"))
    }

    @Test
    fun fr8_the_agent_may_allow_its_own_listener_without_a_person() {
        val requested = manifest.children("uses-permission").map { it.attr("name") }
        assertTrue("android.permission.MANAGE_NOTIFICATION_LISTENERS" in requested)
        val listed = parse("privapp-permissions-dsa-agent.xml").children("permission").map { it.getAttribute("name") }
        assertTrue("android.permission.MANAGE_NOTIFICATION_LISTENERS" in listed)
    }
}

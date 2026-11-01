package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent's accessibility service reads the nodes of watched tasks: one only the system can
 * bind, in the agent's process, that reads window content and changes no input.
 */
class AccessibilityManifestTest {
    private val android = "http://schemas.android.com/apk/res/android"

    private fun parse(path: String): Element = DocumentBuilderFactory.newInstance()
        .apply { isNamespaceAware = true }
        .newDocumentBuilder()
        .parse(File(path))
        .documentElement

    private val application = parse("src/main/AndroidManifest.xml").children("application").single()

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    @Test
    fun fr18_the_agent_declares_an_accessibility_service_only_the_system_binds() {
        val service = application.children("service").singleOrNull { service ->
            service.children("action").any { it.attr("name") == "android.accessibilityservice.AccessibilityService" }
        }
        assertNotNull("the agent declares no accessibility service", service)
        assertEquals(".platform.HostAccessibilityService", service!!.attr("name"))
        assertEquals("android.permission.BIND_ACCESSIBILITY_SERVICE", service.attr("permission"))
        assertEquals("", service.attr("process"))
        val metadata = service.children("meta-data").single()
        assertEquals("@xml/host_accessibility_service", metadata.attr("resource"))
    }

    @Test
    fun fr18_the_service_reads_window_content_and_changes_no_input() {
        val config = parse("src/main/res/xml/host_accessibility_service.xml")
        assertEquals("accessibility-service", config.tagName)
        assertEquals("true", config.attr("canRetrieveWindowContent"))
        val flags = config.attr("accessibilityFlags")
        assertTrue(flags.contains("flagRetrieveInteractiveWindows"))
        for (input in listOf("flagRequestTouchExplorationMode", "flagRequestFilterKeyEvents", "flagRequestAccessibilityButton")) {
            assertFalse("$input would change the user's input", flags.contains(input))
        }
        assertFalse(config.hasAttributeNS(android, "canPerformGestures"))
        assertFalse(config.hasAttributeNS(android, "canRequestTouchExplorationMode"))
    }
}

package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import org.w3c.dom.Element

/** Apps print through the agent's print service, which only the print spooler binds. */
class PrintServiceManifestTest {
    private val android = "http://schemas.android.com/apk/res/android"

    private val manifest: Element = DocumentBuilderFactory.newInstance()
        .apply { isNamespaceAware = true }
        .newDocumentBuilder()
        .parse(File("src/main/AndroidManifest.xml"))
        .documentElement

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    @Test
    fun fr26_the_agent_declares_a_print_service_only_the_spooler_binds() {
        val service = manifest.children("service").singleOrNull { service ->
            service.children("action").any { it.attr("name") == "android.printservice.PrintService" }
        }
        assertNotNull("the agent declares no print service", service)
        assertEquals(".platform.HostPrintService", service!!.attr("name"))
        assertEquals("android.permission.BIND_PRINT_SERVICE", service.attr("permission"))
        assertEquals("true", service.attr("exported"))
        assertEquals("", service.attr("process"))
    }
}

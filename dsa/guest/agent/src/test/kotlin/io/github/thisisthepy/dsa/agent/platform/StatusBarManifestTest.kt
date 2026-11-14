package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent registers as the status bar, so the window manager reports the system bars' live
 * appearance to it, which takes STATUS_BAR_SERVICE. The agent's manifest asks for it, and its
 * privileged permission list names it with the rest.
 */
class StatusBarManifestTest {
    private fun names(path: String, tag: String): Set<String> {
        val document = DocumentBuilderFactory.newInstance()
            .apply { isNamespaceAware = true }
            .newDocumentBuilder()
            .parse(File(path))
        val nodes = document.getElementsByTagName(tag)
        return (0 until nodes.length).map { (nodes.item(it) as Element) }
            .map { element ->
                element.getAttributeNS("http://schemas.android.com/apk/res/android", "name")
                    .ifEmpty { element.getAttribute("name") }
            }
            .toSet()
    }

    @Test
    fun fr22_the_agent_may_be_the_status_bar() {
        val permission = "android.permission.STATUS_BAR_SERVICE"
        assertTrue(permission in names("src/main/AndroidManifest.xml", "uses-permission"))
        assertTrue(permission in names("privapp-permissions-dsa-agent.xml", "permission"))
    }
}

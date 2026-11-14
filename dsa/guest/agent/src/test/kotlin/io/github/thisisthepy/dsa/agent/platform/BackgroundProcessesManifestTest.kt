package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent ends other apps' processes that stay cached with no window through
 * killBackgroundProcesses, which takes KILL_BACKGROUND_PROCESSES and, for another app's
 * package, KILL_ALL_BACKGROUND_PROCESSES; it reads the activity manager's process records
 * through its dump, which takes DUMP. The manifest asks for all three and the privileged
 * permission list names them.
 */
class BackgroundProcessesManifestTest {
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
    fun nfr2_the_agent_may_end_other_apps_background_processes() {
        for (permission in listOf(
            "android.permission.KILL_BACKGROUND_PROCESSES",
            "android.permission.KILL_ALL_BACKGROUND_PROCESSES",
            "android.permission.DUMP",
        )) {
            assertTrue(permission, permission in names("src/main/AndroidManifest.xml", "uses-permission"))
            assertTrue(permission, permission in names("privapp-permissions-dsa-agent.xml", "permission"))
        }
    }
}

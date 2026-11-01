package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent installs the host's APKs without asking the user to confirm each one, which takes
 * INSTALL_PACKAGES. It is a privileged permission, so the framework grants it only when the
 * agent's privileged permission list names it too; without it the framework refuses to boot.
 */
class InstallManifestTest {
    private fun names(path: String, tag: String, attribute: String): Set<String> {
        val document = DocumentBuilderFactory.newInstance()
            .apply { isNamespaceAware = true }
            .newDocumentBuilder()
            .parse(File(path))
        val nodes = document.getElementsByTagName(tag)
        return (0 until nodes.length).map { (nodes.item(it) as Element) }
            .map { element ->
                element.getAttributeNS("http://schemas.android.com/apk/res/android", attribute)
                    .ifEmpty { element.getAttribute(attribute) }
            }
            .toSet()
    }

    @Test
    fun fr11_the_agent_may_install_packages_in_its_manifest_and_its_privileged_list() {
        val permission = "android.permission.INSTALL_PACKAGES"
        assertTrue(permission in names("src/main/AndroidManifest.xml", "uses-permission", "name"))
        assertTrue(permission in names("privapp-permissions-dsa-agent.xml", "permission", "name"))
    }
}

package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent routes each app with a window to an output of its own through an audio policy,
 * which takes MODIFY_AUDIO_ROUTING, and sets Android's media volume to the one the user set on
 * the host, which takes MODIFY_AUDIO_SETTINGS. The manifest asks for both, and the privileged
 * permission list names the privileged one.
 */
class AudioRoutingManifestTest {
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
    fun fr26_the_agent_may_route_apps_and_set_the_media_volume() {
        val manifest = names("src/main/AndroidManifest.xml", "uses-permission")
        assertTrue("android.permission.MODIFY_AUDIO_ROUTING" in manifest)
        assertTrue("android.permission.MODIFY_AUDIO_SETTINGS" in manifest)
        assertTrue(
            "android.permission.MODIFY_AUDIO_ROUTING" in
                names("privapp-permissions-dsa-agent.xml", "permission"),
        )
    }
}

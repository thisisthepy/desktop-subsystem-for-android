package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test
import org.w3c.dom.Element

/**
 * Android hands an app's requestPinShortcut to the default launcher's activity for
 * CONFIRM_PIN_SHORTCUT, and says pinning is supported only while that activity exists. The
 * agent is the default launcher, so it declares one, which shows nothing and never becomes a
 * window on the host.
 */
class PinShortcutManifestTest {
    private val android = "http://schemas.android.com/apk/res/android"
    private val manifest = DocumentBuilderFactory.newInstance()
        .apply { isNamespaceAware = true }
        .newDocumentBuilder()
        .parse(File("src/main/AndroidManifest.xml"))
        .documentElement

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    private val activity = manifest.children("activity").single { it.attr("name") == ".platform.PinShortcutActivity" }

    @Test
    fun fr7_the_agent_takes_the_apps_pin_requests_as_the_default_launcher() {
        val actions = activity.children("intent-filter").flatMap { it.children("action") }.map { it.attr("name") }
        assertEquals(listOf("android.content.pm.action.CONFIRM_PIN_SHORTCUT"), actions)
        // Only the system starts it.
        assertEquals("false", activity.attr("exported"))
    }

    @Test
    fun fr7_the_pin_activity_shows_nothing_and_stays_out_of_the_apps_tasks() {
        assertEquals("@android:style/Theme.NoDisplay", activity.attr("theme"))
        assertEquals("true", activity.attr("excludeFromRecents"))
        assertEquals("true", activity.attr("noHistory"))
        // A task of its own, never the app's that asked, and in the agent's package, which the
        // host shows no window for.
        assertNotEquals("", activity.attr("taskAffinity"))
        assertEquals("", activity.attr("process"))
    }
}

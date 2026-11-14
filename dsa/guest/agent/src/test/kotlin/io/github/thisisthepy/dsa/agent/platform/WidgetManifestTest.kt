package io.github.thisisthepy.dsa.agent.platform

import android.content.Intent
import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent places the widgets the user picks on the host without asking them to allow each
 * one, which takes BIND_APPWIDGET. It is a privileged permission, so the framework grants it
 * only when the agent's privileged permission list names it too. The activity that runs a
 * widget's setup is the agent's own and nobody else may start it.
 */
class WidgetManifestTest {
    private val android = "http://schemas.android.com/apk/res/android"

    private fun elements(path: String, tag: String): List<Element> {
        val document = DocumentBuilderFactory.newInstance()
            .apply { isNamespaceAware = true }
            .newDocumentBuilder()
            .parse(File(path))
        val nodes = document.getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attribute(name: String) = getAttributeNS(android, name).ifEmpty { getAttribute(name) }

    @Test
    fun fr12_the_agent_may_bind_widgets_in_its_manifest_and_its_privileged_list() {
        val permission = "android.permission.BIND_APPWIDGET"
        assertTrue(permission in elements("src/main/AndroidManifest.xml", "uses-permission").map { it.attribute("name") })
        assertTrue(permission in elements("privapp-permissions-dsa-agent.xml", "permission").map { it.attribute("name") })
    }

    @Test
    fun fr12_the_widget_setup_activity_is_declared_and_not_exported() {
        val activity = elements("src/main/AndroidManifest.xml", "activity")
            .single { it.attribute("name") == ".platform.WidgetConfigureActivity" }
        assertEquals("false", activity.attribute("exported"))
        assertEquals("true", activity.attribute("excludeFromRecents"))
    }

    @Test
    fun fr12_a_widget_setup_opens_in_a_resizable_task_of_its_own_not_the_agents_home_task() {
        val manifest = elements("src/main/AndroidManifest.xml", "manifest").single()
        val activity = elements("src/main/AndroidManifest.xml", "activity")
            .single { it.attribute("name") == ".platform.WidgetConfigureActivity" }
        val affinity = activity.attribute("taskAffinity")
        // With the agent's own affinity (the default), it joined the home task, which the host
        // shows no window for, and the user never saw the setup.
        assertTrue("the setup activity needs an affinity of its own", affinity.isNotEmpty())
        assertTrue(affinity != manifest.getAttribute("package"))
        assertTrue(affinity != "io.github.thisisthepy.dsa.agent")
        assertTrue(activity.attribute("resizeableActivity") != "false")
        // Each setup a new task, even beside another setup.
        val flags = WidgetConfigureActivity.LAUNCH_FLAGS
        assertTrue(flags and Intent.FLAG_ACTIVITY_NEW_TASK != 0)
        assertTrue(flags and Intent.FLAG_ACTIVITY_MULTIPLE_TASK != 0)
    }
}

package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * Android puts text into an editor only through an input method, and the guest has no
 * keyboard app, so the agent declares the guest's input method: one the input method manager
 * finds, binds, and selects on its own, and that no other app can bind.
 */
class InputMethodManifestTest {
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

    private fun inputMethods(): List<Element> = application.children("service").filter { service ->
        service.children("action").any { it.attr("name") == "android.view.InputMethod" }
    }

    @Test
    fun fr6_the_agent_declares_the_guests_one_input_method() {
        val service = inputMethods().singleOrNull()
        assertNotNull("the agent declares no input method, so no editor in the guest can take text", service)
        assertEquals(".platform.HostInputMethod", service!!.attr("name"))
        // Only the input method manager may bind it.
        assertEquals("android.permission.BIND_INPUT_METHOD", service.attr("permission"))
        assertEquals("true", service.attr("exported"))
        val metadata = service.children("meta-data").single()
        assertEquals("android.view.im", metadata.attr("name"))
        assertEquals("@xml/host_input_method", metadata.attr("resource"))
    }

    @Test
    fun fr6_the_input_method_is_selected_by_default_and_offers_no_other() {
        val method = parse("src/main/res/xml/host_input_method.xml")
        assertEquals("input-method", method.tagName)
        assertEquals("@bool/host_input_method_is_default", method.attr("isDefault"))
        assertEquals("false", method.attr("supportsSwitchingToNextInputMethod"))
        val isDefault = parse("src/main/res/values/bools.xml").children("bool")
            .single { it.getAttribute("name") == "host_input_method_is_default" }
        assertEquals("true", isDefault.textContent.trim())
    }

    @Test
    fun fr6_the_input_method_runs_in_the_agents_process_before_the_user_unlocks() {
        val service = inputMethods().single()
        // The agent reaches it directly, in its own process.
        assertEquals("", service.attr("process"))
        // Before the first unlock the input method manager binds only direct boot aware ones.
        val directBootAware = service.attr("directBootAware").ifEmpty { application.attr("directBootAware") }
        assertTrue(directBootAware == "true")
    }
}

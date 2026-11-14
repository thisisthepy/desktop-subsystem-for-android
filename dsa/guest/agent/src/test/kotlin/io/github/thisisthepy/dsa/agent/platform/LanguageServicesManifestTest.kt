package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/** The agent declares the guest's text-to-speech engine, speech recognition service and spell checker. */
class LanguageServicesManifestTest {
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

    private fun component(tag: String, action: String): Element {
        val found = manifest.children(tag).singleOrNull { element ->
            element.children("action").any { it.attr("name") == action }
        }
        assertNotNull("the agent declares no $tag for $action", found)
        return found!!
    }

    private fun metaResource(element: Element, name: String): String =
        element.children("meta-data").single { it.attr("name") == name }.attr("resource")

    @Test
    fun fr36_the_agent_declares_a_tts_engine_with_its_voice_data_check() {
        val engine = component("service", "android.intent.action.TTS_SERVICE")
        assertEquals(".platform.HostTextToSpeechService", engine.attr("name"))
        assertEquals("true", engine.attr("exported"))
        assertEquals("@xml/host_tts_engine", metaResource(engine, "android.speech.tts"))
        val check = component("activity", "android.speech.tts.engine.CHECK_TTS_DATA")
        assertEquals(".platform.HostTtsDataActivity", check.attr("name"))
    }

    @Test
    fun fr36_the_agent_declares_a_recognition_service_and_may_record() {
        val service = component("service", "android.speech.RecognitionService")
        assertEquals(".platform.HostRecognitionService", service.attr("name"))
        assertEquals("@xml/host_recognition_service", metaResource(service, "android.speech"))
        assertTrue(
            manifest.children("uses-permission").any { it.attr("name") == "android.permission.RECORD_AUDIO" },
        )
        val granted = parse("default-permissions-dsa-agent.xml").children("permission")
            .map { it.getAttribute("name") }
        assertTrue("RECORD_AUDIO is granted by default", "android.permission.RECORD_AUDIO" in granted)
    }

    @Test
    fun fr36_the_agent_declares_a_spell_checker_only_the_text_services_bind_with_subtypes() {
        val service = component("service", "android.service.textservice.SpellCheckerService")
        assertEquals(".platform.HostSpellCheckerService", service.attr("name"))
        assertEquals("android.permission.BIND_TEXT_SERVICE", service.attr("permission"))
        assertEquals("@xml/host_spell_checker", metaResource(service, "android.view.textservice.scs"))
        val subtypes = parse("src/main/res/xml/host_spell_checker.xml").children("subtype")
        val tags = subtypes.map { it.attr("languageTag") }
        assertTrue("en-US" in tags && "ko-KR" in tags && "de-DE" in tags)
        assertEquals("every subtype once", tags.size, tags.toSet().size)
        for (subtype in subtypes) {
            assertEquals(subtype.attr("subtypeLocale").replace('_', '-'), subtype.attr("languageTag"))
        }
    }
}

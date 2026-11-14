package io.github.thisisthepy.dsa.agent.language

import org.junit.Assert.assertEquals
import org.junit.Test

class LanguageDefaultsTest {
    private val own = "io.github.thisisthepy.dsa.agent"
    private val recognizer = "$own/$own.platform.HostRecognitionService"
    private val spellChecker = "$own/$own.platform.HostSpellCheckerService"

    private fun writes(current: Map<String, String>, installed: Set<String> = setOf(own)) =
        LanguageDefaults.writes(own, recognizer, spellChecker, { current[it] }, { it in installed })

    @Test
    fun fr36_a_fresh_guest_gets_the_agents_services_as_its_defaults() {
        assertEquals(
            mapOf(
                LanguageDefaults.TTS_DEFAULT_SYNTH to own,
                LanguageDefaults.VOICE_RECOGNITION_SERVICE to recognizer,
                LanguageDefaults.SELECTED_SPELL_CHECKER to spellChecker,
                LanguageDefaults.SELECTED_SPELL_CHECKER_SUBTYPE to "0",
                LanguageDefaults.SPELL_CHECKER_ENABLED to "1",
            ),
            writes(emptyMap()),
        )
    }

    @Test
    fun fr36_defaults_already_set_or_naming_a_removed_app_are_handled() {
        // Already the agent's: nothing to write.
        assertEquals(
            emptyMap<String, String>(),
            writes(
                mapOf(
                    LanguageDefaults.TTS_DEFAULT_SYNTH to own,
                    LanguageDefaults.VOICE_RECOGNITION_SERVICE to recognizer,
                    LanguageDefaults.SELECTED_SPELL_CHECKER to spellChecker,
                    LanguageDefaults.SPELL_CHECKER_ENABLED to "0",
                ),
            ),
        )
        // An engine the user installed and chose stays; one since removed is replaced.
        val chosen = writes(
            mapOf(
                LanguageDefaults.TTS_DEFAULT_SYNTH to "com.example.tts",
                LanguageDefaults.VOICE_RECOGNITION_SERVICE to "com.removed/.Recognizer",
                LanguageDefaults.SELECTED_SPELL_CHECKER to "$own/.platform.HostSpellCheckerService",
                LanguageDefaults.SPELL_CHECKER_ENABLED to "1",
            ),
            installed = setOf(own, "com.example.tts"),
        )
        assertEquals(
            mapOf(
                LanguageDefaults.VOICE_RECOGNITION_SERVICE to recognizer,
                LanguageDefaults.SELECTED_SPELL_CHECKER to spellChecker,
                LanguageDefaults.SELECTED_SPELL_CHECKER_SUBTYPE to "0",
            ),
            chosen,
        )
    }
}

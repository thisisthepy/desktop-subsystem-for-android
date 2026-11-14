package io.github.thisisthepy.dsa.agent.language

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LanguageRulesTest {
    private val voices = HostVoices(
        available = true,
        voices = listOf(
            HostVoice("david", "Microsoft David", "en-US", natural = false, needsNetwork = false),
            HostVoice("aria", "Microsoft Aria", "en-US", natural = true, needsNetwork = false),
            HostVoice("hazel", "Microsoft Hazel", "en-GB", natural = false, needsNetwork = false),
            HostVoice("heami", "Microsoft Heami", "ko-KR", natural = false, needsNetwork = false),
        ),
        defaultId = "hazel",
    )

    @Test
    fun fr36_the_languages_the_host_speaks_are_the_engines() {
        assertEquals(LanguageRules.LANG_COUNTRY_AVAILABLE, LanguageRules.availability(voices.voices, "eng", "USA", ""))
        assertEquals(LanguageRules.LANG_AVAILABLE, LanguageRules.availability(voices.voices, "eng", "AUS", ""))
        assertEquals(LanguageRules.LANG_COUNTRY_AVAILABLE, LanguageRules.availability(voices.voices, "kor", "KOR", "x"))
        assertEquals(LanguageRules.LANG_NOT_SUPPORTED, LanguageRules.availability(voices.voices, "fra", "FRA", ""))
    }

    @Test
    fun fr36_the_voice_for_a_language_is_the_best_of_its_country() {
        assertEquals("aria", LanguageRules.voiceFor(voices, "eng", "USA")?.id)
        assertEquals("hazel", LanguageRules.voiceFor(voices, "eng", "GBR")?.id)
        assertEquals("hazel", LanguageRules.voiceFor(voices, "eng", "")?.id)
        assertNull(LanguageRules.voiceFor(voices, "fra", "FRA"))
        assertEquals("heami", LanguageRules.voiceForRequest(voices, "heami", "eng", "USA")?.id)
        assertEquals("aria", LanguageRules.voiceForRequest(voices, "gone", "eng", "USA")?.id)
        assertEquals(listOf("eng-USA", "eng-GBR", "kor-KOR"), LanguageRules.installedVoices(voices.voices))
    }

    @Test
    fun fr36_rate_pitch_and_locales_are_the_hosts_terms() {
        assertEquals(1f, LanguageRules.scale(100))
        assertEquals(2f, LanguageRules.scale(200))
        assertEquals(1f, LanguageRules.scale(0))
        assertEquals("en-US", LanguageRules.tagOfLocale("en_US"))
        assertEquals("", LanguageRules.tagOfLocale(null))
    }

    @Test
    fun fr36_a_word_is_in_the_dictionary_or_a_typo_with_suggestions() {
        val typo = LanguageRules.word("teh", listOf(SpellingMistake(0, 3, listOf("the", "ten"), false)), 1)
        assertEquals(LanguageRules.RESULT_ATTR_LOOKS_LIKE_TYPO, typo.attributes)
        assertEquals(listOf("the"), typo.suggestions)
        val fine = LanguageRules.word("the", emptyList(), 5)
        assertEquals(LanguageRules.RESULT_ATTR_IN_THE_DICTIONARY, fine.attributes)
        // An unchecked word is neither.
        assertEquals(0, LanguageRules.word("the", null, 5).attributes)
        // A mistake past the text's end is dropped.
        assertTrue(LanguageRules.sentence("ab", listOf(SpellingMistake(1, 5, emptyList(), false)), 5).isEmpty())
    }
}

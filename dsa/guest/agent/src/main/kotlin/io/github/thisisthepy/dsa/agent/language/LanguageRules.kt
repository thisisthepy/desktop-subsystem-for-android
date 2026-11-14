package io.github.thisisthepy.dsa.agent.language

import java.util.Locale
import java.util.MissingResourceException

/**
 * The rules between Android's speech and spelling services and the host's answers, apart
 * from Android's classes so they are tested on the JVM: which of the host's voices speaks a
 * language Android names by its ISO 639-2 and ISO 3166 codes, what a failed recognition is
 * as Android's error code, and the suggestions a spelling answer makes.
 */
object LanguageRules {
    // TextToSpeech's language answers.
    const val LANG_COUNTRY_VAR_AVAILABLE = 2
    const val LANG_COUNTRY_AVAILABLE = 1
    const val LANG_AVAILABLE = 0
    const val LANG_NOT_SUPPORTED = -2

    // Voice's quality and latency.
    const val QUALITY_VERY_HIGH = 500
    const val QUALITY_NORMAL = 300
    const val LATENCY_NORMAL = 300

    // SpeechRecognizer's error codes.
    const val ERROR_NETWORK = 2
    const val ERROR_AUDIO = 3
    const val ERROR_SERVER = 4
    const val ERROR_SPEECH_TIMEOUT = 6
    const val ERROR_NO_MATCH = 7
    const val ERROR_RECOGNIZER_BUSY = 8
    const val ERROR_INSUFFICIENT_PERMISSIONS = 9
    const val ERROR_SERVER_DISCONNECTED = 11
    const val ERROR_LANGUAGE_NOT_SUPPORTED = 12

    // SuggestionsInfo's attributes.
    const val RESULT_ATTR_IN_THE_DICTIONARY = 0x0001
    const val RESULT_ATTR_LOOKS_LIKE_TYPO = 0x0002
    const val RESULT_ATTR_HAS_RECOMMENDED_SUGGESTIONS = 0x0004

    /** The locale of a BCP 47 tag, and its ISO 639-2 and ISO 3166 alpha-3 codes. */
    data class Codes(val language: String, val country: String)

    fun codesOf(tag: String): Codes {
        val locale = Locale.forLanguageTag(tag.replace('_', '-'))
        val language = try {
            locale.isO3Language
        } catch (e: MissingResourceException) {
            locale.language
        }
        val country = try {
            locale.isO3Country
        } catch (e: MissingResourceException) {
            locale.country
        }
        return Codes(language.lowercase(Locale.ROOT), country.uppercase(Locale.ROOT))
    }

    private fun sameLanguage(voice: HostVoice, language: String) =
        language.isNotEmpty() && codesOf(voice.languageTag).language == language.lowercase(Locale.ROOT)

    private fun sameCountry(voice: HostVoice, country: String) =
        country.isNotEmpty() && codesOf(voice.languageTag).country == country.uppercase(Locale.ROOT)

    /** How well the host's voices speak the language TextToSpeech names. */
    @Suppress("UNUSED_PARAMETER")
    fun availability(voices: List<HostVoice>, language: String?, country: String?, variant: String?): Int {
        val lang = language.orEmpty()
        val ctry = country.orEmpty()
        val ofLanguage = voices.filter { sameLanguage(it, lang) }
        if (ofLanguage.isEmpty()) return LANG_NOT_SUPPORTED
        val ofCountry = ofLanguage.filter { sameCountry(it, ctry) }
        if (ofCountry.isEmpty()) return LANG_AVAILABLE
        // The host's voices have no variants, so a variant asked for is never matched.
        return LANG_COUNTRY_AVAILABLE
    }

    /**
     * The voice for the language TextToSpeech names: one of its country first, natural ones
     * before the rest, the host's default when it fits. Null when no voice speaks it.
     */
    fun voiceFor(voices: HostVoices, language: String?, country: String?): HostVoice? {
        val lang = language.orEmpty()
        val ctry = country.orEmpty()
        val ofLanguage = voices.voices.filter { sameLanguage(it, lang) }
        if (ofLanguage.isEmpty()) return null
        val default = ofLanguage.firstOrNull { it.id == voices.defaultId }
        if (default != null && (ctry.isEmpty() || sameCountry(default, ctry))) return default
        return ofLanguage.sortedWith(
            compareBy<HostVoice>({ !sameCountry(it, ctry) }, { !it.natural }, { it.needsNetwork }),
        ).first()
    }

    /** The voice a request names, or the one for its language; null when there is none. */
    fun voiceForRequest(voices: HostVoices, voiceName: String?, language: String?, country: String?): HostVoice? {
        if (!voiceName.isNullOrEmpty()) {
            voices.voices.firstOrNull { it.id == voiceName }?.let { return it }
        }
        return voiceFor(voices, language, country)
    }

    /** TextToSpeech's rate or pitch (100 is the voice's own) as the host's (1 is). */
    fun scale(percent: Int): Float = if (percent <= 0) 1f else percent / 100f

    /** SpeechRecognizer's error code for a failure. */
    fun recognitionError(failure: RecognitionFailure): Int = when (failure) {
        RecognitionFailure.NO_MATCH -> ERROR_NO_MATCH
        RecognitionFailure.SPEECH_TIMEOUT -> ERROR_SPEECH_TIMEOUT
        RecognitionFailure.DENIED -> ERROR_INSUFFICIENT_PERMISSIONS
        RecognitionFailure.NO_MICROPHONE -> ERROR_AUDIO
        RecognitionFailure.LANGUAGE_UNSUPPORTED -> ERROR_LANGUAGE_NOT_SUPPORTED
        RecognitionFailure.UNAVAILABLE -> ERROR_SERVER_DISCONNECTED
        RecognitionFailure.NETWORK -> ERROR_NETWORK
        RecognitionFailure.BUSY -> ERROR_RECOGNIZER_BUSY
        RecognitionFailure.OTHER -> ERROR_SERVER
    }

    /** A spell checker session's locale ("en_US") as a BCP 47 tag ("en-US"). */
    fun tagOfLocale(locale: String?): String {
        if (locale.isNullOrEmpty()) return ""
        return Locale.forLanguageTag(locale.replace('_', '-')).toLanguageTag().takeIf { it != "und" } ?: ""
    }

    /** What a mistake in a sentence is to Android's spell checker. */
    data class Suggestion(val offset: Int, val length: Int, val attributes: Int, val suggestions: List<String>)

    /**
     * The suggestions for one sentence: one for each mistake inside it, with the attributes
     * Android shows it by. A mistake the host is sure of has its suggestion recommended.
     */
    fun sentence(text: String, mistakes: List<SpellingMistake>, limit: Int): List<Suggestion> =
        mistakes
            .filter { it.length > 0 && it.start >= 0 && it.start + it.length <= text.length }
            .map { mistake ->
                val suggestions = mistake.suggestions.take(limit.coerceAtLeast(0))
                var attributes = RESULT_ATTR_LOOKS_LIKE_TYPO
                if (mistake.recommended && suggestions.isNotEmpty()) attributes = attributes or RESULT_ATTR_HAS_RECOMMENDED_SUGGESTIONS
                Suggestion(mistake.start, mistake.length, attributes, suggestions)
            }

    /** What a single word is to Android's spell checker: in the dictionary, or a typo with suggestions. */
    fun word(text: String, mistakes: List<SpellingMistake>?, limit: Int): Suggestion {
        if (mistakes == null) return Suggestion(0, text.length, 0, emptyList())
        val found = sentence(text, mistakes, limit).firstOrNull()
            ?: return Suggestion(0, text.length, RESULT_ATTR_IN_THE_DICTIONARY, emptyList())
        return found.copy(offset = 0, length = text.length)
    }

    /** The checks a TTS data activity reports as installed: "eng-USA" for each voice's locale. */
    fun installedVoices(voices: List<HostVoice>): List<String> =
        voices.map { codesOf(it.languageTag) }
            .filter { it.language.isNotEmpty() }
            .map { if (it.country.isEmpty()) it.language else "${it.language}-${it.country}" }
            .distinct()
}

package io.github.thisisthepy.dsa.agent.language

/**
 * Makes the agent's text-to-speech engine, speech recognition service and spell checker the
 * guest's defaults, through the secure settings Android reads them from.
 */
interface LanguageServicesSetup {
    fun useHostServices()

    companion object {
        val NONE = object : LanguageServicesSetup {
            override fun useHostServices() = Unit
        }
    }
}

/**
 * Which secure settings to write so the agent's services are the defaults. A setting already
 * naming another installed engine, recognizer or spell checker is the user's choice, as when
 * they installed one of their own and picked it, and stays. One that is empty, or names one
 * that is no longer installed, becomes the agent's. Spell checking is turned on only when it
 * was never set, so a user who turned it off keeps it off.
 */
object LanguageDefaults {
    const val TTS_DEFAULT_SYNTH = "tts_default_synth"
    const val VOICE_RECOGNITION_SERVICE = "voice_recognition_service"
    const val SELECTED_SPELL_CHECKER = "selected_spell_checker"
    const val SELECTED_SPELL_CHECKER_SUBTYPE = "selected_spell_checker_subtype"
    const val SPELL_CHECKER_ENABLED = "spell_checker_enabled"

    /** The package of a setting that names a package or a flattened component. */
    private fun packageOf(value: String): String = value.substringBefore('/')

    /**
     * The writes that make [ownPackage]'s services the defaults, given the [current] values
     * (null for unset) and whether a package is [installed].
     */
    fun writes(
        ownPackage: String,
        recognizer: String,
        spellChecker: String,
        current: (String) -> String?,
        installed: (String) -> Boolean,
    ): Map<String, String> {
        val writes = linkedMapOf<String, String>()
        fun keepsOther(key: String): Boolean {
            val value = current(key)
            return !value.isNullOrEmpty() && packageOf(value) != ownPackage && installed(packageOf(value))
        }
        if (!keepsOther(TTS_DEFAULT_SYNTH) && current(TTS_DEFAULT_SYNTH) != ownPackage) {
            writes[TTS_DEFAULT_SYNTH] = ownPackage
        }
        if (!keepsOther(VOICE_RECOGNITION_SERVICE) && current(VOICE_RECOGNITION_SERVICE) != recognizer) {
            writes[VOICE_RECOGNITION_SERVICE] = recognizer
        }
        if (!keepsOther(SELECTED_SPELL_CHECKER) && current(SELECTED_SPELL_CHECKER) != spellChecker) {
            writes[SELECTED_SPELL_CHECKER] = spellChecker
            // The subtype that follows the system's language.
            writes[SELECTED_SPELL_CHECKER_SUBTYPE] = "0"
        }
        if (current(SPELL_CHECKER_ENABLED) == null) writes[SPELL_CHECKER_ENABLED] = "1"
        return writes
    }
}

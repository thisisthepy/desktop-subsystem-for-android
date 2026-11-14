package io.github.thisisthepy.dsa.agent.platform

import android.service.textservice.SpellCheckerService
import android.view.textservice.SentenceSuggestionsInfo
import android.view.textservice.SuggestionsInfo
import android.view.textservice.TextInfo
import io.github.thisisthepy.dsa.agent.language.LanguageRules
import io.github.thisisthepy.dsa.agent.language.SpellingAnswer

/**
 * The guest's spell checker. It keeps no dictionary: each text an editor asks about goes to
 * the host's spell checker, in the session's language, and the words the host finds wrong
 * come back underlined with the host's suggestions. Editors ask a sentence at a time, which
 * goes to the host as one request. The text services manager calls it on its own threads,
 * which wait here for the host a bounded time; a host that does not answer, or has no spell
 * checker for the language, leaves the text unmarked.
 */
class HostSpellCheckerService : SpellCheckerService() {
    override fun createSession(): Session = HostSession()

    private class HostSession : Session() {
        private var tag = ""

        override fun onCreate() {
            tag = LanguageRules.tagOfLocale(locale)
        }

        private fun check(texts: List<String>, limit: Int): SpellingAnswer? =
            AgentApplication.agent?.language?.checkSpelling(tag, texts, limit, CHECK_WAIT_MILLIS)

        override fun onGetSuggestions(textInfo: TextInfo, suggestionsLimit: Int): SuggestionsInfo {
            val text = textInfo.text.orEmpty()
            val answer = check(listOf(text), suggestionsLimit)
            val mistakes = answer?.takeIf { it.supported }?.texts?.firstOrNull()
            val word = LanguageRules.word(text, mistakes, suggestionsLimit)
            return SuggestionsInfo(word.attributes, word.suggestions.toTypedArray(), textInfo.cookie, textInfo.sequence)
        }

        override fun onGetSuggestionsMultiple(
            textInfos: Array<TextInfo>,
            suggestionsLimit: Int,
            sequentialWords: Boolean,
        ): Array<SuggestionsInfo> {
            val texts = textInfos.map { it.text.orEmpty() }
            val answer = check(texts, suggestionsLimit)?.takeIf { it.supported }
            return Array(textInfos.size) { i ->
                val word = LanguageRules.word(texts[i], answer?.texts?.getOrNull(i), suggestionsLimit)
                SuggestionsInfo(word.attributes, word.suggestions.toTypedArray(), textInfos[i].cookie, textInfos[i].sequence)
            }
        }

        override fun onGetSentenceSuggestionsMultiple(
            textInfos: Array<TextInfo>,
            suggestionsLimit: Int,
        ): Array<SentenceSuggestionsInfo> {
            val texts = textInfos.map { it.text.orEmpty() }
            val answer = check(texts, suggestionsLimit)?.takeIf { it.supported }
            return Array(textInfos.size) { i ->
                val found = LanguageRules.sentence(texts[i], answer?.texts?.getOrNull(i).orEmpty(), suggestionsLimit)
                val info = textInfos[i]
                SentenceSuggestionsInfo(
                    found.map {
                        SuggestionsInfo(it.attributes, it.suggestions.toTypedArray(), info.cookie, info.sequence)
                    }.toTypedArray(),
                    found.map { it.offset }.toIntArray(),
                    found.map { it.length }.toIntArray(),
                )
            }
        }
    }

    private companion object {
        /** How long a check waits for the host; an editor asks again as the user types. */
        const val CHECK_WAIT_MILLIS = 3_000L
    }
}

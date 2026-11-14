package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.content.Intent
import android.media.AudioFormat
import android.os.Bundle
import android.speech.tts.SynthesisCallback
import android.speech.tts.SynthesisRequest
import android.speech.tts.TextToSpeech
import android.speech.tts.TextToSpeechService
import android.speech.tts.Voice
import io.github.thisisthepy.dsa.agent.language.HostLanguage
import io.github.thisisthepy.dsa.agent.language.HostVoice
import io.github.thisisthepy.dsa.agent.language.HostVoices
import io.github.thisisthepy.dsa.agent.language.LanguageRules
import io.github.thisisthepy.dsa.agent.language.SpeakRequest
import io.github.thisisthepy.dsa.agent.language.SpeechEnd
import io.github.thisisthepy.dsa.agent.language.SpeechSink
import java.util.Locale

/**
 * The guest's text-to-speech engine. It keeps no voices of its own: it lists the host's
 * ([HostLanguage.voices]) and speaks with them, the host synthesising each text and sending
 * its sound back, which goes to the [SynthesisCallback] as it comes. A host that plays the
 * sound itself sends none; the text is then over once the host has spoken it. The text to
 * speech manager calls it on its synthesis thread, which waits here while the host speaks.
 */
class HostTextToSpeechService : TextToSpeechService() {
    /** The language last loaded, as TextToSpeech names it: ISO 639-2, ISO 3166 alpha-3 and a variant. */
    @Volatile
    private var loaded = arrayOf("", "", "")

    private fun language(): HostLanguage? = AgentApplication.agent?.language

    private fun voices(): HostVoices = language()?.voices(VOICES_WAIT_MILLIS) ?: HostVoices.NONE

    override fun onIsLanguageAvailable(lang: String?, country: String?, variant: String?): Int =
        LanguageRules.availability(voices().voices, lang, country, variant)

    override fun onGetLanguage(): Array<String> = loaded

    override fun onLoadLanguage(lang: String?, country: String?, variant: String?): Int {
        val available = onIsLanguageAvailable(lang, country, variant)
        if (available >= LanguageRules.LANG_AVAILABLE) {
            loaded = arrayOf(lang.orEmpty(), if (available >= LanguageRules.LANG_COUNTRY_AVAILABLE) country.orEmpty() else "", "")
        }
        return available
    }

    override fun onStop() {
        language()?.stopSpeaking()
    }

    override fun onGetVoices(): List<Voice> = voices().voices.map(::voiceOf)

    override fun onIsValidVoiceName(voiceName: String?): Int =
        if (voices().voices.any { it.id == voiceName }) TextToSpeech.SUCCESS else TextToSpeech.ERROR

    override fun onLoadVoice(voiceName: String?): Int = onIsValidVoiceName(voiceName)

    override fun onGetDefaultVoiceNameFor(lang: String?, country: String?, variant: String?): String? =
        LanguageRules.voiceFor(voices(), lang, country)?.id

    override fun onSynthesizeText(request: SynthesisRequest, callback: SynthesisCallback) {
        val language = language()
        if (language == null) {
            callback.error(TextToSpeech.ERROR_SERVICE)
            return
        }
        val voice = LanguageRules.voiceForRequest(voices(), request.voiceName, request.language, request.country)
        if (voice == null) {
            callback.error(TextToSpeech.ERROR_INVALID_REQUEST)
            return
        }
        val text = request.charSequenceText?.toString() ?: ""
        val speak = SpeakRequest(
            text = text,
            voiceId = voice.id,
            languageTag = voice.languageTag,
            rate = LanguageRules.scale(request.speechRate),
            pitch = LanguageRules.scale(request.pitch),
        )
        val sink = object : SpeechSink {
            override fun start(sampleRateHz: Int, channels: Int) {
                callback.start(sampleRateHz, AudioFormat.ENCODING_PCM_16BIT, channels)
            }

            override fun audio(pcm: ByteArray) {
                val most = callback.maxBufferSize.coerceAtLeast(2)
                var at = 0
                while (at < pcm.size) {
                    val length = minOf(most, pcm.size - at)
                    if (callback.audioAvailable(pcm, at, length) != TextToSpeech.SUCCESS) return
                    at += length
                }
            }
        }
        val result = language.speak(speak, sink)
        when (result.end) {
            SpeechEnd.DONE -> {
                // The host played the sound itself: the text is over, with no sound here.
                if (!result.started) callback.start(SILENT_RATE, AudioFormat.ENCODING_PCM_16BIT, 1)
                callback.done()
            }
            SpeechEnd.STOPPED -> Unit
            SpeechEnd.UNAVAILABLE -> callback.error(TextToSpeech.ERROR_NOT_INSTALLED_YET)
            SpeechEnd.FAILED -> callback.error(TextToSpeech.ERROR_SYNTHESIS)
        }
    }

    companion object {
        /** How long a question about voices waits for the host. */
        const val VOICES_WAIT_MILLIS = 2_000L

        /** The format given for a text the host spoke itself, which carries no sound. */
        const val SILENT_RATE = 24_000

        fun voiceOf(voice: HostVoice): Voice {
            val features = HashSet<String>()
            if (voice.needsNetwork) {
                features += TextToSpeech.Engine.KEY_FEATURE_NETWORK_SYNTHESIS
            } else {
                features += TextToSpeech.Engine.KEY_FEATURE_EMBEDDED_SYNTHESIS
            }
            return Voice(
                voice.id,
                Locale.forLanguageTag(voice.languageTag),
                if (voice.natural) LanguageRules.QUALITY_VERY_HIGH else LanguageRules.QUALITY_NORMAL,
                LanguageRules.LATENCY_NORMAL,
                voice.needsNetwork,
                features,
            )
        }
    }
}

/**
 * Answers TextToSpeech's check of an engine's voice data, which apps and Settings ask before
 * they speak: the host's voices are the data, so every language the host speaks is installed.
 */
class HostTtsDataActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val voices = AgentApplication.agent?.language?.voices(HostTextToSpeechService.VOICES_WAIT_MILLIS)?.voices.orEmpty()
        val installed = ArrayList(LanguageRules.installedVoices(voices))
        val result = Intent()
            .putStringArrayListExtra(TextToSpeech.Engine.EXTRA_AVAILABLE_VOICES, installed)
            .putStringArrayListExtra(TextToSpeech.Engine.EXTRA_UNAVAILABLE_VOICES, ArrayList())
        setResult(
            if (installed.isEmpty()) TextToSpeech.Engine.CHECK_VOICE_DATA_FAIL else TextToSpeech.Engine.CHECK_VOICE_DATA_PASS,
            result,
        )
        finish()
    }
}

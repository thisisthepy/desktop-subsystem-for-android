package io.github.thisisthepy.dsa.agent.platform

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.RemoteException
import android.speech.RecognitionService
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import io.github.thisisthepy.dsa.agent.language.LanguageRules
import io.github.thisisthepy.dsa.agent.language.ListenRequest
import io.github.thisisthepy.dsa.agent.language.RecognitionFailure
import io.github.thisisthepy.dsa.agent.language.RecognitionListener

/**
 * The guest's speech recognition service. It records nothing in the guest: the host's speech
 * recognition listens to the host's microphone and says what it heard, which goes to the app
 * as a recognizer's results. Both microphone permissions apply: the app's Android permission
 * to record, checked here before the host is asked, and the host's own privacy settings,
 * which the host checks before it opens its microphone. Either refusal is an error the app
 * sees. One recognition runs at a time, as for every recognition service.
 */
class HostRecognitionService : RecognitionService() {
    /** The recognition under way and the callback it answers, while there is one. */
    @Volatile
    private var current: Pair<Int, Callback>? = null

    override fun onStartListening(recognizerIntent: Intent?, listener: Callback) {
        val language = AgentApplication.agent?.language
        if (language == null) {
            tell(listener) { error(LanguageRules.ERROR_SERVER_DISCONNECTED) }
            return
        }
        val request = ListenRequest(
            languageTag = recognizerIntent?.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE).orEmpty(),
            partialResults = recognizerIntent?.getBooleanExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, false) ?: false,
            maxResults = recognizerIntent?.getIntExtra(RecognizerIntent.EXTRA_MAX_RESULTS, DEFAULT_RESULTS) ?: DEFAULT_RESULTS,
            preferOffline = recognizerIntent?.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, false) ?: false,
        )
        val mayRecord = checkPermission(Manifest.permission.RECORD_AUDIO, -1, listener.callingUid) ==
            PackageManager.PERMISSION_GRANTED
        val id = language.startRecognition(request, mayRecord, Answer(listener))
        current = id to listener
    }

    override fun onStopListening(listener: Callback) {
        val running = current ?: return
        if (running.second === listener) AgentApplication.agent?.language?.stopRecognition(running.first)
    }

    override fun onCancel(listener: Callback) {
        val running = current ?: return
        if (running.second === listener) {
            current = null
            AgentApplication.agent?.language?.cancelRecognition(running.first)
        }
    }

    /** Calls the app's callback, which may have gone away. */
    private fun tell(listener: Callback, call: Callback.() -> Unit) {
        try {
            listener.call()
        } catch (e: RemoteException) {
            // The app is gone; nothing waits for the answer.
        }
    }

    private inner class Answer(private val listener: Callback) : RecognitionListener {
        private fun ended() {
            if (current?.second === listener) current = null
        }

        override fun ready() = tell(listener) { readyForSpeech(Bundle()) }

        override fun speechBegan() = tell(listener) { beginningOfSpeech() }

        override fun speechEnded() = tell(listener) { endOfSpeech() }

        override fun partial(text: String) = tell(listener) {
            partialResults(
                Bundle().apply { putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf(text)) },
            )
        }

        override fun results(texts: List<String>, confidences: List<Float>) {
            ended()
            tell(listener) {
                results(
                    Bundle().apply {
                        putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, ArrayList(texts))
                        if (confidences.size == texts.size) {
                            putFloatArray(SpeechRecognizer.CONFIDENCE_SCORES, confidences.toFloatArray())
                        }
                    },
                )
            }
        }

        override fun failed(failure: RecognitionFailure, message: String) {
            ended()
            tell(listener) { error(LanguageRules.recognitionError(failure)) }
        }
    }

    private companion object {
        const val DEFAULT_RESULTS = 5
    }
}

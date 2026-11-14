package io.github.thisisthepy.dsa.agent.language

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.CancelRecognition
import io.github.thisisthepy.dsa.control.v1.CheckSpelling
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ListSpeechVoices
import io.github.thisisthepy.dsa.control.v1.RecognitionError
import io.github.thisisthepy.dsa.control.v1.RecognitionEvent
import io.github.thisisthepy.dsa.control.v1.RecognitionEventKind
import io.github.thisisthepy.dsa.control.v1.SpeakText
import io.github.thisisthepy.dsa.control.v1.SpeechOutcome
import io.github.thisisthepy.dsa.control.v1.SpellingResult
import io.github.thisisthepy.dsa.control.v1.StartRecognition
import io.github.thisisthepy.dsa.control.v1.StopRecognition
import io.github.thisisthepy.dsa.control.v1.StopSpeaking
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/** One of the host's voices. */
data class HostVoice(
    /** The host's name for it, which a request gives back. */
    val id: String,
    val name: String,
    /** BCP 47. */
    val languageTag: String,
    val natural: Boolean,
    val needsNetwork: Boolean,
)

/** The host's voices, as it last said. */
data class HostVoices(
    val available: Boolean,
    val voices: List<HostVoice>,
    val defaultId: String,
) {
    companion object {
        val NONE = HostVoices(available = false, voices = emptyList(), defaultId = "")
    }
}

/** What an app asked to be spoken. Rate and pitch are 1 for the voice's own. */
data class SpeakRequest(
    val text: String,
    val voiceId: String,
    val languageTag: String,
    val rate: Float,
    val pitch: Float,
)

/** Where a spoken text's sound goes, on the thread that asked for it. */
interface SpeechSink {
    /** The first piece is coming, in this format (16-bit PCM). */
    fun start(sampleRateHz: Int, channels: Int)

    fun audio(pcm: ByteArray)
}

/** How a spoken text ended. */
enum class SpeechEnd { DONE, STOPPED, UNAVAILABLE, FAILED }

data class SpeechResult(val end: SpeechEnd, val message: String = "", val started: Boolean = false)

/** What an app asked the recognizer to listen for. */
data class ListenRequest(
    /** BCP 47; empty for the host's own. */
    val languageTag: String,
    val partialResults: Boolean,
    val maxResults: Int,
    val preferOffline: Boolean,
)

/** Why a recognition failed, as Android's recognizer says it. */
enum class RecognitionFailure { NO_MATCH, SPEECH_TIMEOUT, DENIED, NO_MICROPHONE, LANGUAGE_UNSUPPORTED, UNAVAILABLE, NETWORK, BUSY, OTHER }

/** What a recognition says to the app, on the agent's thread. */
interface RecognitionListener {
    fun ready()

    fun speechBegan()

    fun speechEnded()

    fun partial(text: String)

    /** The alternatives, best first, and how sure the host is of each when it says. The end. */
    fun results(texts: List<String>, confidences: List<Float>)

    /** The end. */
    fun failed(failure: RecognitionFailure, message: String)
}

/** A word the host's spell checker finds wrong, in UTF-16 code units of its text. */
data class SpellingMistake(
    val start: Int,
    val length: Int,
    val suggestions: List<String>,
    val recommended: Boolean,
)

/** The mistakes in each text asked about, in order; unsupported when the host has no checker for the language. */
data class SpellingAnswer(val supported: Boolean, val texts: List<List<SpellingMistake>>)

/**
 * The agent's side of speech and spelling from the host: the agent's text-to-speech engine,
 * speech recognition service and spell checker ask here, and the host's own voices, speech
 * recognition and spell checker answer over the control channel.
 *
 * The services call from their own threads, and the ones Android makes wait (speaking a text,
 * checking spelling, listing voices) wait here for the host's answer, a bounded time. Frames
 * go out on the agent's thread ([post] then [send]), and the host's answers come in there
 * ([handle]). When the agent connects again, whatever the last connection carried is over:
 * a text being spoken fails, a recognition fails, a spelling check finds nothing, and the
 * voices are asked for again. Recognition is refused here, without asking the host, for an app
 * that may not record.
 */
class HostLanguage(
    private val send: (ByteArray) -> Boolean,
    private val post: (() -> Unit) -> Unit,
    private val isConnected: () -> Boolean,
    private val log: Log,
) {
    @Volatile
    private var voices: HostVoices? = null
    private val voiceWaiters = ConcurrentHashMap.newKeySet<CompletableFuture<HostVoices>>()
    private val ids = AtomicInteger()
    private val speaking = ConcurrentHashMap<Int, LinkedBlockingQueue<SpeechEvent>>()
    private val listening = ConcurrentHashMap<Int, RecognitionListener>()
    private val spelling = ConcurrentHashMap<Int, CompletableFuture<SpellingAnswer>>()

    private sealed interface SpeechEvent {
        data class Audio(val rate: Int, val channels: Int, val pcm: ByteArray) : SpeechEvent
        data class Done(val result: SpeechResult) : SpeechEvent
    }

    private fun nextId(): Int = ids.updateAndGet { if (it == Int.MAX_VALUE) 1 else it + 1 }

    /** Sends `envelope` on the agent's thread; false when there is no connection to send it on. */
    private fun sendLater(envelope: Envelope, failed: () -> Unit = {}) {
        post {
            if (!isConnected() || !send(envelope.toByteArray())) failed()
        }
    }

    /**
     * The host's voices, waiting up to [timeoutMillis] for them when none were heard yet.
     * [HostVoices.NONE] when the host has none, or could not be asked.
     */
    fun voices(timeoutMillis: Long): HostVoices {
        voices?.let { return it }
        if (!isConnected()) return HostVoices.NONE
        val waiter = CompletableFuture<HostVoices>()
        voiceWaiters += waiter
        askForVoices()
        return try {
            waiter.get(timeoutMillis, TimeUnit.MILLISECONDS)
        } catch (e: Exception) {
            HostVoices.NONE
        } finally {
            voiceWaiters -= waiter
        }
    }

    /** The voices last heard, without waiting. */
    fun knownVoices(): HostVoices = voices ?: HostVoices.NONE

    private fun askForVoices() {
        sendLater(Envelope.newBuilder().setListSpeechVoices(ListSpeechVoices.getDefaultInstance()).build())
    }

    /**
     * Speaks [request] through the host, handing its sound to [sink], and returns once it is
     * over: spoken, stopped ([stopSpeaking]), or failed. Waits at most [idleMillis] between
     * the host's answers.
     */
    fun speak(request: SpeakRequest, sink: SpeechSink, idleMillis: Long = SPEECH_IDLE_MILLIS): SpeechResult {
        if (!isConnected()) return SpeechResult(SpeechEnd.UNAVAILABLE, "DSA's host is not connected")
        val id = nextId()
        val events = LinkedBlockingQueue<SpeechEvent>()
        speaking[id] = events
        try {
            val message = SpeakText.newBuilder()
                .setRequestId(id)
                .setText(request.text.take(MAX_SPEAK_CHARS))
                .setVoiceId(request.voiceId)
                .setLanguageTag(request.languageTag)
                .setRate(request.rate)
                .setPitch(request.pitch)
            sendLater(Envelope.newBuilder().setSpeakText(message).build()) {
                events.offer(SpeechEvent.Done(SpeechResult(SpeechEnd.UNAVAILABLE, "the text could not reach DSA's host")))
            }
            var started = false
            while (true) {
                val event = events.poll(idleMillis, TimeUnit.MILLISECONDS)
                    ?: return SpeechResult(SpeechEnd.FAILED, "DSA's host stopped answering", started)
                when (event) {
                    is SpeechEvent.Audio -> {
                        if (!started) {
                            sink.start(event.rate, event.channels)
                            started = true
                        }
                        sink.audio(event.pcm)
                    }
                    is SpeechEvent.Done -> return event.result.copy(started = started)
                }
            }
        } finally {
            speaking.remove(id)
        }
    }

    /** Stops every text being spoken. Called from any thread. */
    fun stopSpeaking() {
        for ((id, events) in speaking) {
            events.offer(SpeechEvent.Done(SpeechResult(SpeechEnd.STOPPED)))
            sendLater(Envelope.newBuilder().setStopSpeaking(StopSpeaking.newBuilder().setRequestId(id)).build())
        }
    }

    /**
     * Listens through the host's speech recognition for [request], telling [listener] what it
     * hears, on the agent's thread. An app that may not record ([mayRecord] false) is refused
     * at once. Returns the recognition's number, for [stopRecognition] and [cancelRecognition].
     */
    fun startRecognition(request: ListenRequest, mayRecord: Boolean, listener: RecognitionListener): Int {
        val id = nextId()
        if (!mayRecord) {
            post { listener.failed(RecognitionFailure.DENIED, "the app may not record sound") }
            return id
        }
        listening[id] = listener
        val message = StartRecognition.newBuilder()
            .setRequestId(id)
            .setLanguageTag(request.languageTag)
            .setPartialResults(request.partialResults)
            .setMaxResults(request.maxResults.coerceIn(1, MAX_RESULTS))
            .setPreferOffline(request.preferOffline)
        sendLater(Envelope.newBuilder().setStartRecognition(message).build()) {
            listening.remove(id)?.failed(RecognitionFailure.UNAVAILABLE, "DSA's host is not connected")
        }
        return id
    }

    /** The user stopped speaking: the host gives what it recognised. */
    fun stopRecognition(id: Int) {
        if (!listening.containsKey(id)) return
        sendLater(Envelope.newBuilder().setStopRecognition(StopRecognition.newBuilder().setRequestId(id)).build())
    }

    /** Drops a recognition: its listener hears nothing more. */
    fun cancelRecognition(id: Int) {
        if (listening.remove(id) == null) return
        sendLater(Envelope.newBuilder().setCancelRecognition(CancelRecognition.newBuilder().setRequestId(id)).build())
    }

    /**
     * Checks the spelling of [texts] in [languageTag] with the host's spell checker, waiting at
     * most [timeoutMillis]. Null when the host did not answer in time.
     */
    fun checkSpelling(languageTag: String, texts: List<String>, maxSuggestions: Int, timeoutMillis: Long): SpellingAnswer? {
        if (!isConnected()) return null
        val id = nextId()
        val answer = CompletableFuture<SpellingAnswer>()
        spelling[id] = answer
        try {
            val message = CheckSpelling.newBuilder()
                .setRequestId(id)
                .setLanguageTag(languageTag)
                .addAllTexts(texts.take(MAX_SPELLING_TEXTS).map { it.take(MAX_SPELLING_CHARS) })
                .setMaxSuggestions(maxSuggestions.coerceIn(0, MAX_SUGGESTIONS))
            sendLater(Envelope.newBuilder().setCheckSpelling(message).build()) { answer.complete(null) }
            return answer.get(timeoutMillis, TimeUnit.MILLISECONDS)
        } catch (e: Exception) {
            return null
        } finally {
            spelling.remove(id)
        }
    }

    /** A new connection: what the last one carried is over, and the host's voices are asked again. */
    fun connected() {
        voices = null
        for (events in speaking.values) {
            events.offer(SpeechEvent.Done(SpeechResult(SpeechEnd.FAILED, "DSA's host restarted")))
        }
        val lost = listening.values.toList()
        listening.clear()
        for (listener in lost) listener.failed(RecognitionFailure.UNAVAILABLE, "DSA's host restarted")
        for (answer in spelling.values) answer.complete(null)
        askForVoices()
    }

    /** Takes the host's answer if the frame is one. Runs on the agent's thread. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.SPEECH_VOICES -> {
                val heard = envelope.speechVoices
                val known = HostVoices(
                    available = heard.available,
                    voices = heard.voicesList.map {
                        HostVoice(it.id, it.name, it.languageTag, it.natural, it.needsNetwork)
                    },
                    defaultId = heard.defaultVoiceId,
                )
                voices = known
                for (waiter in voiceWaiters) waiter.complete(known)
                log.info("the host has ${known.voices.size} voices${if (known.available) "" else " (no speech synthesis)"}")
            }
            Envelope.BodyCase.SPEECH_AUDIO -> {
                val audio = envelope.speechAudio
                speaking[audio.requestId]?.offer(
                    SpeechEvent.Audio(audio.sampleRateHz, audio.channelCount, audio.pcm.toByteArray()),
                )
            }
            Envelope.BodyCase.SPEECH_DONE -> {
                val done = envelope.speechDone
                val end = when (done.outcome) {
                    SpeechOutcome.SPEECH_OUTCOME_DONE -> SpeechEnd.DONE
                    SpeechOutcome.SPEECH_OUTCOME_STOPPED -> SpeechEnd.STOPPED
                    SpeechOutcome.SPEECH_OUTCOME_UNAVAILABLE -> SpeechEnd.UNAVAILABLE
                    else -> SpeechEnd.FAILED
                }
                speaking[done.requestId]?.offer(SpeechEvent.Done(SpeechResult(end, done.message)))
            }
            Envelope.BodyCase.RECOGNITION_EVENT -> recognised(envelope.recognitionEvent)
            Envelope.BodyCase.SPELLING_RESULT -> {
                val result = envelope.spellingResult
                spelling[result.requestId]?.complete(answerOf(result))
            }
            else -> return false
        }
        return true
    }

    private fun recognised(event: RecognitionEvent) {
        val listener = listening[event.requestId] ?: return
        try {
            when (event.kind) {
                RecognitionEventKind.RECOGNITION_EVENT_KIND_READY -> listener.ready()
                RecognitionEventKind.RECOGNITION_EVENT_KIND_SPEECH_BEGAN -> listener.speechBegan()
                RecognitionEventKind.RECOGNITION_EVENT_KIND_SPEECH_ENDED -> listener.speechEnded()
                RecognitionEventKind.RECOGNITION_EVENT_KIND_PARTIAL ->
                    event.textsList.firstOrNull()?.let(listener::partial)
                RecognitionEventKind.RECOGNITION_EVENT_KIND_FINAL -> {
                    listening.remove(event.requestId)
                    listener.results(event.textsList, event.confidencesList)
                }
                RecognitionEventKind.RECOGNITION_EVENT_KIND_ERROR -> {
                    listening.remove(event.requestId)
                    listener.failed(failureOf(event.error), event.message)
                }
                else -> Unit
            }
        } catch (e: Exception) {
            log.warn("telling an app what the host recognised failed", e)
        }
    }

    companion object {
        const val MAX_SPEAK_CHARS = 4000
        const val MAX_SPELLING_TEXTS = 64
        const val MAX_SPELLING_CHARS = 4096
        const val MAX_SUGGESTIONS = 10
        const val MAX_RESULTS = 10

        /** How long a text being spoken waits for the host's next piece of sound. */
        const val SPEECH_IDLE_MILLIS = 15_000L

        fun failureOf(error: RecognitionError): RecognitionFailure = when (error) {
            RecognitionError.RECOGNITION_ERROR_NO_MATCH -> RecognitionFailure.NO_MATCH
            RecognitionError.RECOGNITION_ERROR_SPEECH_TIMEOUT -> RecognitionFailure.SPEECH_TIMEOUT
            RecognitionError.RECOGNITION_ERROR_DENIED -> RecognitionFailure.DENIED
            RecognitionError.RECOGNITION_ERROR_NO_MICROPHONE -> RecognitionFailure.NO_MICROPHONE
            RecognitionError.RECOGNITION_ERROR_LANGUAGE_UNSUPPORTED -> RecognitionFailure.LANGUAGE_UNSUPPORTED
            RecognitionError.RECOGNITION_ERROR_UNAVAILABLE -> RecognitionFailure.UNAVAILABLE
            RecognitionError.RECOGNITION_ERROR_NETWORK -> RecognitionFailure.NETWORK
            RecognitionError.RECOGNITION_ERROR_BUSY -> RecognitionFailure.BUSY
            else -> RecognitionFailure.OTHER
        }

        fun answerOf(result: SpellingResult): SpellingAnswer = SpellingAnswer(
            supported = result.supported,
            texts = result.textsList.map { text ->
                text.mistakesList.map { SpellingMistake(it.start, it.length, it.suggestionsList, it.recommended) }
            },
        )
    }
}

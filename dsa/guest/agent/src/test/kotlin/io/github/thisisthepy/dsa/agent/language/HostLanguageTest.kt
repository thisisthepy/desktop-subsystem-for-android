package io.github.thisisthepy.dsa.agent.language

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.RecognitionError
import io.github.thisisthepy.dsa.control.v1.RecognitionEvent
import io.github.thisisthepy.dsa.control.v1.RecognitionEventKind
import io.github.thisisthepy.dsa.control.v1.SpeechAudio
import io.github.thisisthepy.dsa.control.v1.SpeechDone
import io.github.thisisthepy.dsa.control.v1.SpeechOutcome
import io.github.thisisthepy.dsa.control.v1.SpeechVoice
import io.github.thisisthepy.dsa.control.v1.SpeechVoices
import io.github.thisisthepy.dsa.control.v1.SpellingMistake as WireMistake
import io.github.thisisthepy.dsa.control.v1.SpellingResult
import io.github.thisisthepy.dsa.control.v1.SpellingText
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The agent's speech and spelling, with the test as the host on the other end of the channel. */
class HostLanguageTest {
    private val sent = CopyOnWriteArrayList<Envelope>()
    @Volatile
    private var connected = true
    private val language = HostLanguage(
        send = { frame ->
            sent += Envelope.parseFrom(frame)
            true
        },
        post = { work -> work() },
        isConnected = { connected },
        log = RecordingLog(),
    )

    /** Waits for the agent to send a frame of [case], and returns it. */
    private fun awaitSent(case: Envelope.BodyCase): Envelope {
        val deadline = System.currentTimeMillis() + 5_000
        while (System.currentTimeMillis() < deadline) {
            sent.firstOrNull { it.bodyCase == case }?.let { return it }
            Thread.sleep(5)
        }
        throw AssertionError("the agent sent no $case: $sent")
    }

    private fun <T> inThread(work: () -> T): () -> T {
        val result = AtomicReference<Result<T>>()
        val thread = Thread { result.set(runCatching(work)) }.apply { start() }
        return {
            thread.join(5_000)
            result.get().getOrThrow()
        }
    }

    private fun voices() = Envelope.newBuilder().setSpeechVoices(
        SpeechVoices.newBuilder()
            .setAvailable(true)
            .setDefaultVoiceId("aria")
            .addVoices(SpeechVoice.newBuilder().setId("aria").setName("Microsoft Aria").setLanguageTag("en-US").setNatural(true))
            .addVoices(SpeechVoice.newBuilder().setId("heami").setName("Microsoft Heami").setLanguageTag("ko-KR")),
    ).build()

    private class Sound : SpeechSink {
        val format = mutableListOf<Pair<Int, Int>>()
        val pcm = java.io.ByteArrayOutputStream()

        override fun start(sampleRateHz: Int, channels: Int) {
            format += sampleRateHz to channels
        }

        override fun audio(pcm: ByteArray) {
            this.pcm.write(pcm)
        }
    }

    @Test
    fun fr36_tts_speaks_through_the_host() {
        val listed = inThread { language.voices(5_000) }
        awaitSent(Envelope.BodyCase.LIST_SPEECH_VOICES)
        assertTrue(language.handle(voices()))
        val heard = listed()
        assertTrue(heard.available)
        assertEquals(listOf("aria", "heami"), heard.voices.map { it.id })
        assertEquals("ko-KR", heard.voices[1].languageTag)
        assertTrue(heard.voices[0].natural)
        // An engine's voices are the host's, in Android's terms.
        val voice = LanguageRules.voiceFor(heard, "kor", "KOR")
        assertEquals("heami", voice?.id)

        val sound = Sound()
        val spoken = inThread {
            language.speak(SpeakRequest("안녕하세요.", "heami", "ko-KR", rate = 1.5f, pitch = 0.8f), sound)
        }
        val request = awaitSent(Envelope.BodyCase.SPEAK_TEXT).speakText
        assertEquals("안녕하세요.", request.text)
        assertEquals("heami", request.voiceId)
        assertEquals("ko-KR", request.languageTag)
        assertEquals(1.5f, request.rate)
        assertEquals(0.8f, request.pitch)
        for (piece in listOf(byteArrayOf(1, 0, 2, 0), byteArrayOf(3, 0))) {
            language.handle(
                Envelope.newBuilder().setSpeechAudio(
                    SpeechAudio.newBuilder().setRequestId(request.requestId).setSampleRateHz(24_000)
                        .setChannelCount(1).setPcm(ByteString.copyFrom(piece)),
                ).build(),
            )
        }
        language.handle(done(request.requestId, SpeechOutcome.SPEECH_OUTCOME_DONE))
        val result = spoken()
        assertEquals(SpeechEnd.DONE, result.end)
        assertTrue(result.started)
        assertEquals(listOf(24_000 to 1), sound.format)
        assertArrayEquals(byteArrayOf(1, 0, 2, 0, 3, 0), sound.pcm.toByteArray())
    }

    private fun done(id: Int, outcome: SpeechOutcome, message: String = "") = Envelope.newBuilder()
        .setSpeechDone(SpeechDone.newBuilder().setRequestId(id).setOutcome(outcome).setMessage(message))
        .build()

    @Test
    fun fr36_a_text_the_host_played_itself_ends_with_no_sound() {
        val sound = Sound()
        val spoken = inThread { language.speak(SpeakRequest("Hi.", "", "en-US", 1f, 1f), sound) }
        val request = awaitSent(Envelope.BodyCase.SPEAK_TEXT).speakText
        language.handle(done(request.requestId, SpeechOutcome.SPEECH_OUTCOME_DONE))
        val result = spoken()
        assertEquals(SpeechEnd.DONE, result.end)
        assertFalse(result.started)
        assertTrue(sound.format.isEmpty())
    }

    @Test
    fun fr36_stopping_speech_stops_it_on_the_host() {
        val spoken = inThread { language.speak(SpeakRequest("A long text.", "", "en-US", 1f, 1f), Sound()) }
        val request = awaitSent(Envelope.BodyCase.SPEAK_TEXT).speakText
        language.stopSpeaking()
        assertEquals(SpeechEnd.STOPPED, spoken().end)
        assertEquals(request.requestId, awaitSent(Envelope.BodyCase.STOP_SPEAKING).stopSpeaking.requestId)
    }

    @Test
    fun fr36_speech_the_host_cannot_give_fails_with_its_reason() {
        val spoken = inThread { language.speak(SpeakRequest("Bonjour.", "", "fr-FR", 1f, 1f), Sound()) }
        val request = awaitSent(Envelope.BodyCase.SPEAK_TEXT).speakText
        language.handle(done(request.requestId, SpeechOutcome.SPEECH_OUTCOME_UNAVAILABLE, "no French voice"))
        val result = spoken()
        assertEquals(SpeechEnd.UNAVAILABLE, result.end)
        assertEquals("no French voice", result.message)
    }

    private class Heard : RecognitionListener {
        val events = mutableListOf<String>()

        override fun ready() {
            events += "ready"
        }

        override fun speechBegan() {
            events += "began"
        }

        override fun speechEnded() {
            events += "ended"
        }

        override fun partial(text: String) {
            events += "partial $text"
        }

        override fun results(texts: List<String>, confidences: List<Float>) {
            events += "results $texts $confidences"
        }

        override fun failed(failure: RecognitionFailure, message: String) {
            events += "failed $failure"
        }
    }

    private fun recognition(id: Int, kind: RecognitionEventKind, vararg texts: String) =
        RecognitionEvent.newBuilder().setRequestId(id).setKind(kind).addAllTexts(texts.toList())

    @Test
    fun fr36_speech_recognition_comes_from_the_host() {
        val heard = Heard()
        val id = language.startRecognition(ListenRequest("en-US", partialResults = true, maxResults = 3, preferOffline = true), true, heard)
        val start = awaitSent(Envelope.BodyCase.START_RECOGNITION).startRecognition
        assertEquals(id, start.requestId)
        assertEquals("en-US", start.languageTag)
        assertTrue(start.partialResults && start.preferOffline)
        assertEquals(3, start.maxResults)
        for (event in listOf(
            recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_READY),
            recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_SPEECH_BEGAN),
            recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_PARTIAL, "hello"),
            recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_SPEECH_ENDED),
            recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_FINAL, "hello world", "yellow world")
                .addConfidences(0.9f).addConfidences(0.4f),
            // Nothing after the end reaches the app.
            recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_PARTIAL, "late"),
        )) {
            assertTrue(language.handle(Envelope.newBuilder().setRecognitionEvent(event).build()))
        }
        assertEquals(
            listOf("ready", "began", "partial hello", "ended", "results [hello world, yellow world] [0.9, 0.4]"),
            heard.events,
        )
    }

    @Test
    fun fr36_speech_recognition_with_the_hosts_microphone_blocked_ends_with_an_error() {
        val heard = Heard()
        val id = language.startRecognition(ListenRequest("", false, 1, false), true, heard)
        awaitSent(Envelope.BodyCase.START_RECOGNITION)
        language.handle(
            Envelope.newBuilder().setRecognitionEvent(
                recognition(id, RecognitionEventKind.RECOGNITION_EVENT_KIND_ERROR)
                    .setError(RecognitionError.RECOGNITION_ERROR_DENIED)
                    .setMessage("the microphone is off in the privacy settings"),
            ).build(),
        )
        assertEquals(listOf("failed DENIED"), heard.events)
        assertEquals(LanguageRules.ERROR_INSUFFICIENT_PERMISSIONS, LanguageRules.recognitionError(RecognitionFailure.DENIED))
    }

    @Test
    fun fr26_an_app_that_may_not_record_is_refused_without_asking_the_host() {
        val heard = Heard()
        language.startRecognition(ListenRequest("", false, 1, false), false, heard)
        assertEquals(listOf("failed DENIED"), heard.events)
        assertTrue(sent.none { it.bodyCase == Envelope.BodyCase.START_RECOGNITION })
    }

    @Test
    fun fr36_spell_checking_comes_from_the_host() {
        val texts = listOf("I saw teh cat.", "All good.")
        val answer = inThread { language.checkSpelling("en-US", texts, 3, 5_000) }
        val check = awaitSent(Envelope.BodyCase.CHECK_SPELLING).checkSpelling
        assertEquals("en-US", check.languageTag)
        assertEquals(texts, check.textsList)
        assertEquals(3, check.maxSuggestions)
        language.handle(
            Envelope.newBuilder().setSpellingResult(
                SpellingResult.newBuilder()
                    .setRequestId(check.requestId)
                    .setSupported(true)
                    .addTexts(
                        SpellingText.newBuilder().addMistakes(
                            WireMistake.newBuilder().setStart(6).setLength(3).addSuggestions("the").addSuggestions("tech")
                                .setRecommended(true),
                        ),
                    )
                    .addTexts(SpellingText.getDefaultInstance()),
            ).build(),
        )
        val result = answer()!!
        assertTrue(result.supported)
        assertEquals(listOf(SpellingMistake(6, 3, listOf("the", "tech"), true)), result.texts[0])
        assertTrue(result.texts[1].isEmpty())
        // As Android's spell checker shows it: a typo whose first suggestion is recommended.
        val sentence = LanguageRules.sentence(texts[0], result.texts[0], 1)
        assertEquals(
            listOf(
                LanguageRules.Suggestion(
                    6,
                    3,
                    LanguageRules.RESULT_ATTR_LOOKS_LIKE_TYPO or LanguageRules.RESULT_ATTR_HAS_RECOMMENDED_SUGGESTIONS,
                    listOf("the"),
                ),
            ),
            sentence,
        )
    }

    @Test
    fun nfr5_a_new_connection_ends_what_the_last_one_carried() {
        val spoken = inThread { language.speak(SpeakRequest("Hello.", "", "en-US", 1f, 1f), Sound()) }
        awaitSent(Envelope.BodyCase.SPEAK_TEXT)
        val checked = inThread { language.checkSpelling("en-US", listOf("teh"), 3, 5_000) }
        awaitSent(Envelope.BodyCase.CHECK_SPELLING)
        val heard = Heard()
        language.startRecognition(ListenRequest("", false, 1, false), true, heard)
        sent.clear()
        language.connected()
        assertEquals(SpeechEnd.FAILED, spoken().end)
        assertNull(checked())
        assertEquals(listOf("failed UNAVAILABLE"), heard.events)
        awaitSent(Envelope.BodyCase.LIST_SPEECH_VOICES)
    }

    @Test
    fun fr36_without_the_host_nothing_waits() {
        connected = false
        assertEquals(HostVoices.NONE, language.voices(5_000))
        assertEquals(SpeechEnd.UNAVAILABLE, language.speak(SpeakRequest("Hi.", "", "", 1f, 1f), Sound()).end)
        assertNull(language.checkSpelling("en-US", listOf("teh"), 3, 5_000))
    }
}

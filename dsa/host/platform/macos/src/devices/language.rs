//! Speech and spelling for the user's Android, from the Mac's own.
//!
//! - Speech synthesis: `AVSpeechSynthesizer`, with every voice the Mac has (the enhanced and
//!   premium ones marked as natural), each sentence written to buffers whose PCM goes to
//!   Android.
//! - Speech recognition: `SFSpeechRecognizer` on the microphone's sound, through
//!   `AVAudioEngine`, with its partial results. macOS asks the user for the microphone and
//!   for speech recognition the first time; a refusal is an error in Android. The end of what
//!   is said is found from the sound's level, as Android's recognizers do.
//! - Spell checking: `NSSpellChecker`, whose ranges are already in UTF-16 code units.

use dsa_host_core::language::RecognitionFailure;

/// `AVSpeechUtterance`'s rate (0 to 1, 0.5 the voice's own) for `rate`, where 1 is the
/// voice's own.
pub fn av_rate(rate: f32) -> f32 {
    let rate = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    };
    let value = if rate >= 1.0 {
        0.5 + (rate - 1.0) / 3.0 * 0.5
    } else {
        0.5 * rate
    };
    value.clamp(0.0, 1.0)
}

/// `AVSpeechUtterance`'s pitch multiplier (0.5 to 2) for `pitch`.
pub fn av_pitch(pitch: f32) -> f32 {
    if pitch.is_finite() && pitch > 0.0 {
        pitch.clamp(0.5, 2.0)
    } else {
        1.0
    }
}

/// Whether an `AVSpeechSynthesisVoiceQuality` is an enhanced or premium voice, which sound
/// natural.
pub fn natural_quality(quality: isize) -> bool {
    quality >= 2
}

/// Interleaved 16-bit little-endian PCM from one slice of samples per channel.
pub fn interleave_i16(channels: &[&[i16]]) -> Vec<u8> {
    let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let mut pcm = Vec::with_capacity(frames * channels.len() * 2);
    for frame in 0..frames {
        for channel in channels {
            pcm.extend_from_slice(&channel[frame].to_le_bytes());
        }
    }
    pcm
}

/// Interleaved 16-bit little-endian PCM from one slice of float samples (-1 to 1) per channel.
pub fn interleave_f32(channels: &[&[f32]]) -> Vec<u8> {
    let converted: Vec<Vec<i16>> = channels
        .iter()
        .map(|c| {
            c.iter()
                .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16)
                .collect()
        })
        .collect();
    let slices: Vec<&[i16]> = converted.iter().map(|c| c.as_slice()).collect();
    interleave_i16(&slices)
}

/// The root mean square of `samples`, from 0 (silence) to 1.
pub fn level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// The level above which the microphone hears speech.
pub const SPEECH_LEVEL: f32 = 0.02;

/// Of the languages `NSSpellChecker` has (such as "en", "en_GB"), the one for `tag`: the very
/// one, else one of its language.
pub fn spell_language(available: &[String], tag: &str) -> Option<String> {
    use dsa_host_core::language::text::{normalize_tag, primary_language};
    let wanted = normalize_tag(tag);
    let language = primary_language(&wanted);
    available
        .iter()
        .find(|found| normalize_tag(found) == wanted)
        .or_else(|| {
            available
                .iter()
                .find(|found| normalize_tag(found) == language)
        })
        .or_else(|| {
            available
                .iter()
                .find(|found| primary_language(found) == language)
        })
        .cloned()
}

/// What `SFSpeechRecognizer`'s authorization status says, as the reason it may not be used.
pub fn speech_authorization(status: isize) -> Result<(), String> {
    match status {
        3 => Ok(()),
        2 => Err("a policy on this Mac does not let programs recognise speech".into()),
        1 => Err(
            "the user did not allow DSA to recognise speech; it can be allowed in System \
             Settings, Privacy & Security, Speech Recognition"
                .into(),
        ),
        _ => Err("the user has not answered macOS's prompt for speech recognition".into()),
    }
}

/// What a recognition error of `domain` and `code` means for Android.
pub fn recognition_failure(domain: &str, code: isize) -> RecognitionFailure {
    match (domain, code) {
        (_, 1110) => RecognitionFailure::SpeechTimeout,
        (_, 203) | (_, 1101) => RecognitionFailure::NoMatch,
        ("NSURLErrorDomain", _) | (_, 1700) => RecognitionFailure::Network,
        ("kLSRErrorDomain", 300) | (_, 1107) => RecognitionFailure::Unavailable,
        _ => RecognitionFailure::Other,
    }
}

#[cfg(target_os = "macos")]
pub use system::{MacRecognition, MacSpeech, MacSpelling};

#[cfg(target_os = "macos")]
mod system {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use dsa_host_core::language::text::pick_voice;
    use dsa_host_core::language::{
        Heard, HeardSink, Listen, Listening, Mistake, Pcm, RecognitionFailure, SpeakError, Speech,
        SpeechRecognizer, SpeechSynthesizer, SpellChecker, Voice, Voices,
    };
    use objc2::msg_send;
    use objc2::rc::{autoreleasepool, Allocated, Retained};
    use objc2::runtime::AnyObject;

    use super::{
        av_pitch, av_rate, interleave_f32, interleave_i16, level, natural_quality,
        recognition_failure, speech_authorization, spell_language, SPEECH_LEVEL,
    };
    use crate::devices::permissions::{self, Device};
    use crate::objc::{class, ns_string, on_main, string_of, NSRange, NOT_FOUND};

    #[link(name = "AVFAudio", kind = "framework")]
    extern "C" {}
    #[link(name = "Speech", kind = "framework")]
    extern "C" {}

    /// `AVAudioCommonFormat` values.
    const PCM_FLOAT32: usize = 1;
    const PCM_INT16: usize = 3;
    /// How long a sentence may take to be written.
    const SENTENCE: Duration = Duration::from_secs(60);
    /// How long the Mac waits for the user to start speaking, and for the end of what they say.
    const INITIAL_SILENCE: Duration = Duration::from_secs(6);
    const END_SILENCE: Duration = Duration::from_millis(1200);

    /// The strings of an `NSArray` of `NSString`.
    unsafe fn strings(array: *mut AnyObject) -> Vec<String> {
        if array.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![array, count];
        (0..count)
            .map(|i| {
                let item: *mut AnyObject = msg_send![array, objectAtIndex: i];
                string_of(item)
            })
            .collect()
    }

    /// The Mac's speech synthesis.
    pub struct MacSpeech {
        synthesizer: Option<Retained<AnyObject>>,
    }

    impl MacSpeech {
        pub fn new() -> MacSpeech {
            MacSpeech { synthesizer: None }
        }
    }

    impl Default for MacSpeech {
        fn default() -> MacSpeech {
            MacSpeech::new()
        }
    }

    /// The sound of one buffer `writeUtterance:toBufferCallback:` gave, or `None` for the
    /// empty one that ends it.
    unsafe fn pcm_of(buffer: *mut AnyObject) -> Option<Pcm> {
        if buffer.is_null() {
            return None;
        }
        let frames: u32 = msg_send![buffer, frameLength];
        if frames == 0 {
            return None;
        }
        let format: *mut AnyObject = msg_send![buffer, format];
        if format.is_null() {
            return None;
        }
        let common: usize = msg_send![format, commonFormat];
        let rate: f64 = msg_send![format, sampleRate];
        let channels: u32 = msg_send![format, channelCount];
        let interleaved: bool = msg_send![format, isInterleaved];
        let frames = frames as usize;
        let count = channels.max(1) as usize;
        let data = match common {
            PCM_INT16 => {
                let planes: *const *const i16 = msg_send![buffer, int16ChannelData];
                if planes.is_null() {
                    return None;
                }
                if interleaved {
                    let samples = std::slice::from_raw_parts(*planes, frames * count);
                    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
                } else {
                    let slices: Vec<&[i16]> = (0..count)
                        .map(|c| std::slice::from_raw_parts(*planes.add(c), frames))
                        .collect();
                    interleave_i16(&slices)
                }
            }
            PCM_FLOAT32 => {
                let planes: *const *const f32 = msg_send![buffer, floatChannelData];
                if planes.is_null() {
                    return None;
                }
                if interleaved {
                    let samples = std::slice::from_raw_parts(*planes, frames * count);
                    interleave_f32(&[samples])
                } else {
                    let slices: Vec<&[f32]> = (0..count)
                        .map(|c| std::slice::from_raw_parts(*planes.add(c), frames))
                        .collect();
                    interleave_f32(&slices)
                }
            }
            _ => return None,
        };
        Some(Pcm {
            sample_rate_hz: rate.round() as u32,
            channels: count as u32,
            data,
        })
    }

    impl SpeechSynthesizer for MacSpeech {
        fn voices(&mut self) -> Result<Voices, String> {
            autoreleasepool(|_| unsafe {
                let voice_class = class(c"AVSpeechSynthesisVoice")?;
                let list: *mut AnyObject = msg_send![voice_class, speechVoices];
                if list.is_null() {
                    return Err("the Mac has no voices".to_string());
                }
                let count: usize = msg_send![list, count];
                let mut voices = Vec::new();
                for i in 0..count {
                    let voice: *mut AnyObject = msg_send![list, objectAtIndex: i];
                    let id: *mut AnyObject = msg_send![voice, identifier];
                    let name: *mut AnyObject = msg_send![voice, name];
                    let language: *mut AnyObject = msg_send![voice, language];
                    let quality: isize = msg_send![voice, quality];
                    voices.push(Voice {
                        id: string_of(id),
                        name: string_of(name),
                        language_tag: string_of(language),
                        natural: natural_quality(quality),
                        needs_network: false,
                    });
                }
                let code: *mut AnyObject = msg_send![voice_class, currentLanguageCode];
                let default: *mut AnyObject = msg_send![voice_class, voiceWithLanguage: code];
                let default_id = if default.is_null() {
                    String::new()
                } else {
                    let id: *mut AnyObject = msg_send![default, identifier];
                    string_of(id)
                };
                if voices.is_empty() {
                    return Err("the Mac has no voices".to_string());
                }
                Ok(Voices { voices, default_id })
            })
        }

        fn speak(
            &mut self,
            speech: &Speech,
            stop: &AtomicBool,
            sound: &mut dyn FnMut(Pcm),
        ) -> Result<(), SpeakError> {
            let listed = self.voices().map_err(SpeakError::Unavailable)?;
            let Some(voice) = pick_voice(
                &listed.voices,
                &speech.voice_id,
                &speech.language_tag,
                &listed.default_id,
            ) else {
                return Err(SpeakError::Unavailable(format!(
                    "the Mac has no voice for {}; add one in System Settings, Accessibility, \
                     Spoken Content",
                    speech.language_tag
                )));
            };
            let (pieces, received) = channel::<Option<Pcm>>();
            let started = autoreleasepool(|_| unsafe {
                let fail = |what: &str| SpeakError::Failed(what.to_string());
                if self.synthesizer.is_none() {
                    let synthesizer: Option<Retained<AnyObject>> = msg_send![
                        class(c"AVSpeechSynthesizer").map_err(SpeakError::Unavailable)?,
                        new
                    ];
                    self.synthesizer = synthesizer;
                }
                let synthesizer = self
                    .synthesizer
                    .as_ref()
                    .ok_or_else(|| fail("the Mac's speech synthesis could not start"))?;
                let utterance: *mut AnyObject = msg_send![
                    class(c"AVSpeechUtterance").map_err(SpeakError::Unavailable)?,
                    speechUtteranceWithString: &*ns_string(&speech.text)
                ];
                if utterance.is_null() {
                    return Err(fail("the text could not be prepared"));
                }
                let voice_object: *mut AnyObject = msg_send![
                    class(c"AVSpeechSynthesisVoice").map_err(SpeakError::Unavailable)?,
                    voiceWithIdentifier: &*ns_string(&voice.id)
                ];
                if !voice_object.is_null() {
                    let _: () = msg_send![utterance, setVoice: voice_object];
                }
                let _: () = msg_send![utterance, setRate: av_rate(speech.rate)];
                let _: () = msg_send![utterance, setPitchMultiplier: av_pitch(speech.pitch)];
                let callback = RcBlock::new(move |buffer: *mut AnyObject| {
                    let _ = pieces.send(pcm_of(buffer));
                });
                let _: () = msg_send![
                    &**synthesizer,
                    writeUtterance: utterance,
                    toBufferCallback: &*callback
                ];
                Ok(())
            });
            started?;
            let deadline = Instant::now() + SENTENCE;
            loop {
                if stop.load(Ordering::Relaxed) {
                    if let Some(synthesizer) = &self.synthesizer {
                        // AVSpeechBoundaryImmediate.
                        let _: bool =
                            unsafe { msg_send![&**synthesizer, stopSpeakingAtBoundary: 0isize] };
                    }
                    return Ok(());
                }
                match received.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(pcm)) => sound(pcm),
                    Ok(None) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(RecvTimeoutError::Timeout) => {
                        if Instant::now() > deadline {
                            return Err(SpeakError::Failed(
                                "the Mac's speech synthesis did not finish".into(),
                            ));
                        }
                    }
                }
            }
        }
    }

    /// The Mac's speech recognition.
    #[derive(Default)]
    pub struct MacRecognition;

    /// Asks the user for speech recognition when they were not asked yet, and waits.
    unsafe fn ask_for_speech() -> Result<(), String> {
        let recognizer_class = class(c"SFSpeechRecognizer")?;
        let status: isize = msg_send![recognizer_class, authorizationStatus];
        if status != 0 {
            return speech_authorization(status);
        }
        let (answer, answered) = channel();
        let handler = RcBlock::new(move |status: isize| {
            let _ = answer.send(status);
        });
        let _: () = msg_send![recognizer_class, requestAuthorization: &*handler];
        match answered.recv_timeout(Duration::from_secs(120)) {
            Ok(status) => speech_authorization(status),
            Err(_) => speech_authorization(0),
        }
    }

    /// What a recognition result says: its transcriptions, best first, and whether it is
    /// the last.
    unsafe fn transcriptions(
        result: *mut AnyObject,
        max: usize,
    ) -> (Vec<(String, Option<f32>)>, bool) {
        let last: bool = msg_send![result, isFinal];
        let list: *mut AnyObject = msg_send![result, transcriptions];
        let mut found = Vec::new();
        if !list.is_null() {
            let count: usize = msg_send![list, count];
            for i in 0..count.min(max) {
                let transcription: *mut AnyObject = msg_send![list, objectAtIndex: i];
                let text: *mut AnyObject = msg_send![transcription, formattedString];
                let segments: *mut AnyObject = msg_send![transcription, segments];
                let mut confidence = None;
                if last && !segments.is_null() {
                    let n: usize = msg_send![segments, count];
                    let mut sum = 0.0f32;
                    for s in 0..n {
                        let segment: *mut AnyObject = msg_send![segments, objectAtIndex: s];
                        let c: f32 = msg_send![segment, confidence];
                        sum += c;
                    }
                    if n > 0 {
                        confidence = Some(sum / n as f32);
                    }
                }
                found.push((string_of(text), confidence));
            }
        }
        if found.is_empty() {
            let best: *mut AnyObject = msg_send![result, bestTranscription];
            if !best.is_null() {
                let text: *mut AnyObject = msg_send![best, formattedString];
                found.push((string_of(text), None));
            }
        }
        (found, last)
    }

    impl SpeechRecognizer for MacRecognition {
        fn start(
            &mut self,
            listen: &Listen,
            heard: HeardSink,
        ) -> Result<Box<dyn Listening>, (RecognitionFailure, String)> {
            permissions::ask(Device::Microphone)
                .map_err(|why| (RecognitionFailure::Denied, why))?;
            unsafe { ask_for_speech() }.map_err(|why| (RecognitionFailure::Denied, why))?;
            let heard: Arc<HeardSink> = Arc::new(heard);
            autoreleasepool(|_| unsafe { start(listen, heard) })
        }
    }

    unsafe fn start(
        listen: &Listen,
        heard: Arc<HeardSink>,
    ) -> Result<Box<dyn Listening>, (RecognitionFailure, String)> {
        let unavailable = |why: String| (RecognitionFailure::Unavailable, why);
        let recognizer_class = class(c"SFSpeechRecognizer").map_err(unavailable)?;
        let allocated: Allocated<AnyObject> = msg_send![recognizer_class, alloc];
        let recognizer: Option<Retained<AnyObject>> = if listen.language_tag.is_empty() {
            msg_send![allocated, init]
        } else {
            let locale: *mut AnyObject = msg_send![
                class(c"NSLocale").map_err(unavailable)?,
                localeWithLocaleIdentifier: &*ns_string(&listen.language_tag)
            ];
            msg_send![allocated, initWithLocale: locale]
        };
        let Some(recognizer) = recognizer else {
            return Err((
                RecognitionFailure::LanguageUnsupported,
                format!(
                    "the Mac does not recognise speech in {}",
                    listen.language_tag
                ),
            ));
        };
        let available: bool = msg_send![&*recognizer, isAvailable];
        if !available {
            return Err(unavailable(
                "the Mac's speech recognition is not available now".into(),
            ));
        }
        let request: Option<Retained<AnyObject>> = msg_send![
            class(c"SFSpeechAudioBufferRecognitionRequest").map_err(unavailable)?,
            new
        ];
        let request = request.ok_or_else(|| unavailable("no recognition request".into()))?;
        let _: () = msg_send![&*request, setShouldReportPartialResults: listen.partial_results];
        if listen.prefer_offline {
            let on_device: bool = msg_send![&*recognizer, supportsOnDeviceRecognition];
            if on_device {
                let _: () = msg_send![&*request, setRequiresOnDeviceRecognition: true];
            }
        }
        let engine: Option<Retained<AnyObject>> =
            msg_send![class(c"AVAudioEngine").map_err(unavailable)?, new];
        let engine = engine.ok_or_else(|| unavailable("no audio engine".into()))?;
        let input: *mut AnyObject = msg_send![&*engine, inputNode];
        if input.is_null() {
            return Err((
                RecognitionFailure::NoMicrophone,
                "this Mac has no microphone".into(),
            ));
        }
        let format: *mut AnyObject = msg_send![input, outputFormatForBus: 0usize];
        let channels: u32 = if format.is_null() {
            0
        } else {
            msg_send![format, channelCount]
        };
        if channels == 0 {
            return Err((
                RecognitionFailure::NoMicrophone,
                "this Mac has no microphone".into(),
            ));
        }

        // The end of what is said, from the sound's level.
        let began = Instant::now();
        let ended = Arc::new(AtomicBool::new(false));
        let spoke: Arc<Mutex<Option<Instant>>> = Arc::default();
        let tap_request = request.clone();
        let tap_ended = Arc::clone(&ended);
        let tap_heard = Arc::clone(&heard);
        let tap = RcBlock::new(move |buffer: *mut AnyObject, _when: *mut AnyObject| {
            if buffer.is_null() || tap_ended.load(Ordering::Relaxed) {
                return;
            }
            let _: () = msg_send![&*tap_request, appendAudioPCMBuffer: buffer];
            let frames: u32 = msg_send![buffer, frameLength];
            let planes: *const *const f32 = msg_send![buffer, floatChannelData];
            let loud = !planes.is_null()
                && level(std::slice::from_raw_parts(*planes, frames as usize)) > SPEECH_LEVEL;
            let now = Instant::now();
            let mut spoke = spoke.lock().unwrap_or_else(|e| e.into_inner());
            if loud {
                if spoke.is_none() {
                    tap_heard(Heard::SpeechBegan);
                }
                *spoke = Some(now);
            }
            let over = match *spoke {
                Some(last) => now.duration_since(last) > END_SILENCE,
                None => now.duration_since(began) > INITIAL_SILENCE,
            };
            if over && !tap_ended.swap(true, Ordering::Relaxed) {
                if spoke.is_some() {
                    tap_heard(Heard::SpeechEnded);
                }
                let _: () = msg_send![&*tap_request, endAudio];
            }
        });
        let _: () = msg_send![
            input,
            installTapOnBus: 0usize,
            bufferSize: 1024u32,
            format: format,
            block: &*tap
        ];

        let max = listen.max_results;
        let result_heard = Arc::clone(&heard);
        let handler = RcBlock::new(move |result: *mut AnyObject, error: *mut AnyObject| {
            if !result.is_null() {
                let (found, last) = transcriptions(result, max);
                if last {
                    result_heard(Heard::Final(found));
                    return;
                }
                if let Some((text, _)) = found.into_iter().next() {
                    result_heard(Heard::Partial(text));
                }
            }
            if !error.is_null() {
                let code: isize = msg_send![error, code];
                let domain: *mut AnyObject = msg_send![error, domain];
                let description: *mut AnyObject = msg_send![error, localizedDescription];
                let failure = recognition_failure(&string_of(domain), code);
                result_heard(Heard::Failed(failure, string_of(description)));
            }
        });
        let task: Option<Retained<AnyObject>> = msg_send![
            &*recognizer,
            recognitionTaskWithRequest: &*request,
            resultHandler: &*handler
        ];
        let _: () = msg_send![&*engine, prepare];
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let running: bool = msg_send![&*engine, startAndReturnError: &mut error];
        if !running {
            let _: () = msg_send![input, removeTapOnBus: 0usize];
            if let Some(task) = &task {
                let _: () = msg_send![&**task, cancel];
            }
            let description: *mut AnyObject = if error.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![error, localizedDescription]
            };
            return Err((
                RecognitionFailure::NoMicrophone,
                format!(
                    "the microphone could not be opened: {}",
                    string_of(description)
                ),
            ));
        }
        heard(Heard::Ready);
        Ok(Box::new(MacListening {
            engine,
            request,
            task,
            _recognizer: recognizer,
            _tap: tap,
            _handler: handler,
            ended,
            stopped: false,
        }))
    }

    /// A recognition the Mac runs.
    struct MacListening {
        engine: Retained<AnyObject>,
        request: Retained<AnyObject>,
        task: Option<Retained<AnyObject>>,
        _recognizer: Retained<AnyObject>,
        _tap: RcBlock<dyn Fn(*mut AnyObject, *mut AnyObject)>,
        _handler: RcBlock<dyn Fn(*mut AnyObject, *mut AnyObject)>,
        ended: Arc<AtomicBool>,
        stopped: bool,
    }

    impl MacListening {
        fn stop_engine(&mut self) {
            if self.stopped {
                return;
            }
            self.stopped = true;
            unsafe {
                let _: () = msg_send![&*self.engine, stop];
                let input: *mut AnyObject = msg_send![&*self.engine, inputNode];
                if !input.is_null() {
                    let _: () = msg_send![input, removeTapOnBus: 0usize];
                }
            }
        }
    }

    impl Listening for MacListening {
        fn stop(&mut self) {
            self.stop_engine();
            if !self.ended.swap(true, Ordering::Relaxed) {
                unsafe {
                    let _: () = msg_send![&*self.request, endAudio];
                }
            }
        }

        fn cancel(&mut self) {
            self.ended.store(true, Ordering::Relaxed);
            self.stop_engine();
            if let Some(task) = &self.task {
                unsafe {
                    let _: () = msg_send![&**task, cancel];
                }
            }
        }
    }

    impl Drop for MacListening {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    /// The Mac's spell checker, used on the main thread, as AppKit wants.
    #[derive(Default)]
    pub struct MacSpelling;

    /// How long a check waits for the main thread.
    const CHECK: Duration = Duration::from_secs(10);

    unsafe fn check_on_main(
        tag: &str,
        text: &str,
        max: usize,
    ) -> Result<Option<Vec<Mistake>>, String> {
        let checker: *mut AnyObject = msg_send![class(c"NSSpellChecker")?, sharedSpellChecker];
        if checker.is_null() {
            return Err("the Mac's spell checker is not available".into());
        }
        let available: *mut AnyObject = msg_send![checker, availableLanguages];
        let tag = if tag.trim().is_empty() {
            let language: *mut AnyObject = msg_send![checker, language];
            string_of(language)
        } else {
            tag.to_string()
        };
        let Some(language) = spell_language(&strings(available), &tag) else {
            return Ok(None);
        };
        if text.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let string = ns_string(text);
        let language = ns_string(&language);
        let length: usize = msg_send![&*string, length];
        let mut mistakes = Vec::new();
        let mut at = 0usize;
        while at < length {
            let range: NSRange = msg_send![
                checker,
                checkSpellingOfString: &*string,
                startingAt: at as isize,
                language: &*language,
                wrap: false,
                inSpellDocumentWithTag: 0isize,
                wordCount: std::ptr::null_mut::<isize>()
            ];
            if range.location == NOT_FOUND || range.length == 0 || range.location < at {
                break;
            }
            let guesses: *mut AnyObject = msg_send![
                checker,
                guessesForWordRange: range,
                inString: &*string,
                language: &*language,
                inSpellDocumentWithTag: 0isize
            ];
            let correction: *mut AnyObject = msg_send![
                checker,
                correctionForWordRange: range,
                inString: &*string,
                language: &*language,
                inSpellDocumentWithTag: 0isize
            ];
            let mut suggestions = Vec::new();
            let recommended = !correction.is_null();
            if recommended {
                suggestions.push(string_of(correction));
            }
            for guess in strings(guesses) {
                if suggestions.len() < max && !suggestions.contains(&guess) {
                    suggestions.push(guess);
                }
            }
            suggestions.truncate(max);
            mistakes.push(Mistake {
                start: range.location as u32,
                length: range.length as u32,
                suggestions,
                recommended,
            });
            at = range.location + range.length;
        }
        Ok(Some(mistakes))
    }

    impl SpellChecker for MacSpelling {
        fn check(
            &mut self,
            language_tag: &str,
            text: &str,
            max_suggestions: usize,
        ) -> Result<Option<Vec<Mistake>>, String> {
            let (answer, answered) = channel();
            let (tag, text) = (language_tag.to_string(), text.to_string());
            on_main(move || {
                let result =
                    autoreleasepool(|_| unsafe { check_on_main(&tag, &text, max_suggestions) });
                let _ = answer.send(result);
            });
            answered
                .recv_timeout(CHECK)
                .unwrap_or_else(|_| Err("the Mac's spell checker did not answer".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_rate_and_pitch_become_av_speechs() {
        assert_eq!(av_rate(1.0), 0.5);
        assert_eq!(av_rate(4.0), 1.0);
        assert_eq!(av_rate(0.5), 0.25);
        assert_eq!(av_pitch(3.0), 2.0);
        assert_eq!(av_pitch(f32::NAN), 1.0);
        assert!(natural_quality(3) && natural_quality(2) && !natural_quality(1));
    }

    #[test]
    fn fr36_buffers_become_interleaved_pcm() {
        assert_eq!(
            interleave_i16(&[&[1, 2], &[3, 4]]),
            [1, 0, 3, 0, 2, 0, 4, 0]
        );
        assert_eq!(interleave_f32(&[&[1.0, -1.0]]), [0xFF, 0x7F, 0x01, 0x80]);
        assert!(level(&[0.5, -0.5]) > SPEECH_LEVEL);
        assert!(level(&[0.0; 4]) < SPEECH_LEVEL);
    }

    #[test]
    fn fr36_the_spell_checkers_language_is_found_for_a_tag() {
        let available: Vec<String> = ["en", "en_GB", "de", "fr_CA"].map(String::from).to_vec();
        assert_eq!(spell_language(&available, "en-GB"), Some("en_GB".into()));
        assert_eq!(spell_language(&available, "en-US"), Some("en".into()));
        assert_eq!(spell_language(&available, "fr-FR"), Some("fr_CA".into()));
        assert_eq!(spell_language(&available, "ko-KR"), None);
    }

    #[test]
    fn fr26_a_refused_speech_recognition_is_an_error_with_where_to_allow_it() {
        assert!(speech_authorization(3).is_ok());
        assert!(speech_authorization(1)
            .unwrap_err()
            .contains("Privacy & Security"));
        assert_eq!(
            recognition_failure("kAFAssistantErrorDomain", 1110),
            RecognitionFailure::SpeechTimeout
        );
        assert_eq!(
            recognition_failure("NSURLErrorDomain", -1009),
            RecognitionFailure::Network
        );
    }
}

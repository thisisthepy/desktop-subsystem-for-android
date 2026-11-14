//! The user's session's side of speech and spelling: the guest's requests, as the service
//! hands them over the local API, answered with the platform's speech synthesis, speech
//! recognition and spell checker.
//!
//! Each of the three runs on a thread of its own ([`LanguageServer`]), so a long sentence
//! being spoken holds up no spelling, and a platform service is made and used on one thread,
//! as COM and the Objective-C runtime want. What they answer is gathered in one queue, which
//! [`serve`] sends to the service. A text is spoken a sentence at a time, so its first sound
//! reaches the guest while the host still speaks the rest, and a stop takes effect at once.
//! When the connection to the service ends, everything under way is stopped and dropped.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dsa_protocol::control::{
    self, envelope::Body, RecognitionEventKind, SpeechOutcome, SpellingMistake, SpellingText,
};
use dsa_transport::Connector;

use super::text::{speech_pieces, utf16_len};
use super::{
    cut, Heard, Listen, Listening, Mistake, Pcm, RecognitionFailure, SpeakError, Speech,
    SpeechRecognizer, SpeechSynthesizer, SpellChecker, DEFAULT_SUGGESTIONS, MAX_ALTERNATIVES,
    MAX_PCM_PIECE, MAX_SPEAK_CHARS, MAX_SPELLING_CHARS, MAX_SPELLING_TEXTS, MAX_SUGGESTIONS,
    MAX_VOICES, PITCH_RANGE, RATE_RANGE,
};
use crate::host_api::HostApiClient;

/// Where the server writes what it has to say: one line each.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Makes one of the platform's services, on the thread that then uses it.
pub type Make<T> = Box<dyn FnOnce() -> Box<T> + Send>;

/// The platform's three services, each made on its own thread.
pub struct Backends {
    pub speech: Make<dyn SpeechSynthesizer>,
    pub recognition: Make<dyn SpeechRecognizer>,
    pub spelling: Make<dyn SpellChecker>,
}

/// One job for a worker's thread.
type Job<T> = Box<dyn FnOnce(&mut T) + Send>;

/// A thread that owns `T` and runs what it is given on it, one job at a time.
struct Worker<T: 'static> {
    jobs: Option<Sender<Job<T>>>,
    thread: Option<JoinHandle<()>>,
}

impl<T: 'static> Worker<T> {
    fn start(name: &str, make: Box<dyn FnOnce() -> T + Send>, log: Log) -> Worker<T> {
        let (tx, rx) = mpsc::channel::<Job<T>>();
        let what = name.to_string();
        let thread_log = Arc::clone(&log);
        let thread = thread::Builder::new().name(name.into()).spawn(move || {
            let Ok(mut it) = catch_unwind(AssertUnwindSafe(make)) else {
                thread_log(&format!("{what}: the host's service could not be made"));
                return;
            };
            while let Ok(job) = rx.recv() {
                // A platform service that panics fails its request, not the program.
                if catch_unwind(AssertUnwindSafe(|| job(&mut it))).is_err() {
                    thread_log(&format!("{what}: the host's service failed a request"));
                }
            }
        });
        match thread {
            Ok(thread) => Worker {
                jobs: Some(tx),
                thread: Some(thread),
            },
            Err(error) => {
                log(&format!("{name}: could not start its thread: {error}"));
                Worker {
                    jobs: None,
                    thread: None,
                }
            }
        }
    }

    /// Runs `job` on the worker's thread after the jobs before it. False when the thread has
    /// gone, and `job` will not run.
    fn run(&self, job: impl FnOnce(&mut T) + Send + 'static) -> bool {
        match &self.jobs {
            Some(jobs) => jobs.send(Box::new(job)).is_ok(),
            None => false,
        }
    }
}

impl<T: 'static> Drop for Worker<T> {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The recognition worker's state: the platform's recognizer and the recognition it runs.
struct Recognition {
    recognizer: Box<dyn SpeechRecognizer>,
    active: Option<Active>,
}

struct Active {
    request_id: u32,
    listening: Box<dyn Listening>,
    /// Set once the recognition is over, by its end or a cancel; nothing more is sent.
    over: Arc<AtomicBool>,
}

/// Answers the guest's speech and spelling requests with the platform's services.
pub struct LanguageServer {
    out_tx: Sender<control::Envelope>,
    out: Receiver<control::Envelope>,
    /// The stop of each text being spoken or waiting to be.
    speaking: Arc<Mutex<HashMap<u32, Arc<AtomicBool>>>>,
    speech: Worker<Box<dyn SpeechSynthesizer>>,
    recognition: Worker<Recognition>,
    spelling: Worker<Box<dyn SpellChecker>>,
}

fn send(out: &Sender<control::Envelope>, body: Body) {
    let _ = out.send(control::Envelope { body: Some(body) });
}

fn done(out: &Sender<control::Envelope>, id: u32, outcome: SpeechOutcome, message: String) {
    send(
        out,
        Body::SpeechDone(control::SpeechDone {
            request_id: id,
            outcome: outcome as i32,
            message: cut(&message, 1024),
        }),
    );
}

fn clamp(value: f32, (low, high): (f32, f32)) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(low, high)
    } else {
        1.0
    }
}

/// The guest's message for what a recognition heard, or `None` for what is not passed on.
fn heard_event(
    request_id: u32,
    listen: &Listen,
    heard: Heard,
) -> Option<control::RecognitionEvent> {
    let event = |kind: RecognitionEventKind| control::RecognitionEvent {
        request_id,
        kind: kind as i32,
        ..Default::default()
    };
    Some(match heard {
        Heard::Ready => event(RecognitionEventKind::Ready),
        Heard::SpeechBegan => event(RecognitionEventKind::SpeechBegan),
        Heard::SpeechEnded => event(RecognitionEventKind::SpeechEnded),
        Heard::Partial(text) => {
            if !listen.partial_results || text.trim().is_empty() {
                return None;
            }
            control::RecognitionEvent {
                texts: vec![cut(&text, 4096)],
                ..event(RecognitionEventKind::Partial)
            }
        }
        Heard::Final(alternatives) => {
            let alternatives: Vec<_> = alternatives
                .into_iter()
                .filter(|(text, _)| !text.trim().is_empty())
                .take(listen.max_results)
                .collect();
            if alternatives.is_empty() {
                return Some(failure_event(
                    request_id,
                    RecognitionFailure::NoMatch,
                    "nothing that was said was recognised".into(),
                ));
            }
            let confidences = if alternatives.iter().all(|(_, c)| c.is_some()) {
                alternatives
                    .iter()
                    .map(|(_, c)| c.unwrap_or(0.0).clamp(0.0, 1.0))
                    .collect()
            } else {
                Vec::new()
            };
            control::RecognitionEvent {
                texts: alternatives
                    .into_iter()
                    .map(|(t, _)| cut(&t, 4096))
                    .collect(),
                confidences,
                ..event(RecognitionEventKind::Final)
            }
        }
        Heard::Failed(failure, message) => failure_event(request_id, failure, message),
    })
}

fn failure_event(
    request_id: u32,
    failure: RecognitionFailure,
    message: String,
) -> control::RecognitionEvent {
    control::RecognitionEvent {
        request_id,
        kind: RecognitionEventKind::Error as i32,
        error: failure.to_control() as i32,
        message: cut(&message, 1024),
        ..Default::default()
    }
}

/// The guest's message for the mistakes in a text of `length` UTF-16 code units: those that
/// lie inside it, in order, with their suggestions cut to `max`.
fn mistakes_of(found: Vec<Mistake>, length: usize, max: usize) -> SpellingText {
    let mut mistakes: Vec<SpellingMistake> = found
        .into_iter()
        .filter(|m| m.length > 0 && (m.start as usize).saturating_add(m.length as usize) <= length)
        .map(|m| {
            let mut suggestions: Vec<String> = Vec::new();
            for suggestion in m.suggestions {
                let suggestion = cut(&suggestion, 256);
                if suggestions.len() < max && !suggestions.contains(&suggestion) {
                    suggestions.push(suggestion);
                }
            }
            SpellingMistake {
                start: m.start,
                length: m.length,
                recommended: m.recommended && !suggestions.is_empty(),
                suggestions,
            }
        })
        .collect();
    mistakes.sort_by_key(|m| m.start);
    mistakes.dedup_by_key(|m| m.start);
    SpellingText { mistakes }
}

impl LanguageServer {
    /// Starts the three services' threads.
    pub fn start(backends: Backends, log: Log) -> LanguageServer {
        let (out_tx, out) = mpsc::channel();
        let Backends {
            speech,
            recognition,
            spelling,
        } = backends;
        LanguageServer {
            out_tx,
            out,
            speaking: Arc::default(),
            speech: Worker::start("dsa-speech", speech, Arc::clone(&log)),
            recognition: Worker::start(
                "dsa-recognition",
                Box::new(move || Recognition {
                    recognizer: recognition(),
                    active: None,
                }),
                Arc::clone(&log),
            ),
            spelling: Worker::start("dsa-spelling", spelling, log),
        }
    }

    /// What the services answered since the last call, waiting at most `timeout` for the
    /// first.
    pub fn answers(&self, timeout: Duration) -> Vec<control::Envelope> {
        let mut answers = Vec::new();
        let first = if timeout.is_zero() {
            self.out.try_recv().ok()
        } else {
            self.out.recv_timeout(timeout).ok()
        };
        if let Some(first) = first {
            answers.push(first);
            while let Ok(next) = self.out.try_recv() {
                answers.push(next);
            }
        }
        answers
    }

    /// Lists the host's voices for the guest, as if it had asked.
    pub fn announce_voices(&self) {
        self.handle(control::Envelope {
            body: Some(Body::ListSpeechVoices(control::ListSpeechVoices {})),
        });
    }

    /// Acts on one request of the guest's. One that is no request is dropped.
    pub fn handle(&self, envelope: control::Envelope) {
        match envelope.body {
            Some(Body::ListSpeechVoices(_)) => {
                let out = self.out_tx.clone();
                let ran = self.speech.run(move |speech| {
                    let answer = match speech.voices() {
                        Ok(mut voices) => {
                            voices.voices.truncate(MAX_VOICES);
                            control::SpeechVoices {
                                voices: voices
                                    .voices
                                    .into_iter()
                                    .map(|v| control::SpeechVoice {
                                        id: v.id,
                                        name: v.name,
                                        language_tag: v.language_tag,
                                        natural: v.natural,
                                        needs_network: v.needs_network,
                                    })
                                    .collect(),
                                default_voice_id: voices.default_id,
                                available: true,
                            }
                        }
                        Err(_) => control::SpeechVoices::default(),
                    };
                    send(&out, Body::SpeechVoices(answer));
                });
                if !ran {
                    send(
                        &self.out_tx,
                        Body::SpeechVoices(control::SpeechVoices::default()),
                    );
                }
            }
            Some(Body::SpeakText(speak)) => self.speak(speak),
            Some(Body::StopSpeaking(stop)) => {
                if let Some(flag) = self.speaking().get(&stop.request_id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            Some(Body::StartRecognition(start)) => self.start_recognition(start),
            Some(Body::StopRecognition(stop)) => {
                let id = stop.request_id;
                self.recognition.run(move |r| {
                    if let Some(active) = r.active.as_mut().filter(|a| a.request_id == id) {
                        active.listening.stop();
                    }
                });
            }
            Some(Body::CancelRecognition(cancel)) => {
                let id = cancel.request_id;
                self.recognition.run(move |r| {
                    if r.active.as_ref().is_some_and(|a| a.request_id == id) {
                        if let Some(mut active) = r.active.take() {
                            active.over.store(true, Ordering::Relaxed);
                            active.listening.cancel();
                        }
                    }
                });
            }
            Some(Body::CheckSpelling(check)) => self.check_spelling(check),
            _ => {}
        }
    }

    fn speaking(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Arc<AtomicBool>>> {
        self.speaking.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn speak(&self, speak: control::SpeakText) {
        let id = speak.request_id;
        let stop = Arc::new(AtomicBool::new(false));
        // A number used again stops the request it named.
        if let Some(old) = self.speaking().insert(id, Arc::clone(&stop)) {
            old.store(true, Ordering::Relaxed);
        }
        let out = self.out_tx.clone();
        let speaking = Arc::clone(&self.speaking);
        let ran = self.speech.run(move |speech| {
            let text = cut(&speak.text, MAX_SPEAK_CHARS);
            let mut format: Option<(u32, u32)> = None;
            let mut outcome = Ok(());
            for piece in speech_pieces(&text) {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let request = Speech {
                    text: piece,
                    voice_id: speak.voice_id.clone(),
                    language_tag: speak.language_tag.clone(),
                    rate: clamp(speak.rate, RATE_RANGE),
                    pitch: clamp(speak.pitch, PITCH_RANGE),
                };
                let mut mismatch = None;
                let mut sound = |pcm: Pcm| {
                    if stop.load(Ordering::Relaxed) || mismatch.is_some() {
                        return;
                    }
                    let this = (pcm.sample_rate_hz, pcm.channels);
                    match format {
                        None => format = Some(this),
                        Some(first) if first != this => {
                            mismatch = Some(format!(
                                "the voice changed its sound from {} Hz on {} channels to {} Hz \
                                 on {} channels",
                                first.0, first.1, this.0, this.1
                            ));
                            return;
                        }
                        Some(_) => {}
                    }
                    let frame = (pcm.channels.max(1) * 2) as usize;
                    let piece = MAX_PCM_PIECE / frame * frame;
                    for chunk in pcm.data.chunks(piece) {
                        let whole = chunk.len() / frame * frame;
                        if whole == 0 {
                            continue;
                        }
                        send(
                            &out,
                            Body::SpeechAudio(control::SpeechAudio {
                                request_id: id,
                                sample_rate_hz: pcm.sample_rate_hz,
                                channel_count: pcm.channels,
                                pcm: chunk[..whole].to_vec(),
                            }),
                        );
                    }
                };
                let spoken = speech.speak(&request, &stop, &mut sound);
                if let Some(why) = mismatch {
                    outcome = Err(SpeakError::Failed(why));
                    break;
                }
                if let Err(error) = spoken {
                    outcome = Err(error);
                    break;
                }
            }
            let mut speaking = speaking.lock().unwrap_or_else(|e| e.into_inner());
            if speaking
                .get(&id)
                .is_some_and(|flag| Arc::ptr_eq(flag, &stop))
            {
                speaking.remove(&id);
            }
            drop(speaking);
            match outcome {
                _ if stop.load(Ordering::Relaxed) => {
                    done(&out, id, SpeechOutcome::Stopped, String::new())
                }
                Ok(()) => done(&out, id, SpeechOutcome::Done, String::new()),
                Err(SpeakError::Unavailable(why)) => {
                    done(&out, id, SpeechOutcome::Unavailable, why)
                }
                Err(SpeakError::Failed(why)) => done(&out, id, SpeechOutcome::Failed, why),
            }
        });
        if !ran {
            self.speaking().remove(&id);
            done(
                &self.out_tx,
                id,
                SpeechOutcome::Unavailable,
                "the host's speech synthesis is not running".into(),
            );
        }
    }

    fn start_recognition(&self, start: control::StartRecognition) {
        let id = start.request_id;
        let out = self.out_tx.clone();
        let listen = Listen {
            language_tag: cut(&start.language_tag, 64),
            partial_results: start.partial_results,
            max_results: (start.max_results as usize).clamp(1, MAX_ALTERNATIVES),
            prefer_offline: start.prefer_offline,
        };
        let ran = self.recognition.run(move |r| {
            if let Some(active) = &r.active {
                if !active.over.load(Ordering::Relaxed) {
                    let why = "the host is already listening for another app".to_string();
                    send(
                        &out,
                        Body::RecognitionEvent(failure_event(id, RecognitionFailure::Busy, why)),
                    );
                    return;
                }
            }
            r.active = None;
            let over = Arc::new(AtomicBool::new(false));
            let sink_over = Arc::clone(&over);
            let sink_out = Mutex::new(out.clone());
            let sink_listen = listen.clone();
            let sink = Box::new(move |heard: Heard| {
                if sink_over.load(Ordering::Relaxed) {
                    return;
                }
                let Some(event) = heard_event(id, &sink_listen, heard) else {
                    return;
                };
                let ends = event.kind == RecognitionEventKind::Final as i32
                    || event.kind == RecognitionEventKind::Error as i32;
                if ends {
                    sink_over.store(true, Ordering::Relaxed);
                }
                let out = sink_out.lock().unwrap_or_else(|e| e.into_inner());
                send(&out, Body::RecognitionEvent(event));
            });
            match r.recognizer.start(&listen, sink) {
                Ok(listening) => {
                    r.active = Some(Active {
                        request_id: id,
                        listening,
                        over,
                    })
                }
                Err((failure, why)) => {
                    if !over.swap(true, Ordering::Relaxed) {
                        send(
                            &out,
                            Body::RecognitionEvent(failure_event(id, failure, why)),
                        );
                    }
                }
            }
        });
        if !ran {
            send(
                &self.out_tx,
                Body::RecognitionEvent(failure_event(
                    id,
                    RecognitionFailure::Unavailable,
                    "the host's speech recognition is not running".into(),
                )),
            );
        }
    }

    fn check_spelling(&self, check: control::CheckSpelling) {
        let id = check.request_id;
        let out = self.out_tx.clone();
        let ran = self.spelling.run(move |checker| {
            let max = match check.max_suggestions as usize {
                0 => DEFAULT_SUGGESTIONS,
                n => n.min(MAX_SUGGESTIONS),
            };
            let mut texts = Vec::new();
            let mut supported = true;
            for (i, text) in check.texts.iter().enumerate() {
                if i >= MAX_SPELLING_TEXTS {
                    texts.push(SpellingText::default());
                    continue;
                }
                let text = cut(text, MAX_SPELLING_CHARS);
                match checker.check(&check.language_tag, &text, max) {
                    Ok(Some(found)) => texts.push(mistakes_of(found, utf16_len(&text), max)),
                    Ok(None) => {
                        supported = false;
                        break;
                    }
                    // A text the checker fails on has no mistakes the host can name.
                    Err(_) => texts.push(SpellingText::default()),
                }
            }
            if check.texts.is_empty() {
                // Asks whether the language is checked at all.
                supported = matches!(checker.check(&check.language_tag, "", max), Ok(Some(_)));
            }
            if !supported {
                texts.clear();
            }
            send(
                &out,
                Body::SpellingResult(control::SpellingResult {
                    request_id: id,
                    texts,
                    supported,
                }),
            );
        });
        if !ran {
            send(
                &self.out_tx,
                Body::SpellingResult(control::SpellingResult {
                    request_id: id,
                    texts: Vec::new(),
                    supported: false,
                }),
            );
        }
    }

    /// The connection to the service ended: everything under way stops, and what was not
    /// sent yet is dropped.
    pub fn reset(&self) {
        for flag in self.speaking().values() {
            flag.store(true, Ordering::Relaxed);
        }
        self.recognition.run(|r| {
            if let Some(mut active) = r.active.take() {
                active.over.store(true, Ordering::Relaxed);
                active.listening.cancel();
            }
        });
        while self.out.try_recv().is_ok() {}
    }
}

/// How long the client waits for the service each time it connects.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the client waits for a request before it sends answers again.
const POLL: Duration = Duration::from_millis(10);
/// How long the client waits before it connects again.
const RETRY: Duration = Duration::from_secs(2);

/// Serves the speech and spelling of the caller's Android through the local API until `stop`
/// is set, with the platform's services in `backends`. A lost connection stops what was under
/// way and is made again.
pub fn serve(
    connector: &mut dyn Connector,
    build: &str,
    backends: Backends,
    stop: &AtomicBool,
    log: Log,
) {
    let server = LanguageServer::start(backends, Arc::clone(&log));
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(connector, build, CONNECT_TIMEOUT)
            .and_then(|client| client.serve_language_services());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => {
                if !warned {
                    log(&format!(
                        "speech and spelling: cannot serve Android yet: {why}"
                    ));
                    warned = true;
                }
                wait(stop, RETRY);
                continue;
            }
        };
        warned = false;
        log("speech and spelling: serving Android's voices, speech recognition and spell checker");
        // An Android that asked before this client was there was told there are no voices.
        server.announce_voices();
        'connected: while !stop.load(Ordering::Relaxed) {
            match stream.next(POLL) {
                Ok(Some(request)) => server.handle(request),
                Ok(None) => {}
                Err(why) => {
                    log(&format!(
                        "speech and spelling: the service connection ended: {why}"
                    ));
                    break 'connected;
                }
            }
            for answer in server.answers(Duration::ZERO) {
                if let Err(why) = stream.send(&answer) {
                    log(&format!(
                        "speech and spelling: the service connection ended: {why}"
                    ));
                    break 'connected;
                }
            }
        }
        server.reset();
        wait(stop, RETRY);
    }
    server.reset();
}

/// Sleeps `time`, or less once `stop` is set.
fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        thread::sleep(step);
        waited += step;
    }
}

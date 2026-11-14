//! Speech and spelling for the guest from the host: the guest's requests reach a client in
//! the user's session over the local API (the in-memory transport), which answers them with
//! fake host services, and the answers come back to the guest the same way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::{PerUser, UserAndroid};
use dsa_host_core::host_api_conformance::RunningService;
use dsa_host_core::ime::ImeEvent;
use dsa_host_core::input::PointerEvent;
use dsa_host_core::keys::KeyEvent;
use dsa_host_core::language::relay::{Relay, Served};
use dsa_host_core::language::server::{self, Backends, Log};
use dsa_host_core::language::{
    Heard, HeardSink, Listen, Listening, Mistake, Pcm, RecognitionFailure, SpeakError, Speech,
    SpeechRecognizer, SpeechSynthesizer, SpellChecker, Voice, Voices,
};
use dsa_host_core::session::SessionState;
use dsa_host_core::tasks::TaskRequestError;
use dsa_protocol::control::{
    self, envelope::Body, RecognitionError, RecognitionEventKind, SpeechOutcome,
};
use dsa_transport::local;

const WAIT: Duration = Duration::from_secs(10);

/// The sound the fake voice makes for each piece of text: two samples.
const SOUND: [u8; 4] = [1, 0, 2, 0];

/// What the fake host services were asked.
#[derive(Default)]
struct Asked {
    spoken: Vec<Speech>,
    listened: Vec<Listen>,
    canceled: usize,
    checked: Vec<(String, String)>,
}

#[derive(Clone, Default)]
struct Host {
    asked: Arc<Mutex<Asked>>,
    /// Set to refuse recognition, as a host whose user turned the microphone off does.
    microphone_blocked: bool,
}

impl SpeechSynthesizer for Host {
    fn voices(&mut self) -> Result<Voices, String> {
        Ok(Voices {
            voices: vec![
                Voice {
                    id: "aria".into(),
                    name: "Microsoft Aria (Natural)".into(),
                    language_tag: "en-US".into(),
                    natural: true,
                    needs_network: false,
                },
                Voice {
                    id: "heami".into(),
                    name: "Microsoft Heami".into(),
                    language_tag: "ko-KR".into(),
                    natural: false,
                    needs_network: false,
                },
            ],
            default_id: "aria".into(),
        })
    }

    fn speak(
        &mut self,
        speech: &Speech,
        _: &AtomicBool,
        sound: &mut dyn FnMut(Pcm),
    ) -> Result<(), SpeakError> {
        if speech.voice_id == "missing" {
            return Err(SpeakError::Unavailable("no such voice".into()));
        }
        self.asked.lock().unwrap().spoken.push(speech.clone());
        sound(Pcm {
            sample_rate_hz: 24000,
            channels: 1,
            data: SOUND.to_vec(),
        });
        Ok(())
    }
}

struct FakeListening(Arc<Mutex<Asked>>);

impl Listening for FakeListening {
    fn stop(&mut self) {}
    fn cancel(&mut self) {
        self.0.lock().unwrap().canceled += 1;
    }
}

impl SpeechRecognizer for Host {
    fn start(
        &mut self,
        listen: &Listen,
        heard: HeardSink,
    ) -> Result<Box<dyn Listening>, (RecognitionFailure, String)> {
        if self.microphone_blocked {
            return Err((
                RecognitionFailure::Denied,
                "the microphone is off in the privacy settings".into(),
            ));
        }
        self.asked.lock().unwrap().listened.push(listen.clone());
        heard(Heard::Ready);
        heard(Heard::SpeechBegan);
        heard(Heard::Partial("hello".into()));
        heard(Heard::SpeechEnded);
        heard(Heard::Final(vec![
            ("hello world".into(), Some(0.9)),
            ("yellow world".into(), Some(0.4)),
            ("hello word".into(), Some(0.2)),
        ]));
        // Nothing after the end reaches the guest.
        heard(Heard::Partial("late".into()));
        Ok(Box::new(FakeListening(Arc::clone(&self.asked))))
    }
}

impl SpellChecker for Host {
    fn check(
        &mut self,
        language_tag: &str,
        text: &str,
        _: usize,
    ) -> Result<Option<Vec<Mistake>>, String> {
        if !language_tag.starts_with("en") {
            return Ok(None);
        }
        self.asked
            .lock()
            .unwrap()
            .checked
            .push((language_tag.into(), text.into()));
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let teh: Vec<u16> = "teh".encode_utf16().collect();
        let mistakes = utf16
            .windows(3)
            .enumerate()
            .filter(|(_, w)| *w == teh.as_slice())
            .map(|(at, _)| Mistake {
                start: at as u32,
                length: 3,
                suggestions: vec!["the".into(), "tech".into(), "the".into()],
                recommended: true,
            })
            .collect();
        Ok(Some(mistakes))
    }
}

/// The guest's side, as the session would be: the relay, and what reached the guest.
#[derive(Default)]
struct Guest {
    relay: Mutex<Relay>,
    received: Mutex<Vec<Body>>,
}

impl Guest {
    /// The guest's agent sends `body`.
    fn ask(&self, body: Body) {
        let replies = self.relay.lock().unwrap().from_guest(body);
        self.received.lock().unwrap().extend(replies);
    }

    /// Waits until something that `wanted` matches reached the guest, and returns all that
    /// reached it so far, which it then forgets.
    fn until(&self, wanted: impl Fn(&Body) -> bool) -> Vec<Body> {
        let deadline = Instant::now() + WAIT;
        loop {
            {
                let mut received = self.received.lock().unwrap();
                if received.iter().any(&wanted) {
                    return std::mem::take(&mut *received);
                }
            }
            assert!(
                Instant::now() < deadline,
                "the guest was not answered in time"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn served(&self) -> bool {
        self.relay.lock().unwrap().served()
    }
}

impl UserAndroid for Guest {
    fn state(&self) -> SessionState {
        SessionState::Active
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {}
    fn stop(&self) {}
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<Receiver<String>> {
        None
    }
    fn serve_language(&self) -> Option<Served> {
        Some(self.relay.lock().unwrap().serve())
    }
    fn language_answer(&self, server: u64, envelope: control::Envelope) -> Result<(), String> {
        let answer = self
            .relay
            .lock()
            .unwrap()
            .answer(server, envelope.body.ok_or("empty")?)?;
        self.received.lock().unwrap().extend(answer);
        Ok(())
    }
    fn language_server_gone(&self, server: u64) {
        let failures = self.relay.lock().unwrap().server_gone(server);
        self.received.lock().unwrap().extend(failures);
    }
}

/// A host session serving `guest` with `host`'s services, until the returned stop is set.
struct Serving {
    /// What the client told the guest of the host's voices as it started.
    announced: Vec<Body>,
    stop: Arc<AtomicBool>,
    client: Option<thread::JoinHandle<()>>,
    _service: RunningService,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(client) = self.client.take() {
            let _ = client.join();
        }
    }
}

fn serve(guest: &Arc<Guest>, host: Host) -> Serving {
    let (listener, mut connector) = local::pair();
    let android = Arc::clone(guest);
    let androids = Arc::new(PerUser::new(move |_: &str| {
        Ok(Arc::clone(&android) as Arc<dyn UserAndroid>)
    }));
    let service = RunningService::start(Box::new(listener), androids);
    let stop = Arc::new(AtomicBool::new(false));
    let client = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let (speech, recognition, spelling) = (host.clone(), host.clone(), host);
            let backends = Backends {
                speech: Box::new(move || Box::new(speech) as Box<dyn SpeechSynthesizer>),
                recognition: Box::new(move || Box::new(recognition) as Box<dyn SpeechRecognizer>),
                spelling: Box::new(move || Box::new(spelling) as Box<dyn SpellChecker>),
            };
            let log: Log = Arc::new(|_: &str| {});
            server::serve(&mut connector, "test", backends, &stop, log);
        })
    };
    let deadline = Instant::now() + WAIT;
    while !guest.served() {
        assert!(
            Instant::now() < deadline,
            "the client did not start serving"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let announced = guest.until(|b| matches!(b, Body::SpeechVoices(_)));
    Serving {
        announced,
        stop,
        client: Some(client),
        _service: service,
    }
}

fn speak(id: u32, text: &str, voice: &str) -> Body {
    Body::SpeakText(control::SpeakText {
        request_id: id,
        text: text.into(),
        voice_id: voice.into(),
        language_tag: "en-US".into(),
        rate: 1.5,
        pitch: 0.8,
    })
}

#[test]
fn fr36_tts_speaks_through_the_host() {
    let guest = Arc::new(Guest::default());
    let host = Host::default();
    let serving = serve(&guest, host.clone());

    // The client lists the host's voices as it starts, and again when asked.
    let announced = serving.announced.clone();
    guest.ask(Body::ListSpeechVoices(control::ListSpeechVoices {}));
    let listed = guest.until(|b| matches!(b, Body::SpeechVoices(_)));
    for received in [announced, listed] {
        let Some(Body::SpeechVoices(voices)) = received.into_iter().next() else {
            panic!("no voices");
        };
        assert!(voices.available);
        assert_eq!(voices.default_voice_id, "aria");
        let names: Vec<_> = voices
            .voices
            .iter()
            .map(|v| (v.id.as_str(), v.language_tag.as_str(), v.natural))
            .collect();
        assert_eq!(names, [("aria", "en-US", true), ("heami", "ko-KR", false)]);
    }

    let text = "This sentence is long enough to be spoken on its own, alone. \
                And so is this second one, which follows the first.";
    guest.ask(speak(3, text, "aria"));
    let answers = guest.until(|b| matches!(b, Body::SpeechDone(_)));
    let mut sound = Vec::new();
    for answer in &answers {
        if let Body::SpeechAudio(audio) = answer {
            assert_eq!(audio.request_id, 3);
            assert_eq!((audio.sample_rate_hz, audio.channel_count), (24000, 1));
            sound.extend_from_slice(&audio.pcm);
        }
    }
    assert_eq!(
        sound,
        [SOUND, SOUND].concat(),
        "the sound of both sentences"
    );
    match answers.last() {
        Some(Body::SpeechDone(done)) => {
            assert_eq!(done.request_id, 3);
            assert_eq!(done.outcome, SpeechOutcome::Done as i32);
        }
        other => panic!("expected the end of the speech, got {other:?}"),
    }
    // The same text, a sentence at a time, with the same voice.
    let asked = host.asked.lock().unwrap();
    let spoken: Vec<_> = asked.spoken.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        spoken,
        [
            "This sentence is long enough to be spoken on its own, alone.",
            "And so is this second one, which follows the first.",
        ]
    );
    assert!(asked.spoken.iter().all(|s| s.voice_id == "aria"
        && s.language_tag == "en-US"
        && s.rate == 1.5
        && s.pitch == 0.8));
}

#[test]
fn fr36_a_voice_the_host_lacks_ends_the_speech_as_unavailable() {
    let guest = Arc::new(Guest::default());
    let _serving = serve(&guest, Host::default());
    guest.ask(speak(4, "Hello.", "missing"));
    let answers = guest.until(|b| matches!(b, Body::SpeechDone(_)));
    match answers.iter().find(|b| matches!(b, Body::SpeechDone(_))) {
        Some(Body::SpeechDone(done)) => {
            assert_eq!(done.outcome, SpeechOutcome::Unavailable as i32);
            assert_eq!(done.message, "no such voice");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr36_speech_recognition_comes_from_the_host() {
    let guest = Arc::new(Guest::default());
    let host = Host::default();
    let _serving = serve(&guest, host.clone());
    guest.ask(Body::StartRecognition(control::StartRecognition {
        request_id: 9,
        language_tag: "en-US".into(),
        partial_results: true,
        max_results: 2,
        prefer_offline: true,
    }));
    let answers = guest.until(
        |b| matches!(b, Body::RecognitionEvent(e) if e.kind == RecognitionEventKind::Final as i32),
    );
    let events: Vec<_> = answers
        .into_iter()
        .filter_map(|b| match b {
            Body::RecognitionEvent(e) => Some(e),
            _ => None,
        })
        .collect();
    let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            RecognitionEventKind::Ready as i32,
            RecognitionEventKind::SpeechBegan as i32,
            RecognitionEventKind::Partial as i32,
            RecognitionEventKind::SpeechEnded as i32,
            RecognitionEventKind::Final as i32,
        ]
    );
    assert!(events.iter().all(|e| e.request_id == 9));
    assert_eq!(events[2].texts, ["hello"]);
    let last = events.last().unwrap();
    assert_eq!(
        last.texts,
        ["hello world", "yellow world"],
        "at most two, best first"
    );
    assert_eq!(last.confidences, [0.9, 0.4]);
    let asked = host.asked.lock().unwrap();
    assert_eq!(asked.listened.len(), 1);
    assert_eq!(asked.listened[0].language_tag, "en-US");
    assert!(asked.listened[0].prefer_offline);
    drop(asked);
    // Nothing reaches the guest after the end.
    thread::sleep(Duration::from_millis(100));
    assert!(guest.received.lock().unwrap().is_empty());
}

#[test]
fn fr36_speech_recognition_with_the_microphone_blocked_ends_with_an_error() {
    let guest = Arc::new(Guest::default());
    let host = Host {
        microphone_blocked: true,
        ..Host::default()
    };
    let _serving = serve(&guest, host);
    guest.ask(Body::StartRecognition(control::StartRecognition {
        request_id: 2,
        ..Default::default()
    }));
    let answers = guest.until(|b| matches!(b, Body::RecognitionEvent(_)));
    match answers.as_slice() {
        [Body::RecognitionEvent(event)] => {
            assert_eq!(event.request_id, 2);
            assert_eq!(event.kind, RecognitionEventKind::Error as i32);
            assert_eq!(event.error, RecognitionError::Denied as i32);
            assert!(event.message.contains("privacy"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr36_spell_checking_comes_from_the_host() {
    let guest = Arc::new(Guest::default());
    let host = Host::default();
    let _serving = serve(&guest, host.clone());
    guest.ask(Body::CheckSpelling(control::CheckSpelling {
        request_id: 5,
        language_tag: "en-US".into(),
        texts: vec!["I saw teh cat.".into(), "All good here.".into()],
        max_suggestions: 1,
    }));
    let answers = guest.until(|b| matches!(b, Body::SpellingResult(_)));
    let Some(Body::SpellingResult(result)) = answers.into_iter().next() else {
        panic!("no result");
    };
    assert_eq!(result.request_id, 5);
    assert!(result.supported);
    assert_eq!(result.texts.len(), 2, "one result for each text");
    let mistakes = &result.texts[0].mistakes;
    assert_eq!(mistakes.len(), 1);
    assert_eq!((mistakes[0].start, mistakes[0].length), (6, 3));
    assert_eq!(mistakes[0].suggestions, ["the"], "at most as many as asked");
    assert!(mistakes[0].recommended);
    assert!(result.texts[1].mistakes.is_empty());
    assert_eq!(host.asked.lock().unwrap().checked.len(), 2);
}

#[test]
fn fr36_a_language_the_host_cannot_check_is_unsupported() {
    let guest = Arc::new(Guest::default());
    let _serving = serve(&guest, Host::default());
    guest.ask(Body::CheckSpelling(control::CheckSpelling {
        request_id: 6,
        language_tag: "ko-KR".into(),
        texts: vec!["안녕하세요".into()],
        max_suggestions: 0,
    }));
    let answers = guest.until(|b| matches!(b, Body::SpellingResult(_)));
    match answers.as_slice() {
        [Body::SpellingResult(result)] => {
            assert!(!result.supported);
            assert!(result.texts.is_empty());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn nfr5_requests_fail_in_the_guest_when_the_host_session_goes_away() {
    let guest = Arc::new(Guest::default());
    let serving = serve(&guest, Host::default());
    drop(serving);
    let deadline = Instant::now() + WAIT;
    loop {
        guest.received.lock().unwrap().clear();
        guest.ask(speak(8, "Hello.", ""));
        let received = guest.received.lock().unwrap().clone();
        if let [Body::SpeechDone(done)] = received.as_slice() {
            assert_eq!(done.outcome, SpeechOutcome::Unavailable as i32);
            break;
        }
        assert!(Instant::now() < deadline, "the guest was not told");
        thread::sleep(Duration::from_millis(20));
    }
}

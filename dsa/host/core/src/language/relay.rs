//! The session's side of speech and spelling: the guest's requests go to the first client in
//! the user's session that serves them, and that client's answers go back to the guest.
//!
//! The relay remembers which client has each request the guest is waiting on, so an answer
//! from a client that does not have the request (late, or made up) is dropped, and a client
//! that goes away fails what it had. A request nobody serves is answered at once with the
//! failure. Nothing a client sends reaches the guest unless it is an answer, of the right
//! size.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use dsa_protocol::control::{
    self, envelope::Body, RecognitionEventKind, SpeechOutcome, SpellingResult,
};

use super::{
    cut, MAX_ALTERNATIVES, MAX_PCM_PIECE, MAX_SPELLING_TEXTS, MAX_SUGGESTIONS, MAX_VOICES,
};

/// What the guest is told when no session of the user's is open to serve it.
const NOBODY: &str = "no desktop session of the user's is open to serve it";
/// What the guest is told when the client that had a request went away.
const GONE: &str = "the desktop session that served it went away";

/// The kinds of request the guest waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Speak,
    Recognize,
    Spell,
}

/// A client's watch of the guest's requests, from [`Relay::serve`].
pub struct Served {
    /// Names the client in [`Relay::answer`] and [`Relay::server_gone`].
    pub id: u64,
    /// The guest's requests for this client, each a whole control message.
    pub requests: Receiver<control::Envelope>,
}

/// Which client serves the guest's speech and spelling, and which of the guest's requests
/// each one has.
#[derive(Default)]
pub struct Relay {
    servers: Vec<(u64, Sender<control::Envelope>)>,
    next_id: u64,
    pending: HashMap<(Kind, u32), u64>,
}

/// The kind and number of a request that waits for an answer, if `body` is one.
fn waits(body: &Body) -> Option<(Kind, u32)> {
    match body {
        Body::SpeakText(speak) => Some((Kind::Speak, speak.request_id)),
        Body::StartRecognition(start) => Some((Kind::Recognize, start.request_id)),
        Body::CheckSpelling(check) => Some((Kind::Spell, check.request_id)),
        _ => None,
    }
}

/// Whether `body` is a request of the guest's that a client serves.
pub fn is_request(body: &Body) -> bool {
    matches!(
        body,
        Body::ListSpeechVoices(_)
            | Body::SpeakText(_)
            | Body::StopSpeaking(_)
            | Body::StartRecognition(_)
            | Body::StopRecognition(_)
            | Body::CancelRecognition(_)
            | Body::CheckSpelling(_)
    )
}

/// Whether `body` is an answer a client gives the guest.
pub fn is_answer(body: &Body) -> bool {
    matches!(
        body,
        Body::SpeechVoices(_)
            | Body::SpeechAudio(_)
            | Body::SpeechDone(_)
            | Body::RecognitionEvent(_)
            | Body::SpellingResult(_)
    )
}

/// The answer that tells the guest its request `kind` `id` failed for `why`.
fn failure(kind: Kind, id: u32, why: &str) -> Body {
    match kind {
        Kind::Speak => Body::SpeechDone(control::SpeechDone {
            request_id: id,
            outcome: SpeechOutcome::Unavailable as i32,
            message: why.to_string(),
        }),
        Kind::Recognize => Body::RecognitionEvent(control::RecognitionEvent {
            request_id: id,
            kind: RecognitionEventKind::Error as i32,
            error: control::RecognitionError::Unavailable as i32,
            message: why.to_string(),
            ..Default::default()
        }),
        Kind::Spell => Body::SpellingResult(SpellingResult {
            request_id: id,
            texts: Vec::new(),
            supported: false,
        }),
    }
}

/// What tells the guest its request `body` cannot be served, if it waits for an answer.
pub fn unavailable(body: &Body) -> Option<Body> {
    if let Body::ListSpeechVoices(_) = body {
        return Some(Body::SpeechVoices(control::SpeechVoices {
            voices: Vec::new(),
            default_voice_id: String::new(),
            available: false,
        }));
    }
    waits(body).map(|(kind, id)| failure(kind, id, NOBODY))
}

impl Relay {
    pub fn new() -> Relay {
        Relay::default()
    }

    /// A new client's watch of the guest's requests. Requests go to the first client still
    /// watching, so this one gets them once those before it have gone.
    pub fn serve(&mut self) -> Served {
        let (tx, rx) = mpsc::channel();
        self.next_id += 1;
        self.servers.push((self.next_id, tx));
        Served {
            id: self.next_id,
            requests: rx,
        }
    }

    /// Whether any client serves the guest now.
    pub fn served(&self) -> bool {
        !self.servers.is_empty()
    }

    /// Hands `envelope` to client `id`; false when it has gone, which is then forgotten.
    fn hand(&mut self, id: u64, envelope: control::Envelope) -> bool {
        let Some(at) = self.servers.iter().position(|(s, _)| *s == id) else {
            return false;
        };
        if self.servers[at].1.send(envelope).is_ok() {
            return true;
        }
        self.servers.remove(at);
        false
    }

    /// Hands `envelope` to the first client still there; its id, or `None` when none is.
    fn hand_first(&mut self, envelope: control::Envelope) -> Option<u64> {
        while let Some((id, tx)) = self.servers.first() {
            let id = *id;
            if tx.send(envelope.clone()).is_ok() {
                return Some(id);
            }
            self.servers.remove(0);
        }
        None
    }

    /// Takes a request of the guest's. Returns what the guest is to be told at once: the
    /// failure, when nobody can serve it.
    pub fn from_guest(&mut self, body: Body) -> Vec<Body> {
        // A stop or a cancel goes to the client that has the request, and to nobody else.
        let follow_up = match &body {
            Body::StopSpeaking(stop) => Some((Kind::Speak, stop.request_id)),
            Body::StopRecognition(stop) => Some((Kind::Recognize, stop.request_id)),
            Body::CancelRecognition(cancel) => Some((Kind::Recognize, cancel.request_id)),
            _ => None,
        };
        if let Some(key) = follow_up {
            let cancel = matches!(body, Body::CancelRecognition(_));
            let holder = if cancel {
                self.pending.remove(&key)
            } else {
                self.pending.get(&key).copied()
            };
            if let Some(holder) = holder {
                let handed = self.hand(holder, control::Envelope { body: Some(body) });
                if !handed && !cancel {
                    self.pending.remove(&key);
                    return vec![failure(key.0, key.1, GONE)];
                }
            }
            return Vec::new();
        }
        let key = waits(&body);
        let failure_now = unavailable(&body);
        match self.hand_first(control::Envelope { body: Some(body) }) {
            Some(server) => {
                if let Some(key) = key {
                    // A number used again replaces the request it named.
                    self.pending.insert(key, server);
                }
                Vec::new()
            }
            None => failure_now.into_iter().collect(),
        }
    }

    /// Takes an answer from client `server`. Returns what to pass to the guest: `Ok(None)`
    /// when it is dropped (the request is over, or the client does not have it), and an error
    /// when the client sent something that is no answer.
    pub fn answer(&mut self, server: u64, body: Body) -> Result<Option<Body>, String> {
        let holds =
            |relay: &Relay, kind: Kind, id: u32| relay.pending.get(&(kind, id)) == Some(&server);
        match body {
            Body::SpeechVoices(mut voices) => {
                voices.voices.truncate(MAX_VOICES);
                for voice in &mut voices.voices {
                    voice.id = cut(&voice.id, 512);
                    voice.name = cut(&voice.name, 256);
                    voice.language_tag = cut(&voice.language_tag, 64);
                }
                voices.default_voice_id = cut(&voices.default_voice_id, 512);
                Ok(Some(Body::SpeechVoices(voices)))
            }
            Body::SpeechAudio(audio) => {
                if !holds(self, Kind::Speak, audio.request_id) {
                    return Ok(None);
                }
                let frame = audio.channel_count as usize * 2;
                if audio.pcm.len() > MAX_PCM_PIECE
                    || !(1..=8).contains(&audio.channel_count)
                    || !(4000..=192_000).contains(&audio.sample_rate_hz)
                    || audio.pcm.len() % frame != 0
                {
                    return Err(format!(
                        "a piece of sound of {} bytes at {} Hz on {} channels",
                        audio.pcm.len(),
                        audio.sample_rate_hz,
                        audio.channel_count
                    ));
                }
                Ok(Some(Body::SpeechAudio(audio)))
            }
            Body::SpeechDone(mut done) => {
                if !holds(self, Kind::Speak, done.request_id) {
                    return Ok(None);
                }
                self.pending.remove(&(Kind::Speak, done.request_id));
                done.message = cut(&done.message, 1024);
                Ok(Some(Body::SpeechDone(done)))
            }
            Body::RecognitionEvent(mut event) => {
                if !holds(self, Kind::Recognize, event.request_id) {
                    return Ok(None);
                }
                let over = event.kind == RecognitionEventKind::Final as i32
                    || event.kind == RecognitionEventKind::Error as i32;
                if over {
                    self.pending.remove(&(Kind::Recognize, event.request_id));
                }
                event.texts.truncate(MAX_ALTERNATIVES);
                for text in &mut event.texts {
                    *text = cut(text, 4096);
                }
                event.confidences.truncate(event.texts.len());
                event.message = cut(&event.message, 1024);
                Ok(Some(Body::RecognitionEvent(event)))
            }
            Body::SpellingResult(mut result) => {
                if !holds(self, Kind::Spell, result.request_id) {
                    return Ok(None);
                }
                self.pending.remove(&(Kind::Spell, result.request_id));
                result.texts.truncate(MAX_SPELLING_TEXTS);
                for text in &mut result.texts {
                    text.mistakes.truncate(super::MAX_SPELLING_CHARS);
                    for mistake in &mut text.mistakes {
                        mistake.suggestions.truncate(MAX_SUGGESTIONS);
                        for suggestion in &mut mistake.suggestions {
                            *suggestion = cut(suggestion, 256);
                        }
                    }
                }
                Ok(Some(Body::SpellingResult(result)))
            }
            other => Err(format!("{} is not an answer", body_name(&other))),
        }
    }

    /// Client `server` went away. Returns the failures the guest is to be told of, one for
    /// each request it had not finished.
    pub fn server_gone(&mut self, server: u64) -> Vec<Body> {
        self.servers.retain(|(id, _)| *id != server);
        let lost: Vec<(Kind, u32)> = self
            .pending
            .iter()
            .filter(|(_, holder)| **holder == server)
            .map(|(key, _)| *key)
            .collect();
        let mut failures = Vec::new();
        for key in lost {
            self.pending.remove(&key);
            failures.push(failure(key.0, key.1, GONE));
        }
        failures.sort_by_key(|body| format!("{body:?}"));
        failures
    }

    /// The guest's connection ended: the clients stop what they did for it, and nothing it
    /// asked is answered any more.
    pub fn guest_gone(&mut self) {
        let pending: Vec<((Kind, u32), u64)> = self.pending.drain().collect();
        for ((kind, id), server) in pending {
            let stop = match kind {
                Kind::Speak => Body::StopSpeaking(control::StopSpeaking { request_id: id }),
                Kind::Recognize => {
                    Body::CancelRecognition(control::CancelRecognition { request_id: id })
                }
                Kind::Spell => continue,
            };
            self.hand(server, control::Envelope { body: Some(stop) });
        }
    }
}

/// A message's name, for the logs.
fn body_name(body: &Body) -> String {
    let text = format!("{body:?}");
    text.split('(').next().unwrap_or("a message").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speak(id: u32) -> Body {
        Body::SpeakText(control::SpeakText {
            request_id: id,
            text: "hello".into(),
            rate: 1.0,
            pitch: 1.0,
            ..Default::default()
        })
    }

    fn audio(id: u32) -> Body {
        Body::SpeechAudio(control::SpeechAudio {
            request_id: id,
            sample_rate_hz: 22050,
            channel_count: 1,
            pcm: vec![0, 1, 2, 3],
        })
    }

    #[test]
    fn fr36_a_request_nobody_serves_fails_at_once() {
        let mut relay = Relay::new();
        match relay.from_guest(speak(1)).as_slice() {
            [Body::SpeechDone(done)] => {
                assert_eq!(done.request_id, 1);
                assert_eq!(done.outcome, SpeechOutcome::Unavailable as i32);
            }
            other => panic!("{other:?}"),
        }
        match relay
            .from_guest(Body::ListSpeechVoices(control::ListSpeechVoices {}))
            .as_slice()
        {
            [Body::SpeechVoices(voices)] => assert!(!voices.available),
            other => panic!("{other:?}"),
        }
        let check = Body::CheckSpelling(control::CheckSpelling {
            request_id: 4,
            ..Default::default()
        });
        match relay.from_guest(check).as_slice() {
            [Body::SpellingResult(result)] => assert!(!result.supported),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr36_answers_reach_the_guest_only_from_the_client_that_has_the_request() {
        let mut relay = Relay::new();
        let first = relay.serve();
        let second = relay.serve();
        assert!(relay.from_guest(speak(7)).is_empty());
        assert!(matches!(
            first.requests.try_recv().unwrap().body,
            Some(Body::SpeakText(_))
        ));
        assert!(
            second.requests.try_recv().is_err(),
            "only the first client is asked"
        );
        assert_eq!(relay.answer(second.id, audio(7)), Ok(None));
        assert!(matches!(relay.answer(first.id, audio(7)), Ok(Some(_))));
        let done = Body::SpeechDone(control::SpeechDone {
            request_id: 7,
            outcome: SpeechOutcome::Done as i32,
            message: String::new(),
        });
        assert!(matches!(relay.answer(first.id, done.clone()), Ok(Some(_))));
        assert_eq!(
            relay.answer(first.id, done),
            Ok(None),
            "the request is over"
        );
        assert!(relay.answer(first.id, speak(1)).is_err());
    }

    #[test]
    fn nfr5_a_client_that_goes_away_fails_what_it_had_and_the_next_one_serves() {
        let mut relay = Relay::new();
        let first = relay.serve();
        let second = relay.serve();
        relay.from_guest(speak(1));
        relay.from_guest(Body::StartRecognition(control::StartRecognition {
            request_id: 2,
            ..Default::default()
        }));
        let failures = relay.server_gone(first.id);
        assert_eq!(failures.len(), 2, "{failures:?}");
        drop(first);
        assert!(relay.from_guest(speak(3)).is_empty());
        assert!(matches!(
            second.requests.try_recv().unwrap().body,
            Some(Body::SpeakText(_))
        ));
    }

    #[test]
    fn fr36_a_stop_goes_to_the_client_that_speaks() {
        let mut relay = Relay::new();
        let first = relay.serve();
        relay.from_guest(speak(5));
        let _ = first.requests.try_recv();
        let stop = Body::StopSpeaking(control::StopSpeaking { request_id: 5 });
        assert!(relay.from_guest(stop).is_empty());
        assert!(matches!(
            first.requests.try_recv().unwrap().body,
            Some(Body::StopSpeaking(_))
        ));
        relay.guest_gone();
        assert!(matches!(
            first.requests.try_recv().unwrap().body,
            Some(Body::StopSpeaking(_))
        ));
        assert_eq!(relay.answer(first.id, audio(5)), Ok(None));
    }

    #[test]
    fn nfr5_sound_of_the_wrong_size_is_refused() {
        let mut relay = Relay::new();
        let first = relay.serve();
        relay.from_guest(speak(1));
        let big = Body::SpeechAudio(control::SpeechAudio {
            request_id: 1,
            sample_rate_hz: 22050,
            channel_count: 1,
            pcm: vec![0; MAX_PCM_PIECE + 2],
        });
        assert!(relay.answer(first.id, big).is_err());
        let odd = Body::SpeechAudio(control::SpeechAudio {
            request_id: 1,
            sample_rate_hz: 22050,
            channel_count: 1,
            pcm: vec![0; 3],
        });
        assert!(relay.answer(first.id, odd).is_err());
    }
}

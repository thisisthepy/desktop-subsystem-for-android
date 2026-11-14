//! Speech and spelling for the guest from the host: the text-to-speech engine, the speech
//! recognition service and the spell checker the guest's apps use are answered by the host's
//! own speech synthesis, speech recognition and spell checker.
//!
//! The agent's services send their requests on the control channel. The session hands each
//! one to the first client of the local API that serves them ([`relay::Relay`]), a program in
//! the user's desktop session, where the user's voices, dictionaries and privacy settings
//! are. That client ([`server::serve`]) runs the platform's [`SpeechSynthesizer`],
//! [`SpeechRecognizer`] and [`SpellChecker`], each on a thread of its own, and its answers go
//! back the same way. With no such client, or a host that has no such service, a request is
//! answered at once with the failure: an app never waits for nothing, and nothing a guest or
//! a client sends ends the session.
//!
//! Nothing here names a host operating system API; the platform supplies the three services.

pub mod relay;
pub mod server;
pub mod text;

/// The most voices the host lists.
pub const MAX_VOICES: usize = 512;
/// The longest text the host speaks at once, in characters, as Android's TextToSpeech takes.
pub const MAX_SPEAK_CHARS: usize = 4000;
/// The largest piece of sound in one message, in bytes.
pub const MAX_PCM_PIECE: usize = 64 * 1024;
/// The most texts one spelling request checks.
pub const MAX_SPELLING_TEXTS: usize = 64;
/// The longest text the host checks, in characters.
pub const MAX_SPELLING_CHARS: usize = 4096;
/// The most suggestions the host gives for one mistake.
pub const MAX_SUGGESTIONS: usize = 10;
/// How many suggestions the host gives when the guest does not say.
pub const DEFAULT_SUGGESTIONS: usize = 5;
/// The most alternatives the host gives for what was said.
pub const MAX_ALTERNATIVES: usize = 10;
/// The slowest and fastest speech the host asks its voices for, where 1 is a voice's own.
pub const RATE_RANGE: (f32, f32) = (0.25, 4.0);
/// The lowest and highest pitch the host asks its voices for, where 1 is a voice's own.
pub const PITCH_RANGE: (f32, f32) = (0.25, 2.0);

/// One of the host's voices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Voice {
    /// The host's own name for it, opaque to the guest.
    pub id: String,
    /// Its name as the host shows it.
    pub name: String,
    /// Its language as a BCP 47 tag.
    pub language_tag: String,
    /// A neural voice that sounds natural.
    pub natural: bool,
    pub needs_network: bool,
}

/// The host's voices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Voices {
    pub voices: Vec<Voice>,
    /// The voice the host speaks with when none is named; empty when it has none.
    pub default_id: String,
}

/// What the guest asked to be spoken: one piece of its text, a sentence or so.
#[derive(Debug, Clone, PartialEq)]
pub struct Speech {
    pub text: String,
    /// Empty for the host's voice for the language.
    pub voice_id: String,
    /// Empty for the host's own language.
    pub language_tag: String,
    /// Where 1 is the voice's own, within [`RATE_RANGE`].
    pub rate: f32,
    /// Where 1 is the voice's own, within [`PITCH_RANGE`].
    pub pitch: f32,
}

/// A piece of spoken sound: 16-bit signed little-endian PCM, channels interleaved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub sample_rate_hz: u32,
    pub channels: u32,
    pub data: Vec<u8>,
}

/// Why the host did not speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeakError {
    /// It has no voice for the language, or no speech synthesis.
    Unavailable(String),
    /// Its speech service failed.
    Failed(String),
}

/// The host's speech synthesis, as the platform runs it. It is made and used on one thread.
pub trait SpeechSynthesizer {
    /// The host's voices. An error says it has no speech synthesis.
    fn voices(&mut self) -> Result<Voices, String>;
    /// Speaks `speech`, handing its sound to `sound` as it comes, and returns once all of it
    /// was handed on. It looks at `stop` as often as it can and returns early once it is set.
    /// A host whose speech service plays the sound itself and gives programs none plays it,
    /// hands nothing on, and returns once it has played.
    fn speak(
        &mut self,
        speech: &Speech,
        stop: &std::sync::atomic::AtomicBool,
        sound: &mut dyn FnMut(Pcm),
    ) -> Result<(), SpeakError>;
}

/// What the guest asked the host to listen for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    /// Empty for the host's own language.
    pub language_tag: String,
    pub partial_results: bool,
    /// At least 1, at most [`MAX_ALTERNATIVES`].
    pub max_results: usize,
    pub prefer_offline: bool,
}

/// Why a recognition failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecognitionFailure {
    NoMatch,
    SpeechTimeout,
    /// The host's privacy settings keep DSA from the microphone or from speech recognition.
    Denied,
    NoMicrophone,
    LanguageUnsupported,
    Unavailable,
    Network,
    Busy,
    Other,
}

impl RecognitionFailure {
    pub fn to_control(self) -> dsa_protocol::control::RecognitionError {
        use dsa_protocol::control::RecognitionError as E;
        match self {
            RecognitionFailure::NoMatch => E::NoMatch,
            RecognitionFailure::SpeechTimeout => E::SpeechTimeout,
            RecognitionFailure::Denied => E::Denied,
            RecognitionFailure::NoMicrophone => E::NoMicrophone,
            RecognitionFailure::LanguageUnsupported => E::LanguageUnsupported,
            RecognitionFailure::Unavailable => E::Unavailable,
            RecognitionFailure::Network => E::Network,
            RecognitionFailure::Busy => E::Busy,
            RecognitionFailure::Other => E::Other,
        }
    }
}

/// What the host's speech recognition heard, as it hears it.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// It listens.
    Ready,
    SpeechBegan,
    SpeechEnded,
    /// What it recognised so far.
    Partial(String),
    /// What it recognised, best first, with how sure it is of each when it says. The end.
    Final(Vec<(String, Option<f32>)>),
    /// It failed. The end.
    Failed(RecognitionFailure, String),
}

/// A recognition the host runs.
pub trait Listening {
    /// The user stopped speaking: stop listening and give what was recognised.
    fn stop(&mut self);
    /// Drop it; nothing more is wanted of it.
    fn cancel(&mut self);
}

/// Where a recognition says what it heard, from any thread.
pub type HeardSink = Box<dyn Fn(Heard) + Send + Sync>;

/// The host's speech recognition, as the platform runs it. It is made and used on one thread;
/// what it hears may come on any.
pub trait SpeechRecognizer {
    /// Starts listening for `listen`, saying what it hears to `heard` until a
    /// [`Heard::Final`] or a [`Heard::Failed`]. An error says it cannot listen at all.
    fn start(
        &mut self,
        listen: &Listen,
        heard: HeardSink,
    ) -> Result<Box<dyn Listening>, (RecognitionFailure, String)>;
}

/// A word the host's spell checker finds wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mistake {
    /// Where it is in its text, in UTF-16 code units.
    pub start: u32,
    pub length: u32,
    /// What to write instead, best first; an empty one means delete the word.
    pub suggestions: Vec<String>,
    /// The host is sure of its first suggestion.
    pub recommended: bool,
}

/// The host's spell checker, as the platform runs it. It is made and used on one thread.
pub trait SpellChecker {
    /// The mistakes in `text`, in order, with at most `max_suggestions` suggestions each.
    /// `None` when the host has no spell checker for `language_tag`.
    fn check(
        &mut self,
        language_tag: &str,
        text: &str,
        max_suggestions: usize,
    ) -> Result<Option<Vec<Mistake>>, String>;
}

/// A host without the service: every request is answered as unavailable.
pub struct Unavailable(pub String);

impl SpeechSynthesizer for Unavailable {
    fn voices(&mut self) -> Result<Voices, String> {
        Err(self.0.clone())
    }

    fn speak(
        &mut self,
        _: &Speech,
        _: &std::sync::atomic::AtomicBool,
        _: &mut dyn FnMut(Pcm),
    ) -> Result<(), SpeakError> {
        Err(SpeakError::Unavailable(self.0.clone()))
    }
}

impl SpeechRecognizer for Unavailable {
    fn start(
        &mut self,
        _: &Listen,
        _: HeardSink,
    ) -> Result<Box<dyn Listening>, (RecognitionFailure, String)> {
        Err((RecognitionFailure::Unavailable, self.0.clone()))
    }
}

impl SpellChecker for Unavailable {
    fn check(&mut self, _: &str, _: &str, _: usize) -> Result<Option<Vec<Mistake>>, String> {
        Ok(None)
    }
}

/// Cuts `text` to at most `max` characters.
pub(crate) fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

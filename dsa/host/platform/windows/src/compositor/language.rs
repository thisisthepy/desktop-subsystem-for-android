//! Speech and spelling for the user's Android, from Windows' own.
//!
//! The service runs as the machine and has none of the user's voices, dictionaries or
//! privacy settings, so the window manager, which runs in the user's session, answers the
//! Android's text-to-speech engine, speech recognition service and spell checker
//! ([`dsa_host_core::language::server::serve`]) with:
//!
//! - speech synthesis: `Windows.Media.SpeechSynthesis`, with every voice it offers programs,
//!   the natural ones too when Windows has them, each sentence synthesised to a WAV stream
//!   whose PCM goes to Android;
//! - speech recognition: `Windows.Media.SpeechRecognition` with its dictation topic, one
//!   utterance per request, with its hypotheses as partial results. Windows' microphone
//!   privacy setting for desktop apps, and its online speech recognition setting, which
//!   dictation needs, are checked first, so a blocked microphone is an error in Android and is
//!   never opened;
//! - spell checking: the spell checker of `ISpellCheckerFactory`, whose errors are already in
//!   UTF-16 code units, as Android counts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::language::server::{self, Backends, Log};
use dsa_host_core::language::text::{normalize_tag, pick_voice, primary_language, wav_pcm};
use dsa_host_core::language::{
    Heard, HeardSink, Listen, Listening, Mistake, Pcm, RecognitionFailure, SpeakError, Speech,
    SpeechRecognizer, SpeechSynthesizer, SpellChecker, Voice, Voices,
};
use dsa_transport::Connector;
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::SpeechRecognition::{
    ISpeechRecognitionConstraint, SpeechRecognitionConfidence,
    SpeechRecognitionHypothesisGeneratedEventArgs, SpeechRecognitionResult,
    SpeechRecognitionResultStatus, SpeechRecognitionScenario, SpeechRecognitionTopicConstraint,
    SpeechRecognizer as WinRecognizer, SpeechRecognizerState,
    SpeechRecognizerStateChangedEventArgs,
};
use windows::Media::SpeechSynthesis::{SpeechSynthesizer as WinSynthesizer, VoiceInformation};
use windows::Storage::Streams::DataReader;
use windows::Win32::Globalization::{
    ISpellChecker, ISpellCheckerFactory, SpellCheckerFactory, CORRECTIVE_ACTION_DELETE,
    CORRECTIVE_ACTION_REPLACE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IEnumString, CLSCTX_ALL};

use crate::compositor::log::{note, CompositorLogger};

/// The build the window manager names itself by when it serves speech and spelling.
const BUILD: &str = "dsa-wm speech and spelling";

/// How long Windows waits for the user to start speaking, and for the end of what they say.
const INITIAL_SILENCE: Duration = Duration::from_secs(6);
const END_SILENCE: Duration = Duration::from_millis(1200);

/// What Windows says when its online speech recognition, which dictation needs, is off
/// (SPERR_SPEECH_PRIVACY_POLICY_NOT_ACCEPTED).
const PRIVACY_POLICY_NOT_ACCEPTED: u32 = 0x8004_5509;
/// E_ACCESSDENIED: the microphone is off for desktop apps.
const ACCESS_DENIED: u32 = 0x8007_0005;
/// What Windows says when there is no capture device: E_NOTFOUND, and Media Foundation's
/// MF_E_NO_CAPTURE_DEVICES_AVAILABLE.
const NOT_FOUND: u32 = 0x8007_0490;
const NO_CAPTURE_DEVICES: u32 = 0xC00D_ABE0;

/// Serves the user's Android with Windows' speech and spelling until `stop` is set.
pub fn serve(
    mut connector: Box<dyn Connector>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let log: Log = Arc::new(move |line: &str| note(logger.as_deref(), line));
    let backends = Backends {
        speech: Box::new(|| Box::new(WindowsSpeech::new()) as Box<dyn SpeechSynthesizer>),
        recognition: Box::new(|| {
            Box::new(WindowsRecognition {
                _com: Apartment::enter(),
            }) as Box<dyn SpeechRecognizer>
        }),
        spelling: Box::new(|| Box::new(WindowsSpelling::new()) as Box<dyn SpellChecker>),
    };
    server::serve(&mut *connector, BUILD, backends, &stop, log);
}

/// COM for one of the services' threads, in the multithreaded apartment, for as long as the
/// service lives.
struct Apartment {
    initialised: bool,
}

impl Apartment {
    fn enter() -> Apartment {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // SAFETY: initialises COM for this thread; undone on drop when it succeeded.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Apartment {
            initialised: result.is_ok(),
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialised {
            // SAFETY: balances the successful CoInitializeEx of this thread.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// Whether a voice of `name` and `description` is one of Windows' natural (neural) voices.
pub fn natural_voice(name: &str, description: &str) -> bool {
    [name, description]
        .iter()
        .any(|text| text.contains("Natural") || text.contains("Neural"))
}

/// The speaking rate Windows takes for `rate`, where 1 is the voice's own: 0.5 to 6.
pub fn speaking_rate(rate: f32) -> f64 {
    f64::from(rate).clamp(0.5, 6.0)
}

/// The pitch Windows takes for `pitch`, where 1 is the voice's own: 0 to 2.
pub fn audio_pitch(pitch: f32) -> f64 {
    f64::from(pitch).clamp(0.0, 2.0)
}

/// Windows' speech synthesis, with the voices it offers programs.
struct WindowsSpeech {
    _com: Apartment,
    voices: Vec<(Voice, VoiceInformation)>,
    default_id: String,
}

fn failed(what: &str, error: windows::core::Error) -> SpeakError {
    SpeakError::Failed(format!("{what}: {error}"))
}

impl WindowsSpeech {
    fn new() -> WindowsSpeech {
        WindowsSpeech {
            _com: Apartment::enter(),
            voices: Vec::new(),
            default_id: String::new(),
        }
    }

    /// Reads Windows' voices again.
    fn load(&mut self) -> windows::core::Result<()> {
        let all = WinSynthesizer::AllVoices()?;
        let mut voices = Vec::new();
        for i in 0..all.Size()? {
            let info = all.GetAt(i)?;
            let name = info.DisplayName()?.to_string();
            let description = info
                .Description()
                .map(|d| d.to_string())
                .unwrap_or_default();
            let voice = Voice {
                id: info.Id()?.to_string(),
                natural: natural_voice(&name, &description),
                name,
                language_tag: info.Language()?.to_string(),
                needs_network: description.contains("Online"),
            };
            voices.push((voice, info));
        }
        self.default_id = WinSynthesizer::DefaultVoice()
            .and_then(|v| v.Id())
            .map(|id| id.to_string())
            .unwrap_or_default();
        self.voices = voices;
        Ok(())
    }
}

impl SpeechSynthesizer for WindowsSpeech {
    fn voices(&mut self) -> Result<Voices, String> {
        self.load()
            .map_err(|e| format!("Windows' speech synthesis cannot be read: {e}"))?;
        if self.voices.is_empty() {
            return Err("Windows has no voices for programs".into());
        }
        Ok(Voices {
            voices: self.voices.iter().map(|(v, _)| v.clone()).collect(),
            default_id: self.default_id.clone(),
        })
    }

    fn speak(
        &mut self,
        speech: &Speech,
        stop: &AtomicBool,
        sound: &mut dyn FnMut(Pcm),
    ) -> Result<(), SpeakError> {
        if self.voices.is_empty() {
            self.load()
                .map_err(|e| failed("reading Windows' voices", e))?;
        }
        let listed: Vec<Voice> = self.voices.iter().map(|(v, _)| v.clone()).collect();
        let Some(voice) = pick_voice(
            &listed,
            &speech.voice_id,
            &speech.language_tag,
            &self.default_id,
        ) else {
            return Err(SpeakError::Unavailable(format!(
                "Windows has no voice for {}; add one in Settings, Time & language, Speech",
                speech.language_tag
            )));
        };
        let Some((_, info)) = self.voices.iter().find(|(v, _)| v.id == voice.id) else {
            return Err(SpeakError::Unavailable("the voice is gone".into()));
        };
        let synthesizer =
            WinSynthesizer::new().map_err(|e| failed("starting Windows' speech synthesis", e))?;
        synthesizer
            .SetVoice(info)
            .map_err(|e| failed("choosing the voice", e))?;
        if let Ok(options) = synthesizer.Options() {
            let _ = options.SetSpeakingRate(speaking_rate(speech.rate));
            let _ = options.SetAudioPitch(audio_pitch(speech.pitch));
        }
        let stream = synthesizer
            .SynthesizeTextToStreamAsync(&HSTRING::from(speech.text.as_str()))
            .and_then(|op| op.join())
            .map_err(|e| failed("speaking", e))?;
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let size = u32::try_from(stream.Size().map_err(|e| failed("reading the sound", e))?)
            .map_err(|_| SpeakError::Failed("the sound is too long".into()))?;
        let input = stream
            .GetInputStreamAt(0)
            .map_err(|e| failed("reading the sound", e))?;
        let reader =
            DataReader::CreateDataReader(&input).map_err(|e| failed("reading the sound", e))?;
        reader
            .LoadAsync(size)
            .and_then(|op| op.join())
            .map_err(|e| failed("reading the sound", e))?;
        let mut wav = vec![0u8; size as usize];
        reader
            .ReadBytes(&mut wav)
            .map_err(|e| failed("reading the sound", e))?;
        let pcm = wav_pcm(&wav).map_err(SpeakError::Failed)?;
        sound(pcm);
        Ok(())
    }
}

/// Reads a string value under the current user's (or, with `machine`, the computer's)
/// registry key `key`; `None` when it is not set.
fn registry_string(machine: bool, key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let root = if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let mut buffer = [0u16; 64];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the names are NUL-terminated, and the buffer and its size in bytes match.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let chars = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..chars]);
    Some(text.trim_end_matches('\0').to_string())
}

/// Reads a DWORD value under the current user's registry key `key`.
fn registry_dword(key: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: the names are NUL-terminated, and the value is four bytes, as asked.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

const MICROPHONE_CONSENT: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// Why Windows keeps DSA from the microphone, from its consent values: the computer's, the
/// user's, and the user's for desktop apps, each "Allow", "Deny" or unset. `None` when it
/// does not.
pub fn microphone_blocked(
    machine: Option<&str>,
    user: Option<&str>,
    desktop_apps: Option<&str>,
) -> Option<&'static str> {
    if machine == Some("Deny") {
        return Some("microphone access is off for this computer in Windows' privacy settings");
    }
    if user == Some("Deny") {
        return Some("microphone access is off in Windows' privacy settings");
    }
    if desktop_apps == Some("Deny") {
        return Some(
            "desktop apps may not use the microphone in Windows' privacy settings \
             (Privacy & security, Microphone)",
        );
    }
    None
}

/// What a failed recognition's error code means for Android.
pub fn failure_of_code(code: u32) -> (RecognitionFailure, String) {
    match code {
        PRIVACY_POLICY_NOT_ACCEPTED => (
            RecognitionFailure::Denied,
            "online speech recognition is off in Windows' privacy settings (Privacy & security, \
             Speech), and dictation needs it"
                .into(),
        ),
        ACCESS_DENIED => (
            RecognitionFailure::Denied,
            "Windows keeps DSA from the microphone".into(),
        ),
        NOT_FOUND | NO_CAPTURE_DEVICES => (
            RecognitionFailure::NoMicrophone,
            "this computer has no microphone".into(),
        ),
        other => (
            RecognitionFailure::Other,
            format!("Windows' speech recognition failed (0x{other:08X})"),
        ),
    }
}

/// What a recognition result's status means for Android, when it is no success.
pub fn failure_of_status(status: SpeechRecognitionResultStatus) -> (RecognitionFailure, String) {
    let (failure, why) = match status {
        SpeechRecognitionResultStatus::TimeoutExceeded => {
            (RecognitionFailure::SpeechTimeout, "nobody spoke")
        }
        SpeechRecognitionResultStatus::NetworkFailure => (
            RecognitionFailure::Network,
            "Windows' speech recognition could not reach its service",
        ),
        SpeechRecognitionResultStatus::MicrophoneUnavailable => (
            RecognitionFailure::NoMicrophone,
            "the microphone could not be opened",
        ),
        SpeechRecognitionResultStatus::TopicLanguageNotSupported
        | SpeechRecognitionResultStatus::GrammarLanguageMismatch => (
            RecognitionFailure::LanguageUnsupported,
            "Windows does not recognise speech in this language",
        ),
        SpeechRecognitionResultStatus::AudioQualityFailure
        | SpeechRecognitionResultStatus::UserCanceled => (
            RecognitionFailure::NoMatch,
            "nothing that was said was recognised",
        ),
        _ => (
            RecognitionFailure::Other,
            "Windows' speech recognition failed",
        ),
    };
    (failure, why.to_string())
}

/// Why Windows keeps DSA from recognising speech now, if it does.
fn recognition_blocked() -> Option<(RecognitionFailure, String)> {
    let desktop_key = format!(r"{MICROPHONE_CONSENT}\NonPackaged");
    let machine = registry_string(true, MICROPHONE_CONSENT, "Value");
    let user = registry_string(false, MICROPHONE_CONSENT, "Value");
    let desktop = registry_string(false, &desktop_key, "Value");
    if let Some(why) = microphone_blocked(machine.as_deref(), user.as_deref(), desktop.as_deref()) {
        return Some((RecognitionFailure::Denied, why.into()));
    }
    let accepted = registry_dword(
        r"Software\Microsoft\Speech_OneCore\Settings\OnlineSpeechPrivacy",
        "HasAccepted",
    );
    if accepted == Some(0) {
        return Some(failure_of_code(PRIVACY_POLICY_NOT_ACCEPTED));
    }
    None
}

/// Windows' speech recognition.
struct WindowsRecognition {
    _com: Apartment,
}

fn other(what: &str, error: windows::core::Error) -> (RecognitionFailure, String) {
    let code = error.code().0 as u32;
    match code {
        PRIVACY_POLICY_NOT_ACCEPTED | ACCESS_DENIED | NOT_FOUND | NO_CAPTURE_DEVICES => {
            failure_of_code(code)
        }
        _ => (RecognitionFailure::Other, format!("{what}: {error}")),
    }
}

/// The recognizer for `tag`, or why there is none.
fn recognizer_for(tag: &str) -> Result<WinRecognizer, (RecognitionFailure, String)> {
    if tag.trim().is_empty() {
        return WinRecognizer::new().map_err(|e| other("starting speech recognition", e));
    }
    let wanted = normalize_tag(tag);
    let languages = WinRecognizer::SupportedTopicLanguages()
        .map_err(|e| other("reading the recognised languages", e))?;
    let mut same_language = None;
    for i in 0..languages.Size().unwrap_or(0) {
        let Ok(language) = languages.GetAt(i) else {
            continue;
        };
        let Ok(found) = language.LanguageTag() else {
            continue;
        };
        let found = normalize_tag(&found.to_string());
        if found == wanted {
            same_language = Some(language);
            break;
        }
        if same_language.is_none() && primary_language(&found) == primary_language(&wanted) {
            same_language = Some(language);
        }
    }
    let Some(language) = same_language else {
        return Err((
            RecognitionFailure::LanguageUnsupported,
            format!(
                "Windows does not recognise speech in {tag}; add its speech pack in Settings, \
                 Time & language, Speech"
            ),
        ));
    };
    WinRecognizer::Create(&language).map_err(|e| other("starting speech recognition", e))
}

fn timespan(time: Duration) -> TimeSpan {
    TimeSpan {
        Duration: i64::try_from(time.as_nanos() / 100).unwrap_or(i64::MAX),
    }
}

/// What a finished recognition heard, for Android.
fn heard_of(result: &SpeechRecognitionResult, max: usize) -> Heard {
    let status = match result.Status() {
        Ok(status) => status,
        Err(e) => return Heard::Failed(RecognitionFailure::Other, e.to_string()),
    };
    if status != SpeechRecognitionResultStatus::Success {
        let (failure, why) = failure_of_status(status);
        return Heard::Failed(failure, why);
    }
    if result.Confidence().ok() == Some(SpeechRecognitionConfidence::Rejected) {
        return Heard::Final(Vec::new());
    }
    let mut alternatives = Vec::new();
    if let Ok(list) = result.GetAlternates(max as u32) {
        for i in 0..list.Size().unwrap_or(0) {
            if let Ok(alternative) = list.GetAt(i) {
                let text = alternative
                    .Text()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                let confidence = alternative.RawConfidence().ok().map(|c| c as f32);
                alternatives.push((text, confidence));
            }
        }
    }
    if alternatives.is_empty() {
        let text = result.Text().map(|t| t.to_string()).unwrap_or_default();
        let confidence = result.RawConfidence().ok().map(|c| c as f32);
        alternatives.push((text, confidence));
    }
    Heard::Final(alternatives)
}

impl SpeechRecognizer for WindowsRecognition {
    fn start(
        &mut self,
        listen: &Listen,
        heard: HeardSink,
    ) -> Result<Box<dyn Listening>, (RecognitionFailure, String)> {
        if let Some(blocked) = recognition_blocked() {
            return Err(blocked);
        }
        let recognizer = recognizer_for(&listen.language_tag)?;
        let constraint = SpeechRecognitionTopicConstraint::Create(
            SpeechRecognitionScenario::Dictation,
            &HSTRING::from("dictation"),
        )
        .and_then(|c| c.cast::<ISpeechRecognitionConstraint>())
        .map_err(|e| other("preparing dictation", e))?;
        recognizer
            .Constraints()
            .and_then(|constraints| constraints.Append(&constraint))
            .map_err(|e| other("preparing dictation", e))?;
        let compiled = recognizer
            .CompileConstraintsAsync()
            .and_then(|op| op.join())
            .map_err(|e| other("preparing dictation", e))?;
        let status = compiled
            .Status()
            .map_err(|e| other("preparing dictation", e))?;
        if status != SpeechRecognitionResultStatus::Success {
            return Err(failure_of_status(status));
        }
        if let Ok(timeouts) = recognizer.Timeouts() {
            let _ = timeouts.SetInitialSilenceTimeout(timespan(INITIAL_SILENCE));
            let _ = timeouts.SetEndSilenceTimeout(timespan(END_SILENCE));
        }
        let heard: Arc<HeardSink> = Arc::new(heard);
        let partial = Arc::clone(&heard);
        let hypothesis_token = recognizer
            .HypothesisGenerated(&TypedEventHandler::<
                WinRecognizer,
                SpeechRecognitionHypothesisGeneratedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    let text = args.Hypothesis()?.Text()?.to_string();
                    partial(Heard::Partial(text));
                }
                Ok(())
            }))
            .ok();
        let states = Arc::clone(&heard);
        let state_token = recognizer
            .StateChanged(&TypedEventHandler::<
                WinRecognizer,
                SpeechRecognizerStateChangedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    match args.State()? {
                        SpeechRecognizerState::Capturing => states(Heard::Ready),
                        SpeechRecognizerState::SpeechDetected => states(Heard::SpeechBegan),
                        SpeechRecognizerState::SoundEnded => states(Heard::SpeechEnded),
                        _ => {}
                    }
                }
                Ok(())
            }))
            .ok();
        let operation = recognizer
            .RecognizeAsync()
            .map_err(|e| other("listening", e))?;
        let max = listen.max_results;
        let waiting = operation.clone();
        let ended = Arc::clone(&heard);
        let waiter = std::thread::Builder::new()
            .name("dsa-recognition-wait".into())
            .spawn(move || {
                let _com = Apartment::enter();
                match waiting.join() {
                    Ok(result) => ended(heard_of(&result, max)),
                    Err(error) => {
                        let (failure, why) = other("listening", error);
                        ended(Heard::Failed(failure, why));
                    }
                }
            });
        if let Err(error) = waiter {
            let _ = operation.Cancel();
            return Err((
                RecognitionFailure::Other,
                format!("could not wait for the recognition: {error}"),
            ));
        }
        Ok(Box::new(WindowsListening {
            recognizer,
            operation,
            hypothesis_token,
            state_token,
        }))
    }
}

/// A recognition Windows runs.
struct WindowsListening {
    recognizer: WinRecognizer,
    operation: windows_future::IAsyncOperation<SpeechRecognitionResult>,
    hypothesis_token: Option<i64>,
    state_token: Option<i64>,
}

impl Listening for WindowsListening {
    fn stop(&mut self) {
        // Windows ends the utterance and gives what it recognised so far.
        let _ = self.recognizer.StopRecognitionAsync();
    }

    fn cancel(&mut self) {
        let _ = self.operation.Cancel();
    }
}

impl Drop for WindowsListening {
    fn drop(&mut self) {
        if let Some(token) = self.hypothesis_token.take() {
            let _ = self.recognizer.RemoveHypothesisGenerated(token);
        }
        if let Some(token) = self.state_token.take() {
            let _ = self.recognizer.RemoveStateChanged(token);
        }
        let _ = self.operation.Cancel();
        let _ = self.recognizer.Close();
    }
}

/// Windows' spell checker, one for each language asked of it.
struct WindowsSpelling {
    _com: Apartment,
    factory: Option<ISpellCheckerFactory>,
    checkers: HashMap<String, Option<ISpellChecker>>,
}

/// Takes a string COM allocated and frees it.
fn take_string(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: COM gave a NUL-terminated string, which is read once and freed once.
    unsafe {
        let value = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0 as *const _));
        value
    }
}

/// Every string of a COM string enumeration, freed as it is read.
fn strings_of(list: &IEnumString, max: usize) -> Vec<String> {
    let mut found = Vec::new();
    while found.len() < max {
        let mut item = [PWSTR::null()];
        let mut fetched = 0u32;
        // SAFETY: one slot for one string, and the count of those fetched.
        let status = unsafe { list.Next(&mut item, Some(&mut fetched as *mut u32)) };
        if status.is_err() || fetched == 0 {
            break;
        }
        found.push(take_string(item[0]));
    }
    found
}

impl WindowsSpelling {
    fn new() -> WindowsSpelling {
        WindowsSpelling {
            _com: Apartment::enter(),
            factory: None,
            checkers: HashMap::new(),
        }
    }

    fn factory(&mut self) -> Result<ISpellCheckerFactory, String> {
        if let Some(factory) = &self.factory {
            return Ok(factory.clone());
        }
        // SAFETY: creates the spell checker factory, a plain COM class.
        let factory: ISpellCheckerFactory =
            unsafe { CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_ALL) }
                .map_err(|e| format!("Windows' spell checker cannot be opened: {e}"))?;
        self.factory = Some(factory.clone());
        Ok(factory)
    }

    /// The language Windows checks for `tag`: the very one, or another of its language.
    fn language_for(factory: &ISpellCheckerFactory, tag: &str) -> Option<String> {
        let wanted = normalize_tag(tag);
        // SAFETY: a NUL-terminated language tag.
        let exact = unsafe { factory.IsSupported(&HSTRING::from(wanted.as_str())) };
        if exact.map(|b| b.as_bool()).unwrap_or(false) {
            return Some(wanted);
        }
        // SAFETY: lists the languages the factory checks.
        let list = unsafe { factory.SupportedLanguages() }.ok()?;
        strings_of(&list, 256)
            .into_iter()
            .find(|found| primary_language(found) == primary_language(&wanted))
    }

    fn checker(&mut self, tag: &str) -> Result<Option<ISpellChecker>, String> {
        let key = normalize_tag(tag);
        if let Some(checker) = self.checkers.get(&key) {
            return Ok(checker.clone());
        }
        let factory = self.factory()?;
        let checker = match Self::language_for(&factory, &key) {
            // SAFETY: a NUL-terminated language tag the factory says it checks.
            Some(language) => unsafe { factory.CreateSpellChecker(&HSTRING::from(language)) }.ok(),
            None => None,
        };
        self.checkers.insert(key, checker.clone());
        Ok(checker)
    }
}

impl SpellChecker for WindowsSpelling {
    fn check(
        &mut self,
        language_tag: &str,
        text: &str,
        max_suggestions: usize,
    ) -> Result<Option<Vec<Mistake>>, String> {
        let tag = if language_tag.trim().is_empty() {
            user_language()
        } else {
            language_tag.to_string()
        };
        let Some(checker) = self.checker(&tag)? else {
            return Ok(None);
        };
        if text.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let wide = HSTRING::from(text);
        // SAFETY: a NUL-terminated text, checked whole.
        let errors = unsafe { checker.ComprehensiveCheck(&wide) }
            .map_err(|e| format!("Windows' spell checker failed: {e}"))?;
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut mistakes = Vec::new();
        loop {
            let mut error = None;
            // SAFETY: one slot for the next error; S_FALSE and no error at the end.
            let status = unsafe { errors.Next(&mut error) };
            let Some(error) = error.filter(|_| status.is_ok()) else {
                break;
            };
            // SAFETY: reads the error's fields.
            let (start, length, action) = unsafe {
                (
                    error.StartIndex().unwrap_or(0),
                    error.Length().unwrap_or(0),
                    error.CorrectiveAction().ok(),
                )
            };
            let end = (start as usize).saturating_add(length as usize);
            if length == 0 || end > utf16.len() {
                continue;
            }
            let mut suggestions = Vec::new();
            let mut recommended = false;
            if action == Some(CORRECTIVE_ACTION_REPLACE) {
                // SAFETY: the replacement Windows would write itself, which is freed after.
                if let Ok(replacement) = unsafe { error.Replacement() } {
                    suggestions.push(take_string(replacement));
                    recommended = true;
                }
            } else if action == Some(CORRECTIVE_ACTION_DELETE) {
                suggestions.push(String::new());
                recommended = true;
            }
            if suggestions.len() < max_suggestions {
                let word = String::from_utf16_lossy(&utf16[start as usize..end]);
                // SAFETY: a NUL-terminated word.
                if let Ok(list) = unsafe { checker.Suggest(&HSTRING::from(word)) } {
                    for suggestion in strings_of(&list, max_suggestions) {
                        if suggestions.len() < max_suggestions && !suggestions.contains(&suggestion)
                        {
                            suggestions.push(suggestion);
                        }
                    }
                }
            }
            mistakes.push(Mistake {
                start,
                length,
                suggestions,
                recommended,
            });
        }
        Ok(Some(mistakes))
    }
}

/// The user's language, as a BCP 47 tag.
fn user_language() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut name = [0u16; 85];
    // SAFETY: the buffer is LOCALE_NAME_MAX_LENGTH characters, as the call wants.
    let length = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
    if length <= 1 {
        return "en-US".into();
    }
    String::from_utf16_lossy(&name[..length as usize - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_windows_natural_voices_are_told_apart() {
        assert!(natural_voice(
            "Microsoft Aria (Natural) - English (United States)",
            ""
        ));
        assert!(natural_voice(
            "Microsoft Ava",
            "Microsoft Ava Online (Neural)"
        ));
        assert!(!natural_voice("Microsoft David", "Microsoft David Desktop"));
    }

    #[test]
    fn fr36_rate_and_pitch_stay_in_the_range_windows_takes() {
        assert_eq!(speaking_rate(1.0), 1.0);
        assert_eq!(speaking_rate(0.25), 0.5);
        assert_eq!(speaking_rate(4.0), 4.0);
        assert_eq!(audio_pitch(2.5), 2.0);
        assert_eq!(audio_pitch(0.8), f64::from(0.8f32));
    }

    #[test]
    fn fr26_a_microphone_windows_blocks_for_desktop_apps_is_never_opened() {
        assert_eq!(microphone_blocked(None, None, None), None);
        assert_eq!(
            microphone_blocked(Some("Allow"), Some("Allow"), Some("Allow")),
            None
        );
        assert!(microphone_blocked(Some("Deny"), None, None).is_some());
        assert!(microphone_blocked(None, Some("Deny"), Some("Allow")).is_some());
        assert!(microphone_blocked(None, Some("Allow"), Some("Deny"))
            .unwrap()
            .contains("desktop apps"));
    }

    #[test]
    fn fr36_windows_recognition_errors_become_androids() {
        assert_eq!(
            failure_of_code(PRIVACY_POLICY_NOT_ACCEPTED).0,
            RecognitionFailure::Denied
        );
        assert_eq!(failure_of_code(ACCESS_DENIED).0, RecognitionFailure::Denied);
        assert_eq!(
            failure_of_code(NOT_FOUND).0,
            RecognitionFailure::NoMicrophone
        );
        assert_eq!(
            failure_of_status(SpeechRecognitionResultStatus::TimeoutExceeded).0,
            RecognitionFailure::SpeechTimeout
        );
        assert_eq!(
            failure_of_status(SpeechRecognitionResultStatus::TopicLanguageNotSupported).0,
            RecognitionFailure::LanguageUnsupported
        );
        assert_eq!(
            failure_of_status(SpeechRecognitionResultStatus::NetworkFailure).0,
            RecognitionFailure::Network
        );
    }
}

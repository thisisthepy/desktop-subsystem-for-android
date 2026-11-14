//! Speech and spelling for the user's Android, from the Linux desktop's own.
//!
//! - Speech synthesis: Speech Dispatcher, the desktop's speech service, over its SSIP socket.
//!   Its voices are Android's voices. SSIP gives a client no sound: Speech Dispatcher plays
//!   it on the user's sound server itself, so the guest's engine gets no PCM, only the end of
//!   each text once it has been spoken, and a stop in Android cancels the speech here.
//! - Speech recognition: the Linux desktops have no speech recognition service that programs
//!   share, so every request is answered as unavailable, and apps take Android's usual path.
//! - Spell checking: Enchant, which reaches the user's Hunspell, Nuspell or Aspell
//!   dictionaries, a word at a time.
//!
//! Neither library is linked: Enchant is opened when it is first needed, and Speech
//! Dispatcher is a socket. A desktop without them answers that it has none, and the rest of
//! DSA goes on.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::language::server::{self, Backends};
use dsa_host_core::language::text::{normalize_tag, pick_voice, primary_language, words};
use dsa_host_core::language::{
    Mistake, Pcm, SpeakError, Speech, SpeechRecognizer, SpeechSynthesizer, SpellChecker,
    Unavailable, Voice, Voices,
};
use dsa_transport::Connector;

use super::Log;

/// The build the follower names itself by.
const BUILD: &str = "dsa-devices speech and spelling";

/// What every request for speech recognition is told.
const NO_RECOGNITION: &str = "this desktop has no speech recognition service that programs share";

/// Serves the user's Android with the desktop's speech and spelling until `stop` is set.
pub fn serve(mut connector: Box<dyn Connector>, stop: Arc<AtomicBool>, log: Log) {
    let backends = Backends {
        speech: Box::new(|| Box::new(SpeechDispatcher::default()) as Box<dyn SpeechSynthesizer>),
        recognition: Box::new(|| {
            Box::new(Unavailable(NO_RECOGNITION.into())) as Box<dyn SpeechRecognizer>
        }),
        spelling: Box::new(|| Box::new(Enchant::default()) as Box<dyn SpellChecker>),
    };
    server::serve(&mut *connector, BUILD, backends, &stop, log);
}

/// The SSIP rate (-100 to 100, 0 the voice's own) for `rate`, where 1 is the voice's own.
pub fn ssip_rate(rate: f32) -> i32 {
    let rate = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    };
    let value = if rate >= 1.0 {
        (rate - 1.0) / 3.0 * 100.0
    } else {
        (rate - 1.0) / 0.75 * 100.0
    };
    value.round().clamp(-100.0, 100.0) as i32
}

/// The SSIP pitch (-100 to 100) for `pitch`, where 1 is the voice's own.
pub fn ssip_pitch(pitch: f32) -> i32 {
    let pitch = if pitch.is_finite() && pitch > 0.0 {
        pitch
    } else {
        1.0
    };
    let value = if pitch >= 1.0 {
        (pitch - 1.0) * 100.0
    } else {
        (pitch - 1.0) / 0.75 * 100.0
    };
    value.round().clamp(-100.0, 100.0) as i32
}

/// The lines of an SSIP SPEAK block for `text`: every line break as CR LF, a line that starts
/// with a dot doubled, and the block's own end.
pub fn ssip_block(text: &str) -> String {
    let mut block = String::new();
    for line in text.lines() {
        if line.starts_with('.') {
            block.push('.');
        }
        block.push_str(line);
        block.push_str("\r\n");
    }
    block.push_str(".\r\n");
    block
}

/// The voices of an answer to `LIST SYNTHESIS_VOICES`: its lines without the code, each the
/// voice's name, its language and its variant, separated by tabs.
pub fn ssip_voices(lines: &[String]) -> Vec<Voice> {
    lines
        .iter()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let language = fields.next().unwrap_or("").trim();
            if name.is_empty() {
                return None;
            }
            Some(Voice {
                id: name.to_string(),
                name: name.to_string(),
                language_tag: normalize_tag(if language == "none" { "" } else { language }),
                natural: false,
                needs_network: false,
            })
        })
        .collect()
}

/// One SSIP answer: its code, and the text of each of its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u32,
    pub lines: Vec<String>,
}

/// Reads one SSIP line ("225-text" goes on, "225 text" ends), as its code, text and whether
/// it is the last line of its answer.
pub fn ssip_line(line: &str) -> Option<(u32, &str, bool)> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.len() < 4 {
        return None;
    }
    let code = line[..3].parse().ok()?;
    let last = match &line[3..4] {
        " " => true,
        "-" => false,
        _ => return None,
    };
    Some((code, &line[4..], last))
}

/// Where Speech Dispatcher listens for the user: `SPEECHD_ADDRESS`, else its socket in the
/// user's runtime folder.
fn socket_path() -> Option<PathBuf> {
    if let Some(address) = std::env::var_os("SPEECHD_ADDRESS") {
        let address = address.to_string_lossy().into_owned();
        if let Some(path) = address.strip_prefix("unix_socket:") {
            return Some(PathBuf::from(path));
        }
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    Some(runtime.join("speech-dispatcher").join("speechd.sock"))
}

/// A connection to Speech Dispatcher.
struct Ssip {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

impl Ssip {
    fn open() -> Result<Ssip, String> {
        let path = socket_path().ok_or("this session has no XDG_RUNTIME_DIR")?;
        let stream = match std::os::unix::net::UnixStream::connect(&path) {
            Ok(stream) => stream,
            Err(_) => {
                // Speech Dispatcher starts on demand; libspeechd starts it the same way.
                let _ = std::process::Command::new("speech-dispatcher")
                    .arg("--spawn")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                std::os::unix::net::UnixStream::connect(&path).map_err(|e| {
                    format!(
                        "Speech Dispatcher is not running ({}: {e}); install speech-dispatcher",
                        path.display()
                    )
                })?
            }
        };
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut ssip = Ssip {
            reader: BufReader::new(stream),
            writer,
        };
        ssip.command("SET self CLIENT_NAME user:dsa:android")?;
        Ok(ssip)
    }

    /// Reads one whole answer, waiting at most `timeout` for its first line. `Ok(None)` when
    /// nothing came in time.
    fn reply(&mut self, timeout: Duration) -> Result<Option<Reply>, String> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Err("Speech Dispatcher closed the connection".into()),
                Ok(_) => {}
                Err(e)
                    if lines.is_empty()
                        && matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(e.to_string()),
            }
            let Some((code, text, last)) = ssip_line(&line) else {
                return Err(format!("Speech Dispatcher said {line:?}"));
            };
            lines.push(text.to_string());
            if last {
                return Ok(Some(Reply { code, lines }));
            }
            // The rest of an answer comes at once.
            self.reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
        }
    }

    /// Sends one command and returns its answer, past any event that comes first.
    fn command(&mut self, line: &str) -> Result<Reply, String> {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        loop {
            match self.reply(Duration::from_secs(5))? {
                Some(reply) if (700..800).contains(&reply.code) => continue,
                Some(reply) if reply.code >= 300 => {
                    return Err(format!(
                        "Speech Dispatcher refused {line:?}: {} {}",
                        reply.code,
                        reply.lines.join(" ")
                    ))
                }
                Some(reply) => return Ok(reply),
                None => return Err(format!("Speech Dispatcher did not answer {line:?}")),
            }
        }
    }
}

/// Speech Dispatcher, which plays what it speaks on the user's sound server.
#[derive(Default)]
struct SpeechDispatcher {
    connection: Option<Ssip>,
    voices: Vec<Voice>,
}

impl SpeechDispatcher {
    fn connection(&mut self) -> Result<&mut Ssip, String> {
        if self.connection.is_none() {
            let mut ssip = Ssip::open()?;
            ssip.command("SET self NOTIFICATION ALL on")?;
            self.connection = Some(ssip);
        }
        Ok(self.connection.as_mut().expect("connected above"))
    }

    fn speak_with(&mut self, speech: &Speech, stop: &AtomicBool) -> Result<(), String> {
        if self.voices.is_empty() {
            let _ = self.voices();
        }
        let voice = pick_voice(&self.voices, &speech.voice_id, &speech.language_tag, "")
            .map(|v| v.id.clone());
        let ssip = self.connection()?;
        if !speech.language_tag.is_empty() {
            let language = primary_language(&speech.language_tag);
            ssip.command(&format!("SET self LANGUAGE {language}"))?;
        }
        if let Some(voice) = voice {
            ssip.command(&format!("SET self SYNTHESIS_VOICE {voice}"))?;
        }
        ssip.command(&format!("SET self RATE {}", ssip_rate(speech.rate)))?;
        ssip.command(&format!("SET self PITCH {}", ssip_pitch(speech.pitch)))?;
        ssip.command("SPEAK")?;
        ssip.writer
            .write_all(ssip_block(&speech.text).as_bytes())
            .map_err(|e| e.to_string())?;
        // The message's number, then its events: BEGIN, then END or CANCELED.
        let queued = loop {
            match ssip.reply(Duration::from_secs(5))? {
                Some(reply) if reply.code == 225 => break reply,
                Some(reply) if (700..800).contains(&reply.code) => continue,
                Some(reply) => {
                    return Err(format!(
                        "Speech Dispatcher refused the text: {}",
                        reply.code
                    ))
                }
                None => return Err("Speech Dispatcher did not take the text".into()),
            }
        };
        let id = queued.lines.first().cloned().unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut stopped = false;
        loop {
            if stop.load(Ordering::Relaxed) && !stopped {
                stopped = true;
                let _ = ssip.writer.write_all(b"STOP self\r\n");
            }
            match ssip.reply(Duration::from_millis(50))? {
                Some(event) if matches!(event.code, 702 | 703) => {
                    if event.lines.first() == Some(&id) || id.is_empty() {
                        return Ok(());
                    }
                }
                Some(_) | None => {}
            }
            if Instant::now() > deadline {
                return Err("Speech Dispatcher did not finish speaking".into());
            }
        }
    }
}

impl SpeechSynthesizer for SpeechDispatcher {
    fn voices(&mut self) -> Result<Voices, String> {
        let reply = self.connection()?.command("LIST SYNTHESIS_VOICES");
        let reply = match reply {
            Ok(reply) => reply,
            Err(why) => {
                self.connection = None;
                return Err(why);
            }
        };
        let lines = &reply.lines[..reply.lines.len().saturating_sub(1)];
        self.voices = ssip_voices(lines);
        if self.voices.is_empty() {
            return Err("Speech Dispatcher has no voices".into());
        }
        Ok(Voices {
            voices: self.voices.clone(),
            default_id: String::new(),
        })
    }

    fn speak(
        &mut self,
        speech: &Speech,
        stop: &AtomicBool,
        _sound: &mut dyn FnMut(Pcm),
    ) -> Result<(), SpeakError> {
        self.speak_with(speech, stop).map_err(|why| {
            // The next request connects again.
            self.connection = None;
            SpeakError::Unavailable(why)
        })
    }
}

type BrokerInit = unsafe extern "C" fn() -> *mut c_void;
type BrokerFree = unsafe extern "C" fn(*mut c_void);
type RequestDict = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type DictExists = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type DictCheck = unsafe extern "C" fn(*mut c_void, *const c_char, isize) -> c_int;
type DictSuggest =
    unsafe extern "C" fn(*mut c_void, *const c_char, isize, *mut usize) -> *mut *mut c_char;
type FreeStringList = unsafe extern "C" fn(*mut c_void, *mut *mut c_char);

/// Enchant's functions.
struct EnchantApi {
    broker_init: BrokerInit,
    broker_free: BrokerFree,
    request_dict: RequestDict,
    dict_exists: DictExists,
    dict_check: DictCheck,
    dict_suggest: DictSuggest,
    free_string_list: FreeStringList,
}

/// Looks up `name` in the library `handle`.
///
/// # Safety
///
/// `T` must be the function pointer type of the symbol.
unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    // SAFETY: a library handle dlopen returned and a NUL-terminated name.
    let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if address.is_null() {
        return None;
    }
    // SAFETY: the caller names the symbol's type; a pointer is the size of a function pointer.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) })
}

fn enchant_api() -> Result<EnchantApi, String> {
    let mut handle = std::ptr::null_mut();
    for name in [c"libenchant-2.so.2", c"libenchant.so.1"] {
        // SAFETY: a NUL-terminated library name. The library stays loaded for the process.
        handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if !handle.is_null() {
            break;
        }
    }
    if handle.is_null() {
        return Err("this computer has no Enchant library (libenchant-2.so.2)".into());
    }
    let missing = || "the Enchant library lacks a function DSA uses".to_string();
    // SAFETY: each symbol is looked up with the type Enchant's header declares for it.
    unsafe {
        Ok(EnchantApi {
            broker_init: symbol(handle, c"enchant_broker_init").ok_or_else(missing)?,
            broker_free: symbol(handle, c"enchant_broker_free").ok_or_else(missing)?,
            request_dict: symbol(handle, c"enchant_broker_request_dict").ok_or_else(missing)?,
            dict_exists: symbol(handle, c"enchant_broker_dict_exists").ok_or_else(missing)?,
            dict_check: symbol(handle, c"enchant_dict_check").ok_or_else(missing)?,
            dict_suggest: symbol(handle, c"enchant_dict_suggest").ok_or_else(missing)?,
            free_string_list: symbol(handle, c"enchant_dict_free_string_list")
                .ok_or_else(missing)?,
        })
    }
}

/// The Enchant tags to try for `tag`, best first: "en_US", then "en".
pub fn enchant_tags(tag: &str) -> Vec<String> {
    let normal = normalize_tag(tag);
    let mut tags = vec![normal.replace('-', "_")];
    let language = primary_language(&normal);
    if !language.is_empty() && !tags.contains(&language) {
        tags.push(language);
    }
    tags
}

/// Enchant's broker and the dictionaries asked of it, kept while the program runs.
#[derive(Default)]
struct Enchant {
    api: Option<EnchantApi>,
    broker: Option<usize>,
    dictionaries: std::collections::HashMap<String, Option<usize>>,
}

impl Enchant {
    fn broker(&mut self) -> Result<(&EnchantApi, *mut c_void), String> {
        if self.api.is_none() {
            self.api = Some(enchant_api()?);
        }
        let api = self.api.as_ref().expect("loaded above");
        if self.broker.is_none() {
            // SAFETY: Enchant's own constructor.
            let broker = unsafe { (api.broker_init)() };
            if broker.is_null() {
                return Err("Enchant could not start".into());
            }
            self.broker = Some(broker as usize);
        }
        Ok((api, self.broker.expect("made above") as *mut c_void))
    }

    fn dictionary(&mut self, tag: &str) -> Result<Option<*mut c_void>, String> {
        let key = normalize_tag(tag);
        if let Some(found) = self.dictionaries.get(&key) {
            return Ok(found.map(|d| d as *mut c_void));
        }
        let (api, broker) = self.broker()?;
        let mut found = None;
        for candidate in enchant_tags(&key) {
            let Ok(name) = CString::new(candidate) else {
                continue;
            };
            // SAFETY: the broker is live and the tag NUL-terminated.
            if unsafe { (api.dict_exists)(broker, name.as_ptr()) } == 0 {
                continue;
            }
            // SAFETY: as above; the dictionary stays with the broker while the program runs.
            let dictionary = unsafe { (api.request_dict)(broker, name.as_ptr()) };
            if !dictionary.is_null() {
                found = Some(dictionary as usize);
                break;
            }
        }
        self.dictionaries.insert(key, found);
        Ok(found.map(|d| d as *mut c_void))
    }
}

impl Drop for Enchant {
    fn drop(&mut self) {
        if let (Some(api), Some(broker)) = (&self.api, self.broker.take()) {
            // SAFETY: frees the broker, and with it every dictionary it gave.
            unsafe { (api.broker_free)(broker as *mut c_void) };
        }
    }
}

impl SpellChecker for Enchant {
    fn check(
        &mut self,
        language_tag: &str,
        text: &str,
        max_suggestions: usize,
    ) -> Result<Option<Vec<Mistake>>, String> {
        let tag = if language_tag.trim().is_empty() {
            std::env::var("LANG")
                .ok()
                .and_then(|lang| lang.split('.').next().map(str::to_string))
                .unwrap_or_else(|| "en_US".into())
        } else {
            language_tag.to_string()
        };
        let Some(dictionary) = self.dictionary(&tag)? else {
            return Ok(None);
        };
        let api = self.api.as_ref().expect("loaded with the dictionary");
        let mut mistakes = Vec::new();
        for word in words(text) {
            let length = word.text.len() as isize;
            let pointer = word.text.as_ptr().cast::<c_char>();
            // SAFETY: the word's bytes with their length; Enchant reads no further.
            let checked = unsafe { (api.dict_check)(dictionary, pointer, length) };
            if checked <= 0 {
                continue;
            }
            let mut count = 0usize;
            // SAFETY: as above; the list is freed below with the dictionary's own call.
            let list = unsafe { (api.dict_suggest)(dictionary, pointer, length, &mut count) };
            let mut suggestions = Vec::new();
            if !list.is_null() {
                for i in 0..count.min(max_suggestions) {
                    // SAFETY: Enchant gave `count` NUL-terminated strings.
                    let item = unsafe { *list.add(i) };
                    if !item.is_null() {
                        // SAFETY: as above.
                        let text = unsafe { CStr::from_ptr(item) };
                        suggestions.push(text.to_string_lossy().into_owned());
                    }
                }
                // SAFETY: the list Enchant gave, freed once.
                unsafe { (api.free_string_list)(dictionary, list) };
            }
            mistakes.push(Mistake {
                start: word.start,
                length: word.length,
                suggestions,
                recommended: false,
            });
        }
        Ok(Some(mistakes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_rate_and_pitch_become_ssips() {
        assert_eq!(ssip_rate(1.0), 0);
        assert_eq!(ssip_rate(4.0), 100);
        assert_eq!(ssip_rate(0.25), -100);
        assert_eq!(ssip_rate(2.5), 50);
        assert_eq!(ssip_pitch(1.0), 0);
        assert_eq!(ssip_pitch(1.5), 50);
        assert_eq!(ssip_pitch(0.25), -100);
        assert_eq!(ssip_rate(f32::NAN), 0);
    }

    #[test]
    fn fr36_a_text_is_sent_to_speech_dispatcher_as_one_block() {
        assert_eq!(ssip_block("Hello.\n.hidden"), "Hello.\r\n..hidden\r\n.\r\n");
    }

    #[test]
    fn fr36_speech_dispatchers_voices_are_androids() {
        let voices = ssip_voices(&[
            "Alan\ten_GB\tnone".to_string(),
            "kal_diphone\ten\tnone".to_string(),
            "\t\t".to_string(),
        ]);
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "Alan");
        assert_eq!(voices[0].language_tag, "en-GB");
        assert_eq!(voices[1].language_tag, "en");
    }

    #[test]
    fn fr36_ssip_lines_are_read_with_their_codes() {
        assert_eq!(ssip_line("225-21\r\n"), Some((225, "21", false)));
        assert_eq!(
            ssip_line("225 OK MESSAGE QUEUED\r\n"),
            Some((225, "OK MESSAGE QUEUED", true))
        );
        assert_eq!(ssip_line("garbage"), None);
    }

    #[test]
    fn fr36_enchant_is_asked_for_the_region_then_the_language() {
        assert_eq!(enchant_tags("en-US"), ["en_US", "en"]);
        assert_eq!(enchant_tags("de"), ["de"]);
    }
}

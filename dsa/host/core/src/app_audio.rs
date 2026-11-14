//! Each app's sound under its own name and volume on the host, and the host as the one place
//! the volume is set.
//!
//! Android has a media volume of its own, and a host has its own volume too. When both turn
//! the sound down it is turned down twice: at Android's media volume of 10 out of 15 the sound
//! left the guest some 15 dB down, and the host's volume had to stand three times higher than
//! for its own programs. So the guest's audio module sends every output at full scale, and the
//! volume lives on the host alone.
//!
//! The audio module gives each app with a window an output of its own (`app 0`, `app 1`, ...),
//! and plays every other sound on the shared output ([`PRIMARY_STREAM`]). The agent says which
//! app each output plays and the volume Android gives that app ([`checked_streams`]). The
//! session keeps that list for the local API ([`AppAudioHub`]), and the platform that plays the
//! sound follows it ([`follow`]) into an [`AppVolumes`]: each output plays as its app, under the
//! app's name and icon, at the app's volume. When the user changes an app's volume or mute in
//! the host's own mixer, the platform tells [`AppVolumes::host_changed`], and the change goes
//! back to the agent ([`VolumeChange`]), which makes it the volume Android's apps read.
//!
//! Nothing here names a host operating system API, and nothing the guest sends can take the
//! host down: a list past the limits is cut, a name that is not one is dropped, and a volume
//! out of range is brought into it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use dsa_protocol::control;
use dsa_protocol::host_api::types;
use dsa_transport::{Connection, Connector, TransportError};
use sha2::{Digest, Sha256};

use crate::background::valid_package;

/// The output every sound of an app without one of its own plays on.
pub const PRIMARY_STREAM: &str = "primary";

/// The most outputs the host keeps. The guest has a handful; past this they are dropped.
pub const MAX_STREAMS: usize = 32;

/// The longest output name the host takes, in bytes.
pub const MAX_STREAM_NAME_BYTES: usize = 64;

/// The longest app name the host keeps, in characters. A volume mixer shows one line of it.
pub const MAX_LABEL_CHARS: usize = 256;

/// How far apart two volumes must be to count as different: half a percent. A volume goes
/// through Android's steps (15 by default) and the host's mixer's own rounding on its way
/// round, and a change smaller than this is that rounding, not the user.
pub const VOLUME_TOLERANCE: f32 = 0.005;

/// The name DSA's shared output plays under on the host.
pub const SHARED_LABEL: &str = "Desktop Subsystem for Android";

/// What one output of the guest plays.
#[derive(Debug, Clone, PartialEq)]
pub struct AppStream {
    /// The output, as the audio port names it.
    pub stream: String,
    /// Empty for the shared output.
    pub package_name: String,
    /// Empty for the shared output.
    pub app_label: String,
    /// From 0 (silent) to 1 (full).
    pub volume: f32,
    pub muted: bool,
}

impl AppStream {
    /// What `stream` plays while the guest has said nothing of it: the shared output's sound,
    /// at full volume, as Android's own mixer would leave it.
    pub fn unknown(stream: &str) -> AppStream {
        AppStream {
            stream: stream.to_string(),
            package_name: String::new(),
            app_label: String::new(),
            volume: 1.0,
            muted: false,
        }
    }

    /// Whether it is the shared output rather than one app's.
    pub fn is_shared(&self) -> bool {
        self.package_name.is_empty()
    }

    /// The name the host's mixer shows for it.
    pub fn display_name(&self) -> &str {
        if self.is_shared() || self.app_label.is_empty() {
            if self.is_shared() {
                SHARED_LABEL
            } else {
                &self.package_name
            }
        } else {
            &self.app_label
        }
    }

    /// Whether `other` is the same app on the same output, whatever its volume.
    pub fn same_app(&self, other: &AppStream) -> bool {
        self.stream == other.stream
            && self.package_name == other.package_name
            && self.app_label == other.app_label
    }

    /// Whether `other` has the same volume and mute, within [`VOLUME_TOLERANCE`].
    pub fn same_volume(&self, other: &AppStream) -> bool {
        same_volume(self.volume, other.volume) && self.muted == other.muted
    }

    /// The output as the local API carries it.
    pub fn to_host_api(&self) -> types::AppAudioStream {
        types::AppAudioStream {
            stream: self.stream.clone(),
            package_name: self.package_name.clone(),
            app_label: self.app_label.clone(),
            volume: self.volume,
            muted: self.muted,
        }
    }
}

/// Whether two volumes are the same within [`VOLUME_TOLERANCE`].
pub fn same_volume(a: f32, b: f32) -> bool {
    (a - b).abs() < VOLUME_TOLERANCE
}

/// `volume` brought into 0 to 1. A value that is not a number is full volume, which is what
/// the app itself plays at.
pub fn clamp_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        1.0
    } else {
        volume.clamp(0.0, 1.0)
    }
}

/// Whether `name` could be an output's name: 1 to [`MAX_STREAM_NAME_BYTES`] bytes of letters,
/// digits, spaces, `_` and `-`.
pub fn valid_stream_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_STREAM_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'_' || b == b'-')
}

fn cut(text: String, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text;
    }
    text.chars().take(chars).collect()
}

fn checked_one(
    stream: String,
    package_name: String,
    app_label: String,
    volume: f32,
    muted: bool,
) -> Result<AppStream, String> {
    if !valid_stream_name(&stream) {
        return Err(format!(
            "an output whose name is not one ({} bytes)",
            stream.len()
        ));
    }
    if !package_name.is_empty() && !valid_package(&package_name) {
        return Err(format!(
            "output {stream} names a package that is not one ({} bytes)",
            package_name.len()
        ));
    }
    let app_label = if package_name.is_empty() {
        String::new()
    } else {
        cut(app_label, MAX_LABEL_CHARS)
    };
    Ok(AppStream {
        stream,
        package_name,
        app_label,
        volume: clamp_volume(volume),
        muted,
    })
}

/// Keeps the first of each output, at most [`MAX_STREAMS`].
fn bounded(streams: impl IntoIterator<Item = Result<AppStream, String>>) -> Vec<AppStream> {
    let mut kept: Vec<AppStream> = Vec::new();
    for stream in streams {
        match stream {
            Ok(stream) if kept.iter().any(|known| known.stream == stream.stream) => {
                eprintln!("app audio: output {} was listed twice", stream.stream)
            }
            Ok(_) if kept.len() >= MAX_STREAMS => {
                eprintln!("app audio: dropped an output; the host keeps at most {MAX_STREAMS}");
            }
            Ok(stream) => kept.push(stream),
            Err(why) => eprintln!("app audio: dropped {why}"),
        }
    }
    kept
}

/// The outputs the agent listed, checked: a bad entry is dropped, a label is cut, a volume is
/// brought into range, and the list is cut to [`MAX_STREAMS`].
pub fn checked_streams(message: control::AppAudio) -> Vec<AppStream> {
    bounded(
        message
            .streams
            .into_iter()
            .map(|s| checked_one(s.stream, s.package_name, s.app_label, s.volume, s.muted)),
    )
}

/// The outputs the local API carried, checked as the agent's are.
pub fn streams_from_host_api(message: types::AppAudio) -> Vec<AppStream> {
    bounded(
        message
            .streams
            .into_iter()
            .map(|s| checked_one(s.stream, s.package_name, s.app_label, s.volume, s.muted)),
    )
}

/// The outputs as the local API carries them.
pub fn streams_to_host_api(streams: &[AppStream]) -> types::AppAudio {
    types::AppAudio {
        streams: streams.iter().map(AppStream::to_host_api).collect(),
    }
}

/// A volume or a mute the user set on the host, for an app (or, with no package, the shared
/// output).
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeChange {
    pub package_name: String,
    pub volume: f32,
    pub muted: bool,
}

impl VolumeChange {
    /// The change as the local API carries it.
    pub fn to_host_api(&self) -> types::SetAppVolume {
        types::SetAppVolume {
            package_name: self.package_name.clone(),
            volume: self.volume,
            muted: self.muted,
        }
    }

    /// The change the local API carried, checked.
    pub fn from_host_api(message: types::SetAppVolume) -> Result<VolumeChange, String> {
        if !message.package_name.is_empty() && !valid_package(&message.package_name) {
            return Err("an app volume for a package name that is not one".into());
        }
        Ok(VolumeChange {
            package_name: message.package_name,
            volume: clamp_volume(message.volume),
            muted: message.muted,
        })
    }

    /// The change as the agent takes it.
    pub fn to_control(&self) -> control::SetAppVolume {
        control::SetAppVolume {
            package_name: self.package_name.clone(),
            volume: self.volume,
            muted: self.muted,
        }
    }
}

/// The outputs now, and every list after them.
pub struct AppAudioWatch {
    pub streams: Vec<AppStream>,
    pub events: Receiver<Vec<AppStream>>,
}

/// The session's copy of the agent's list, with who watches it.
#[derive(Default)]
pub struct AppAudioHub {
    streams: Vec<AppStream>,
    watchers: Vec<Sender<Vec<AppStream>>>,
}

impl AppAudioHub {
    /// Takes the agent's new list and hands it to the watchers, unless nothing changed.
    pub fn apply(&mut self, streams: Vec<AppStream>) {
        if streams == self.streams {
            return;
        }
        self.streams = streams;
        let list = self.streams.clone();
        self.watchers
            .retain(|watcher| watcher.send(list.clone()).is_ok());
    }

    /// Empties the list, as when the guest stops.
    pub fn clear(&mut self) {
        self.apply(Vec::new());
    }

    /// The outputs now.
    pub fn streams(&self) -> Vec<AppStream> {
        self.streams.clone()
    }

    /// The outputs now, and a receiver of every list after them.
    pub fn watch(&mut self) -> AppAudioWatch {
        let (tx, events) = mpsc::channel();
        self.watchers.push(tx);
        AppAudioWatch {
            streams: self.streams(),
            events,
        }
    }
}

/// One output as the platform that plays it knows it, and the revision it was last changed
/// at.
struct Known {
    app: AppStream,
    revision: u64,
}

#[derive(Default)]
struct VolumesState {
    streams: BTreeMap<String, Known>,
    revision: u64,
    /// Whether the guest's list has arrived since the watch started.
    listed: bool,
}

/// The platform's view of what each output plays and how loud: the guest's latest list, and
/// the volumes the user set on the host since. The platform's player reads it for its output
/// ([`AppVolumes::app_of`]) each time it wakes, and opens its session as the app, or sets the
/// session's volume, when the revision changed.
pub struct AppVolumes {
    state: Mutex<VolumesState>,
    changes: Mutex<Option<Sender<VolumeChange>>>,
}

impl Default for AppVolumes {
    fn default() -> Self {
        AppVolumes {
            state: Mutex::new(VolumesState::default()),
            changes: Mutex::new(None),
        }
    }
}

impl AppVolumes {
    pub fn new() -> Arc<AppVolumes> {
        Arc::new(AppVolumes::default())
    }

    fn state(&self) -> MutexGuard<'_, VolumesState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the volumes the user sets on the host go from now on. Each one goes once.
    pub fn changes(&self) -> Receiver<VolumeChange> {
        let (tx, rx) = mpsc::channel();
        *self.changes.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }

    /// Takes the guest's whole list. Returns the outputs whose volume or mute the guest
    /// changed, on outputs that play the same app as before: those are Android's own changes
    /// (an app set its volume, or a volume key was pressed). The first list after a watch
    /// starts changes nothing that way, since nothing came before it.
    pub fn update(&self, streams: Vec<AppStream>) -> Vec<AppStream> {
        let mut guard = self.state();
        let state = &mut *guard;
        let listed = state.listed;
        state.listed = true;
        let mut changed = Vec::new();
        let names: Vec<String> = streams.iter().map(|s| s.stream.clone()).collect();
        for app in streams {
            let revision = state.revision + 1;
            match state.streams.get_mut(&app.stream) {
                Some(known) if known.app == app => {}
                Some(known) => {
                    if listed && known.app.same_app(&app) && !known.app.same_volume(&app) {
                        changed.push(app.clone());
                    }
                    known.app = app;
                    known.revision = revision;
                    state.revision = revision;
                }
                None => {
                    state
                        .streams
                        .insert(app.stream.clone(), Known { app, revision });
                    state.revision = revision;
                }
            }
        }
        let gone: Vec<String> = state
            .streams
            .keys()
            .filter(|name| !names.contains(name))
            .cloned()
            .collect();
        for name in gone {
            state.streams.remove(&name);
            state.revision += 1;
        }
        changed
    }

    /// Forgets the list, as when the watch of it ended. Each output plays as the shared one
    /// until the next list.
    pub fn forget(&self) {
        let mut state = self.state();
        if !state.streams.is_empty() {
            state.streams.clear();
            state.revision += 1;
        }
        state.listed = false;
    }

    /// What `stream` plays and its volume, and the revision to compare with the next time. An
    /// output the guest has not listed plays as the shared one at full volume, under revision
    /// 0.
    pub fn app_of(&self, stream: &str) -> (AppStream, u64) {
        let state = self.state();
        match state.streams.get(stream) {
            Some(known) => (known.app.clone(), known.revision),
            None => (AppStream::unknown(stream), 0),
        }
    }

    /// Every output known now.
    pub fn streams(&self) -> Vec<AppStream> {
        self.state()
            .streams
            .values()
            .map(|known| known.app.clone())
            .collect()
    }

    /// The user set `stream`'s volume or mute in the host's own mixer. It is kept as that
    /// output's volume, and goes to the guest unless it is what the guest already has (a
    /// change the platform made itself, coming back). Returns whether it went.
    pub fn host_changed(&self, stream: &str, volume: f32, muted: bool) -> bool {
        let volume = clamp_volume(volume);
        let change = {
            let mut state = self.state();
            let Some(known) = state.streams.get_mut(stream) else {
                // The guest has not said what plays there; there is nobody to tell.
                return false;
            };
            if same_volume(known.app.volume, volume) && known.app.muted == muted {
                return false;
            }
            // Kept without a new revision: the platform's session already has it.
            known.app.volume = volume;
            known.app.muted = muted;
            VolumeChange {
                package_name: known.app.package_name.clone(),
                volume,
                muted,
            }
        };
        let changes = self.changes.lock().unwrap_or_else(|e| e.into_inner());
        match changes.as_ref() {
            Some(tx) => tx.send(change).is_ok(),
            None => false,
        }
    }
}

/// A session identity for an app's sound that stays the same for the app across runs: the
/// first 16 bytes of SHA-256 over a DSA prefix and the package name (none for the shared
/// output). A host that remembers a session's place in its mixer by an identity keeps it.
pub fn session_id(package_name: &str) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"dsa app audio session\0");
    hasher.update(package_name.as_bytes());
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    // An RFC 4122 identity of the name-based kind, so a host that checks the form takes it.
    id[6] = (id[6] & 0x0f) | 0x50;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);
/// How long the watch waits for a list before it looks at `stop` and the host's changes.
const POLL: Duration = Duration::from_millis(50);
/// How long the follower waits before it connects again.
const RETRY: Duration = Duration::from_secs(2);

/// Follows the caller's Android's outputs into `volumes` until `stop` is set, and sends the
/// volumes the user sets on the host to the service. `guest_changed` hears each output whose
/// volume Android changed, for a host that has no mixer of its own per app. A lost
/// connection is made again; while there is none, every output plays as the shared one.
pub fn follow(
    connector: &mut dyn Connector,
    build: &str,
    volumes: &AppVolumes,
    stop: &AtomicBool,
    log: &dyn Fn(&str),
    guest_changed: &dyn Fn(&[AppStream]),
) {
    let changes = volumes.changes();
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let watch = crate::host_api::HostApiClient::connect(connector, build, ANSWER)
            .and_then(|client| client.watch_app_audio());
        let mut watch = match watch {
            Ok(watch) => watch,
            Err(why) => {
                if !warned {
                    log(&format!(
                        "app audio: cannot follow Android's app volumes yet: {why}"
                    ));
                    warned = true;
                }
                // What the user sets meanwhile has nowhere to go.
                while changes.try_recv().is_ok() {}
                wait(stop, RETRY);
                continue;
            }
        };
        if warned {
            log("app audio: following Android's app volumes");
        }
        warned = false;
        let first = volumes.update(watch.streams.clone());
        if !first.is_empty() {
            guest_changed(&first);
        }
        let mut sender: Option<crate::host_api::HostApiClient> = None;
        while !stop.load(Ordering::Relaxed) {
            match watch.next(POLL) {
                Ok(None) => {}
                Ok(Some(streams)) => {
                    let changed = volumes.update(streams);
                    if !changed.is_empty() {
                        guest_changed(&changed);
                    }
                }
                Err(why) => {
                    log(&format!("app audio: the service connection ended: {why}"));
                    break;
                }
            }
            // Only the newest change of each app goes: a slider being dragged makes many.
            let mut latest: Vec<VolumeChange> = Vec::new();
            while let Ok(change) = changes.try_recv() {
                latest.retain(|known| known.package_name != change.package_name);
                latest.push(change);
            }
            for change in latest {
                if sender.is_none() {
                    sender = crate::host_api::HostApiClient::connect(connector, build, ANSWER)
                        .map_err(|why| {
                            log(&format!(
                                "app audio: the volume set on the host did not reach Android: \
                                 {why}"
                            ))
                        })
                        .ok();
                }
                if let Some(client) = sender.as_mut() {
                    if let Err(why) = client.set_app_volume(&change) {
                        log(&format!(
                            "app audio: the volume set on the host did not reach Android: {why}"
                        ));
                        sender = None;
                    }
                }
            }
        }
        volumes.forget();
        wait(stop, RETRY);
    }
}

/// Sleeps `time`, or less once `stop` is set.
fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        waited += step;
    }
}

/// The outputs of the caller's Android, from [`crate::host_api::HostApiClient::watch_app_audio`].
pub struct AppAudioStream {
    /// The outputs when the watch started.
    pub streams: Vec<AppStream>,
    pub(crate) connection: Box<dyn Connection>,
}

impl AppAudioStream {
    /// The next whole list, waiting at most `timeout`. `Ok(None)` means none came in time; an
    /// error means the connection has ended or the service sent something else.
    pub fn next(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<AppStream>>, crate::host_api::ClientError> {
        use crate::host_api::ClientError;
        use dsa_protocol::host_api::types::{envelope::Body, Envelope};
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match dsa_protocol::host_api::decode(&frame) {
            Ok(Envelope {
                body: Some(Body::AppAudio(list)),
            }) => Ok(Some(streams_from_host_api(list))),
            Ok(_) => Err(ClientError::Malformed(
                "expected the outputs of Android's sound".into(),
            )),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(stream: &str, package: &str, volume: f32) -> control::AppAudioStream {
        control::AppAudioStream {
            stream: stream.into(),
            package_name: package.into(),
            app_label: if package.is_empty() {
                String::new()
            } else {
                "Music".into()
            },
            volume,
            muted: false,
        }
    }

    #[test]
    fn fr26_a_bad_output_is_dropped_and_a_volume_is_brought_into_range() {
        let streams = checked_streams(control::AppAudio {
            streams: vec![
                entry("app 0", "com.example.music", 1.5),
                entry("app 1", "bad name", 0.5),
                entry("../x", "com.example.other", 0.5),
                entry("app 0", "com.example.again", 0.5),
                entry(PRIMARY_STREAM, "", -1.0),
                entry("app 2", "com.example.nan", f32::NAN),
            ],
        });
        assert_eq!(streams.len(), 3);
        assert_eq!(streams[0].volume, 1.0);
        assert_eq!(streams[0].package_name, "com.example.music");
        assert_eq!(streams[1].volume, 0.0);
        assert!(streams[1].is_shared());
        assert_eq!(streams[1].display_name(), SHARED_LABEL);
        assert_eq!(streams[2].volume, 1.0);
    }

    #[test]
    fn fr26_the_list_is_cut_to_the_limit() {
        let streams = checked_streams(control::AppAudio {
            streams: (0..MAX_STREAMS + 4)
                .map(|i| entry(&format!("app {i}"), "com.example.music", 1.0))
                .collect(),
        });
        assert_eq!(streams.len(), MAX_STREAMS);
    }

    #[test]
    fn fr26_the_hub_tells_watchers_of_changes_only() {
        let mut hub = AppAudioHub::default();
        let watch = hub.watch();
        assert!(watch.streams.is_empty());
        let list = checked_streams(control::AppAudio {
            streams: vec![entry("app 0", "com.example.music", 0.5)],
        });
        hub.apply(list.clone());
        hub.apply(list.clone());
        assert_eq!(watch.events.try_recv().unwrap(), list);
        assert!(watch.events.try_recv().is_err());
        hub.clear();
        assert!(watch.events.try_recv().unwrap().is_empty());
    }

    #[test]
    fn fr26_an_unlisted_output_plays_as_the_shared_one_at_full_volume() {
        let volumes = AppVolumes::default();
        let (app, revision) = volumes.app_of("app 3");
        assert!(app.is_shared());
        assert_eq!(app.volume, 1.0);
        assert_eq!(revision, 0);
    }

    #[test]
    fn fr26_the_guests_change_of_volume_is_told_apart_from_a_new_app() {
        let volumes = AppVolumes::default();
        let list = |package: &str, volume: f32| {
            checked_streams(control::AppAudio {
                streams: vec![entry("app 0", package, volume)],
            })
        };
        // The first list is where things stand, not a change.
        assert!(volumes.update(list("com.example.music", 0.5)).is_empty());
        let (_, first) = volumes.app_of("app 0");
        // Android changed the app's volume.
        let changed = volumes.update(list("com.example.music", 0.25));
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].volume, 0.25);
        let (app, second) = volumes.app_of("app 0");
        assert!(second > first);
        assert_eq!(app.volume, 0.25);
        // Another app on the output is not a change of volume.
        assert!(volumes.update(list("com.example.video", 1.0)).is_empty());
        // The output gone: it plays as the shared one again.
        volumes.update(Vec::new());
        assert!(volumes.app_of("app 0").0.is_shared());
    }

    #[test]
    fn fr26_a_volume_set_on_the_host_goes_to_the_guest_once() {
        let volumes = AppVolumes::default();
        let changes = volumes.changes();
        volumes.update(checked_streams(control::AppAudio {
            streams: vec![entry("app 0", "com.example.music", 0.5)],
        }));
        let (_, revision) = volumes.app_of("app 0");
        // What the guest already has is the platform's own change coming back.
        assert!(!volumes.host_changed("app 0", 0.502, false));
        assert!(volumes.host_changed("app 0", 0.8, false));
        assert_eq!(
            changes.try_recv().unwrap(),
            VolumeChange {
                package_name: "com.example.music".into(),
                volume: 0.8,
                muted: false,
            }
        );
        // Kept, without a revision the session would apply again.
        let (app, after) = volumes.app_of("app 0");
        assert_eq!(app.volume, 0.8);
        assert_eq!(after, revision);
        assert!(volumes.host_changed("app 0", 0.8, true));
        assert!(changes.try_recv().unwrap().muted);
        // An output the guest has not listed has nobody to tell.
        assert!(!volumes.host_changed("app 5", 0.3, false));
        // The guest's echo of the host's change is no change of its own.
        let echo = volumes.update(checked_streams(control::AppAudio {
            streams: vec![control::AppAudioStream {
                muted: true,
                ..entry("app 0", "com.example.music", 0.8)
            }],
        }));
        assert!(echo.is_empty());
    }

    #[test]
    fn fr26_an_apps_session_identity_stays_the_same() {
        assert_eq!(
            session_id("com.example.music"),
            session_id("com.example.music")
        );
        assert_ne!(session_id("com.example.music"), session_id(""));
        assert_eq!(session_id("x")[6] >> 4, 5);
    }

    #[test]
    fn fr26_a_change_from_the_local_api_is_checked() {
        assert!(VolumeChange::from_host_api(types::SetAppVolume {
            package_name: "bad name".into(),
            volume: 0.5,
            muted: false,
        })
        .is_err());
        let change = VolumeChange::from_host_api(types::SetAppVolume {
            package_name: String::new(),
            volume: 7.0,
            muted: true,
        })
        .unwrap();
        assert_eq!(change.volume, 1.0);
        assert_eq!(change.to_control().package_name, "");
    }
}

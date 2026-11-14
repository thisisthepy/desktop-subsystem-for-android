//! The Mac's devices and settings, lent to the apps in the user's Android.
//!
//! Each device has a platform-neutral half in `dsa-host-core` (the guest's port, the rules,
//! the messages) and a Mac half here that reaches the framework for it:
//!
//! ```text
//! sound out and in   Audio Queue Services             [`audio`]
//! sound volume       the Mac's volume                 [`system_volume`]
//! video decoding     VideoToolbox                     [`video`]
//! cameras            AVFoundation                     [`camera`]
//! sensors            none a program may read          [`sensors`]
//! location           CoreLocation                     [`location`]
//! printing           PDFKit and NSPrintOperation      [`print`], [`print_jobs`]
//! passkeys           AuthenticationServices           `passkeys`, [`passkey_ceremonies`]
//! web links          /usr/bin/open                    [`links`]
//! controllers        the GameController framework     [`gamepads`], [`front`]
//! permissions        AVFoundation's prompts           [`permissions`]
//! settings           time zone, languages, format,    [`settings`]
//!                    dark mode and accent color
//! folders            the user's home folders          [`shared_folders`]
//! speech             AVSpeechSynthesizer and          [`language`]
//!                    SFSpeechRecognizer
//! spelling           NSSpellChecker                   [`language`]
//! address lookups    the boundaries in the bundle     [`geocode`]
//! sharing            NSSharingServicePicker           [`share`]
//! ```
//!
//! The devices run in the user's device lender (`dsa-devices`), a launchd agent of its own in
//! the user's login session, where macOS asks the user for the camera, the microphone and the
//! location. [`start_guest_ports`] serves the guest's device ports, which `dsa-hostd` relays
//! from the VM to sockets in the user's DSA folder; [`start_followers`] follows the local API
//! for the rest. The folders belong to `dsa-hostd`, which runs as the user and serves them
//! with the user's own rights.
//!
//! Nothing here takes the host down. A device the Mac does not have, or one the user did not
//! allow, is logged; the guest sees no such device, and everything else goes on.

pub mod camera;
pub mod front;
pub mod gamepads;
pub mod geocode;
pub mod language;
pub mod links;
pub mod location;
pub mod passkey_ceremonies;
pub mod permissions;
pub mod print_jobs;
pub mod sensors;
pub mod settings;
pub mod share;
pub mod shared_folders;
pub mod system_volume;
pub mod video;

#[cfg(target_os = "macos")]
pub mod audio;
#[cfg(target_os = "macos")]
pub mod passkeys;
#[cfg(target_os = "macos")]
pub mod print;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dsa_transport::Connector;

/// Where a device writes what it has to say: one line each.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Makes a new connection to the local API for each follower.
pub type ConnectorFactory = Arc<dyn Fn() -> Box<dyn Connector> + Send + Sync>;

/// The build each device names itself by.
pub fn build(device: &str) -> String {
    format!("dsa-devices {} {device}", crate::VERSION)
}

/// Waits `time`, or less when `stop` is set.
pub(crate) fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        thread::sleep(step);
        waited += step;
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn spawn(name: &str, log: &Log, run: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    match thread::Builder::new().name(name.into()).spawn(run) {
        Ok(handle) => Some(handle),
        Err(error) => {
            log(&format!("could not start {name}: {error}"));
            None
        }
    }
}

/// Which of the lender's parts to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Followers {
    pub location: bool,
    pub print: bool,
    pub links: bool,
    pub settings: bool,
    pub gamepads: bool,
    pub passkeys: bool,
    /// Speech synthesis, speech recognition and spell checking for Android.
    pub language: bool,
    /// The address lookups of the apps in Android.
    pub geocoding: bool,
    /// What the apps in Android share with the Mac.
    pub share: bool,
}

impl Default for Followers {
    fn default() -> Followers {
        Followers {
            location: true,
            print: true,
            links: true,
            settings: true,
            gamepads: true,
            passkeys: true,
            language: true,
            geocoding: true,
            share: true,
        }
    }
}

#[cfg(target_os = "macos")]
pub use lender::{start_followers, start_guest_ports};

#[cfg(target_os = "macos")]
mod lender {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use dsa_host_unix::device_socket::DeviceSocketListener;
    use dsa_transport::Listener;

    use super::{build, spawn, ConnectorFactory, Followers, Log};
    use crate::host_config::{relay_socket, UserFolders};

    /// Listens on the socket `dsa-hostd` relays the guest's `port` to, or logs why it cannot.
    fn listen(user: &UserFolders, port: u32, what: &str, log: &Log) -> Option<Box<dyn Listener>> {
        let path = relay_socket(user, port);
        match DeviceSocketListener::bind(&path) {
            Ok(listener) => Some(Box::new(listener)),
            Err(error) => {
                log(&format!(
                    "{what} is not lent to Android: {} cannot be opened ({error})",
                    path.display()
                ));
                None
            }
        }
    }

    /// Serves the guest's device ports (sound, video, cameras, sensors) on the sockets in
    /// `user`'s folder until `stop`, each on a thread of its own. A port that cannot be
    /// opened is logged and left out. The guest's sound has the Mac's volume and nothing
    /// else: a thread follows the apps' volumes over the local API through `connect`, and
    /// another the Mac's volume ([`super::system_volume`]).
    pub fn start_guest_ports(
        connect: &ConnectorFactory,
        user: &UserFolders,
        stop: &Arc<AtomicBool>,
        log: &Log,
    ) -> Vec<JoinHandle<()>> {
        let mut threads = Vec::new();

        if let Some(mut listener) = listen(user, dsa_host_core::audio::AUDIO_PORT, "sound", log) {
            let volumes = dsa_host_core::app_audio::AppVolumes::new();
            let one = Arc::new(std::sync::Mutex::new(
                super::system_volume::OneVolume::default(),
            ));
            {
                let (mut connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
                let (volumes, one) = (Arc::clone(&volumes), Arc::clone(&one));
                threads.extend(spawn("dsa-app-audio", &Arc::clone(&log), move || {
                    dsa_host_core::app_audio::follow(
                        &mut *connector,
                        &build("app audio"),
                        &volumes,
                        &stop,
                        &*log,
                        &|changed: &[dsa_host_core::app_audio::AppStream]| {
                            let set = one
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .guest_changed(changed);
                            if let Some((volume, muted)) = set {
                                super::system_volume::set(volume, muted);
                            }
                        },
                    )
                }));
            }
            {
                let stop = Arc::clone(stop);
                let (volumes, one) = (Arc::clone(&volumes), Arc::clone(&one));
                threads.extend(spawn("dsa-mac-volume", &Arc::clone(log), move || {
                    super::system_volume::follow(&volumes, &one, &stop)
                }));
            }
            let (stop, log) = (Arc::clone(stop), Arc::clone(log));
            let device: Arc<dyn dsa_host_core::audio::AudioDevice> =
                Arc::new(super::audio::CoreAudio);
            threads.extend(spawn("dsa-audio", &Arc::clone(&log), move || {
                dsa_host_core::audio::serve_port(
                    &mut *listener,
                    device,
                    &build("audio"),
                    &stop,
                    log,
                )
            }));
        }

        if let Some(listener) = listen(user, dsa_host_core::video::VIDEO_PORT, "video", log) {
            let (stop, log) = (Arc::clone(stop), Arc::clone(log));
            let decoders = Arc::new(super::video::VideoToolboxDecoders);
            threads.extend(spawn("dsa-video", &Arc::clone(&log), move || {
                dsa_host_core::video::serve(listener, decoders, build("video"), stop, log)
            }));
        }

        if let Some(listener) = listen(user, dsa_host_core::camera::CAMERA_PORT, "the cameras", log)
        {
            let (stop, log) = (Arc::clone(stop), Arc::clone(log));
            let cameras = Arc::new(super::camera::MacCameras::new());
            threads.extend(spawn("dsa-camera", &Arc::clone(&log), move || {
                dsa_host_core::camera::serve(listener, cameras, build("camera"), stop, log)
            }));
        }

        if let Some(listener) = listen(
            user,
            dsa_host_core::sensors::SENSOR_PORT,
            "the sensors",
            log,
        ) {
            let (stop, log) = (Arc::clone(stop), Arc::clone(log));
            let sensors = Arc::new(super::sensors::MacSensors);
            threads.extend(spawn("dsa-sensors", &Arc::clone(&log), move || {
                dsa_host_core::sensors::serve(listener, sensors, build("sensors"), stop, log)
            }));
        }

        threads
    }

    /// Follows the local API for the devices it carries (the location, print jobs, passkey
    /// ceremonies, web links, the user's settings and the game controllers) until `stop`,
    /// each on a thread of its own with its own connection.
    pub fn start_followers(
        connect: ConnectorFactory,
        which: Followers,
        stop: &Arc<AtomicBool>,
        log: &Log,
    ) -> Vec<JoinHandle<()>> {
        let mut threads = Vec::new();
        if which.location {
            let (mut connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-location", &Arc::clone(&log), move || {
                let mut provider = super::location::CoreLocation::new();
                let line = move |line: &str| log(line);
                dsa_host_core::location::follow(
                    &mut *connector,
                    &build("location"),
                    &mut provider,
                    &stop,
                    &line,
                )
            }));
        }
        if which.print {
            let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-print", &Arc::clone(&log), move || {
                let print: super::print_jobs::Print = Arc::new(super::print::print);
                super::print_jobs::serve(connector, print, stop, log)
            }));
        }
        if which.passkeys {
            let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-passkeys", &Arc::clone(&log), move || {
                let perform: super::passkey_ceremonies::Perform =
                    Arc::new(super::passkeys::perform);
                super::passkey_ceremonies::serve(connector, perform, stop, log)
            }));
        }
        if which.geocoding {
            let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-geocode", &Arc::clone(&log), move || {
                super::geocode::serve(connector, stop, log)
            }));
        }
        if which.share {
            let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-share", &Arc::clone(&log), move || {
                super::share::serve(connector, stop, log)
            }));
        }
        if which.links {
            let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-links", &Arc::clone(&log), move || {
                super::links::follow(connector, &mut super::links::MacOpener, stop, log)
            }));
        }
        if which.settings {
            let (mut connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-settings", &Arc::clone(&log), move || {
                let mut source = super::settings::MacSettingsSource::new();
                let line = move |line: &str| log(line);
                dsa_host_core::system_settings::follow(
                    &mut *connector,
                    &build("settings"),
                    &mut source,
                    &stop,
                    &line,
                )
            }));
        }
        if which.language {
            let (mut connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-language", &Arc::clone(&log), move || {
                use dsa_host_core::language::server::{serve, Backends};
                use dsa_host_core::language::{SpeechRecognizer, SpeechSynthesizer, SpellChecker};
                let backends = Backends {
                    speech: Box::new(|| {
                        Box::new(super::language::MacSpeech::new()) as Box<dyn SpeechSynthesizer>
                    }),
                    recognition: Box::new(|| {
                        Box::new(super::language::MacRecognition) as Box<dyn SpeechRecognizer>
                    }),
                    spelling: Box::new(|| {
                        Box::new(super::language::MacSpelling) as Box<dyn SpellChecker>
                    }),
                };
                serve(&mut *connector, &build("language"), backends, &stop, log)
            }));
        }
        if which.gamepads {
            let in_front = Arc::new(AtomicBool::new(false));
            {
                let (in_front, stop) = (Arc::clone(&in_front), Arc::clone(stop));
                threads.extend(spawn("dsa-front", log, move || {
                    super::front::watch(in_front, stop)
                }));
            }
            let (mut connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
            threads.extend(spawn("dsa-gamepads", &Arc::clone(&log), move || {
                let mut source = super::gamepads::GameControllers::new();
                let line = move |line: &str| log(line);
                let active = move || in_front.load(std::sync::atomic::Ordering::Relaxed);
                dsa_host_core::gamepads::follow(
                    &mut *connector,
                    &build("gamepads"),
                    &mut source,
                    &active,
                    &stop,
                    &line,
                )
            }));
        }
        threads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr26_the_lender_runs_every_device_unless_told_otherwise() {
        let all = Followers::default();
        assert!(all.location && all.print && all.links && all.settings && all.gamepads);
        assert!(all.passkeys);
        assert!(all.language);
        assert!(all.geocoding && all.share);
        assert!(build("camera").starts_with("dsa-devices "));
        assert!(build("camera").ends_with(" camera"));
    }
}

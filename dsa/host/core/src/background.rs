//! The apps of a user's Android that keep running with no window, and their media sessions.
//!
//! An app that runs a foreground service (music playback, a download, a VPN) keeps running
//! after its last window closes. Android shows such an app in its status bar, which the
//! guest has none of; the host shows it instead, so the user can see that it runs and stop
//! it. The agent says when a package starts and stops running one, with the notification the
//! service keeps shown, and the same for an app's active media session, so the host's media
//! keys reach it.
//!
//! What the guest sends is checked first ([`checked_app`], [`checked_media`]): a package name
//! that is not one, and text, actions or images past the limits here, are cut down or
//! dropped, so a bad guest can neither crash the host nor make it hold more than a bounded
//! amount per app. [`BackgroundHub`] keeps the current apps and sessions and who watches them
//! under one lock, so a watcher's starting point and the changes after it never overlap.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;

use dsa_protocol::control;

/// The longest package name the host takes, in bytes. Android allows no more than 255.
pub const MAX_PACKAGE_BYTES: usize = 255;

/// The longest title, text, label, artist, album or action title the host keeps, in
/// characters. A longer one is cut; a tooltip or a menu shows one line of it.
pub const MAX_LINE_CHARS: usize = 256;

/// The most actions of a notification the host keeps. Android shows at most three; the room
/// is for apps that set more than it shows.
pub const MAX_ACTIONS: usize = 8;

/// The largest app icon the host takes, in bytes of PNG.
pub const MAX_ICON_BYTES: usize = 64 * 1024;

/// The largest album art the host takes, in bytes of PNG. The agent draws it at 256 pixels on
/// a side.
pub const MAX_ART_BYTES: usize = 256 * 1024;

/// The most apps, and separately the most media sessions, the host keeps for one Android. A
/// phone runs a handful; past this a new package is dropped.
pub const MAX_APPS: usize = 64;

/// An app that runs a foreground service, with the notification the service keeps shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackgroundApp {
    pub package_name: String,
    pub app_label: String,
    /// A PNG image; empty for none.
    pub icon_png: Vec<u8>,
    pub title: String,
    pub text: String,
    /// The notification's action titles, in its order.
    pub actions: Vec<String>,
}

impl BackgroundApp {
    /// The name the app goes by: its label, or its package when it has none.
    pub fn display_name(&self) -> &str {
        crate::tasks::app_name(&self.app_label, &self.package_name)
    }
}

/// Where a media session's playback is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Playback {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// An app's active media session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaSession {
    pub package_name: String,
    pub app_label: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// A PNG image; empty for none.
    pub art_png: Vec<u8>,
    pub playback: Playback,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
}

/// A change to the apps that run in the background, as a watcher hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundEvent {
    /// The app is new, or replaces what was known of it.
    App(BackgroundApp),
    /// The package runs no foreground service any more.
    AppRemoved { package_name: String },
    /// The session is new, or replaces the package's earlier one.
    Media(MediaSession),
    /// The package has no active media session any more.
    MediaRemoved { package_name: String },
}

/// What the user does with an app that runs in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundCommand {
    /// Opens its window, as clicking its notification does.
    Open,
    /// Runs the notification's action with this index.
    Action(u32),
    /// Ends the app: Android force-stops its package.
    Stop,
}

/// A command for a media session, as a media key sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

impl MediaCommand {
    /// The command as the control channel carries it.
    pub fn to_control(self) -> control::MediaCommand {
        match self {
            MediaCommand::Play => control::MediaCommand::Play,
            MediaCommand::Pause => control::MediaCommand::Pause,
            MediaCommand::Next => control::MediaCommand::Next,
            MediaCommand::Previous => control::MediaCommand::Previous,
            MediaCommand::Stop => control::MediaCommand::Stop,
        }
    }
}

/// Whether `name` could be an Android package name: 1 to [`MAX_PACKAGE_BYTES`] bytes of
/// letters, digits, `_` and `.`. Anything else is not passed on, as it would end up in a
/// tray icon's identity and in a force-stop.
pub fn valid_package(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PACKAGE_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

fn cut(text: String, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text;
    }
    text.chars().take(chars).collect()
}

fn image(bytes: Vec<u8>, limit: usize, what: &str, package: &str) -> Vec<u8> {
    if bytes.len() > limit {
        eprintln!(
            "background apps: left out {what} of {} bytes from {package}; the limit is {limit}",
            bytes.len()
        );
        return Vec::new();
    }
    bytes
}

/// The app the guest said runs in the background, cut to the limits here, or why it is
/// dropped.
pub fn checked_app(message: control::BackgroundApp) -> Result<BackgroundApp, String> {
    if !valid_package(&message.package_name) {
        return Err(format!(
            "an app whose package name is not one ({} bytes)",
            message.package_name.len()
        ));
    }
    let icon_png = image(
        message.icon_png,
        MAX_ICON_BYTES,
        "an icon",
        &message.package_name,
    );
    let actions: Vec<String> = message
        .actions
        .into_iter()
        .take(MAX_ACTIONS)
        .map(|action| cut(action, MAX_LINE_CHARS))
        .collect();
    Ok(BackgroundApp {
        package_name: message.package_name,
        app_label: cut(message.app_label, MAX_LINE_CHARS),
        icon_png,
        title: cut(message.title, MAX_LINE_CHARS),
        text: cut(message.text, MAX_LINE_CHARS),
        actions,
    })
}

/// The media session the guest sent, cut to the limits here, or why it is dropped.
pub fn checked_media(message: control::MediaSession) -> Result<MediaSession, String> {
    if !valid_package(&message.package_name) {
        return Err(format!(
            "a media session whose package name is not one ({} bytes)",
            message.package_name.len()
        ));
    }
    let playback = match control::MediaPlayback::try_from(message.playback) {
        Ok(control::MediaPlayback::Playing) => Playback::Playing,
        Ok(control::MediaPlayback::Paused) => Playback::Paused,
        _ => Playback::Stopped,
    };
    let art_png = image(
        message.art_png,
        MAX_ART_BYTES,
        "album art",
        &message.package_name,
    );
    Ok(MediaSession {
        package_name: message.package_name,
        app_label: cut(message.app_label, MAX_LINE_CHARS),
        title: cut(message.title, MAX_LINE_CHARS),
        artist: cut(message.artist, MAX_LINE_CHARS),
        album: cut(message.album, MAX_LINE_CHARS),
        art_png,
        playback,
        can_play: message.can_play,
        can_pause: message.can_pause,
        can_next: message.can_next,
        can_previous: message.can_previous,
    })
}

/// The apps and sessions now, and every change to them from then on.
pub struct BackgroundWatch {
    pub apps: Vec<BackgroundApp>,
    pub media: Vec<MediaSession>,
    pub events: std::sync::mpsc::Receiver<BackgroundEvent>,
}

/// The apps that run in the background and the active media sessions, by package, with who
/// watches them.
#[derive(Default)]
pub struct BackgroundHub {
    apps: BTreeMap<String, BackgroundApp>,
    media: BTreeMap<String, MediaSession>,
    watchers: Vec<Sender<BackgroundEvent>>,
}

impl BackgroundHub {
    /// Takes a change and hands it to the watchers, unless it changes nothing. A new package
    /// past [`MAX_APPS`] is dropped; a removal of one the hub does not have is ignored.
    pub fn apply(&mut self, event: BackgroundEvent) {
        let changed = match &event {
            BackgroundEvent::App(app) => {
                Self::put(&mut self.apps, app.package_name.clone(), app.clone())
            }
            BackgroundEvent::AppRemoved { package_name } => {
                self.apps.remove(package_name).is_some()
            }
            BackgroundEvent::Media(session) => Self::put(
                &mut self.media,
                session.package_name.clone(),
                session.clone(),
            ),
            BackgroundEvent::MediaRemoved { package_name } => {
                self.media.remove(package_name).is_some()
            }
        };
        if changed {
            self.watchers
                .retain(|watcher| watcher.send(event.clone()).is_ok());
        }
    }

    fn put<T: PartialEq>(map: &mut BTreeMap<String, T>, package: String, value: T) -> bool {
        match map.get(&package) {
            Some(known) if *known == value => false,
            None if map.len() >= MAX_APPS => {
                eprintln!("background apps: dropped {package}; the host keeps at most {MAX_APPS}");
                false
            }
            _ => {
                map.insert(package, value);
                true
            }
        }
    }

    /// Removes every app and session, as when the guest stops.
    pub fn clear(&mut self) {
        let apps: Vec<String> = self.apps.keys().cloned().collect();
        let media: Vec<String> = self.media.keys().cloned().collect();
        for package_name in media {
            self.apply(BackgroundEvent::MediaRemoved { package_name });
        }
        for package_name in apps {
            self.apply(BackgroundEvent::AppRemoved { package_name });
        }
    }

    /// The apps now, by package name.
    pub fn apps(&self) -> Vec<BackgroundApp> {
        self.apps.values().cloned().collect()
    }

    /// The active media sessions now, by package name.
    pub fn media(&self) -> Vec<MediaSession> {
        self.media.values().cloned().collect()
    }

    pub fn has_app(&self, package_name: &str) -> bool {
        self.apps.contains_key(package_name)
    }

    pub fn has_media(&self, package_name: &str) -> bool {
        self.media.contains_key(package_name)
    }

    /// The apps and sessions now, and a receiver of every change after them.
    pub fn watch(&mut self) -> BackgroundWatch {
        let (tx, events) = std::sync::mpsc::channel();
        self.watchers.push(tx);
        BackgroundWatch {
            apps: self.apps(),
            media: self.media(),
            events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(package: &str, title: &str) -> control::BackgroundApp {
        control::BackgroundApp {
            package_name: package.into(),
            app_label: "Music".into(),
            icon_png: vec![1, 2, 3],
            title: title.into(),
            text: "Artist".into(),
            actions: vec!["Pause".into(), "Next".into()],
        }
    }

    #[test]
    fn fr24_a_package_name_must_be_one() {
        assert!(valid_package("com.example.music"));
        assert!(!valid_package(""));
        assert!(!valid_package("com.example/../x"));
        assert!(!valid_package("a b"));
        assert!(!valid_package(&"a".repeat(MAX_PACKAGE_BYTES + 1)));
        assert!(checked_app(app("bad name", "x")).is_err());
    }

    #[test]
    fn fr24_text_actions_and_images_are_cut_to_the_limits() {
        let mut message = app("com.example.music", &"t".repeat(MAX_LINE_CHARS + 10));
        message.actions = (0..20).map(|i| format!("action {i}")).collect();
        message.icon_png = vec![0; MAX_ICON_BYTES + 1];
        let checked = checked_app(message).unwrap();
        assert_eq!(checked.title.chars().count(), MAX_LINE_CHARS);
        assert_eq!(checked.actions.len(), MAX_ACTIONS);
        assert!(checked.icon_png.is_empty());

        let media = checked_media(control::MediaSession {
            package_name: "com.example.music".into(),
            art_png: vec![0; MAX_ART_BYTES + 1],
            playback: 99,
            ..Default::default()
        })
        .unwrap();
        assert!(media.art_png.is_empty());
        assert_eq!(media.playback, Playback::Stopped);
    }

    #[test]
    fn fr24_the_hub_tells_watchers_of_changes_only() {
        let mut hub = BackgroundHub::default();
        let watch = hub.watch();
        assert!(watch.apps.is_empty());
        let music = checked_app(app("com.example.music", "Song")).unwrap();
        hub.apply(BackgroundEvent::App(music.clone()));
        hub.apply(BackgroundEvent::App(music.clone()));
        assert_eq!(
            watch.events.try_recv().unwrap(),
            BackgroundEvent::App(music)
        );
        assert!(watch.events.try_recv().is_err());

        hub.apply(BackgroundEvent::AppRemoved {
            package_name: "com.example.other".into(),
        });
        assert!(watch.events.try_recv().is_err());

        let later = hub.watch();
        assert_eq!(later.apps.len(), 1);
        hub.clear();
        assert_eq!(
            later.events.try_recv().unwrap(),
            BackgroundEvent::AppRemoved {
                package_name: "com.example.music".into()
            }
        );
        assert!(hub.apps().is_empty());
    }

    #[test]
    fn fr24_the_hub_keeps_a_bounded_number_of_apps() {
        let mut hub = BackgroundHub::default();
        for i in 0..MAX_APPS + 5 {
            let package = format!("com.example.app{i}");
            hub.apply(BackgroundEvent::App(
                checked_app(app(&package, "x")).unwrap(),
            ));
        }
        assert_eq!(hub.apps().len(), MAX_APPS);
    }
}

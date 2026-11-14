//! Applying a downloaded Android image without the user restarting Android.
//!
//! The service downloads a new image while a user's Android runs, and that Android keeps the
//! image it booted from until it starts again. Rather than wait for the user to restart it,
//! the service restarts it into the new image itself, as long as the user's image auto-restart
//! setting is on (it is unless they turned it off):
//!
//! - With no app window open (the Android dozes, or is frozen), it restarts at once, and the
//!   user is told once the new image has started.
//! - With app windows open, the user's desktop asks ([`AlertKind::Restart`], with [Restart now]
//!   and [Later]). [Restart now] restarts it at once; otherwise it restarts the next time every
//!   window is closed and it dozes. Nothing the user is using is closed without asking.
//! - With the setting off, the image waits for the next start, as it always did. The settings
//!   app's [Restart now] still applies it at once.
//!
//! The restart is a stop and a start of the user's Android, so a new image still boots on
//! trial, and a trial that fails still goes back to the previous image.
//!
//! [`ImageRestart`] keeps what this needs for one user's Android: the image it booted from,
//! the setting, the question asked, and the news to tell the user once a restart is over.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::alerts::{AlertKind, Subject, UpdateAlert};

/// One user's Android and the downloaded image it waits to restart into.
pub struct ImageRestart {
    state: Mutex<State>,
    /// Set while a restart into a new image runs.
    restarting: AtomicBool,
}

struct State {
    /// The user's image auto-restart setting.
    auto: bool,
    /// The image the running Android booted from; `None` while it is stopped, or before it
    /// is known.
    booted: Option<String>,
    /// The image the latest boot ran, kept after it stopped: what a stop is about.
    last_booted: Option<String>,
    /// The question asked of the user about the image that waits, while it waits.
    asked: Option<UpdateAlert>,
    /// The news that a restart applied an image which had already passed its trial on
    /// another user's Android, so no trial tells it.
    applied: Option<UpdateAlert>,
    /// The image the user asked to restart into now.
    restart_now: Option<String>,
    /// The keys of the alerts above the user's desktop was told.
    told: HashSet<String>,
}

impl Default for ImageRestart {
    fn default() -> ImageRestart {
        ImageRestart {
            state: Mutex::new(State {
                auto: true,
                booted: None,
                last_booted: None,
                asked: None,
                applied: None,
                restart_now: None,
                told: HashSet::new(),
            }),
            restarting: AtomicBool::new(false),
        }
    }
}

impl ImageRestart {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the Android restarts by itself into a downloaded image.
    pub fn auto(&self) -> bool {
        self.state().auto
    }

    /// The user's image auto-restart setting. Turned off, the question asked is dropped: the
    /// image waits for the next start.
    pub fn set_auto(&self, on: bool) {
        let mut state = self.state();
        state.auto = on;
        if !on {
            state.asked = None;
        }
    }

    /// The image the running Android booted from, when known.
    pub fn booted(&self) -> Option<String> {
        self.state().booted.clone()
    }

    /// The Android booted from `version`, or stopped (`None`).
    pub fn set_booted(&self, version: Option<String>) {
        let mut state = self.state();
        if version.is_some() {
            state.last_booted.clone_from(&version);
        }
        state.booted = version;
    }

    /// The image the latest boot ran, also once it has stopped.
    pub fn last_booted(&self) -> Option<String> {
        self.state().last_booted.clone()
    }

    /// The Android runs, and it booted from `current` unless the image it booted from is
    /// known already. A start that did not come through the updating Android boots the
    /// current image too, so that is what it runs.
    pub fn learn_booted(&self, current: &str) {
        let mut state = self.state();
        if state.booted.is_none() {
            state.booted = Some(current.to_string());
            state.last_booted = Some(current.to_string());
        }
    }

    /// Whether a restart into a new image runs now.
    pub fn restarting(&self) -> bool {
        self.restarting.load(Ordering::SeqCst)
    }

    /// Begins a restart. Returns false when one runs already.
    pub(crate) fn begin(&self) -> bool {
        !self.restarting.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn end(&self) {
        self.restarting.store(false, Ordering::SeqCst);
    }

    /// The image the Android waits for now (`None`: none). A question or a request to restart
    /// about another image is dropped: it is not news any more.
    pub fn waiting(&self, version: Option<&str>) {
        let mut state = self.state();
        if state.asked.as_ref().map(|asked| asked.version.as_str()) != version {
            state.asked = None;
        }
        if state.restart_now.as_deref() != version {
            state.restart_now = None;
        }
    }

    /// Asks the user whether the Android restarts into `version` now, once for each version.
    /// Returns whether it was asked just now.
    pub fn ask(&self, version: &str) -> bool {
        let mut state = self.state();
        if state
            .asked
            .as_ref()
            .is_some_and(|asked| asked.version == version)
        {
            return false;
        }
        state.asked = Some(UpdateAlert::new(
            Subject::Android,
            AlertKind::Restart,
            version,
        ));
        true
    }

    /// The image the user was asked about, while the question stands.
    pub fn asked(&self) -> Option<String> {
        self.state()
            .asked
            .as_ref()
            .map(|asked| asked.version.clone())
    }

    /// The user asked to restart into `version` now.
    pub fn ask_restart_now(&self, version: &str) {
        self.state().restart_now = Some(version.to_string());
    }

    /// Whether the user asked to restart into `version` now; the request is taken.
    pub fn take_restart_now(&self, version: &str) -> bool {
        let mut state = self.state();
        if state.restart_now.as_deref() == Some(version) {
            state.restart_now = None;
            true
        } else {
            false
        }
    }

    /// A restart applied `version`, an image no trial tells the user about.
    pub fn applied(&self, version: &str) {
        self.state().applied = Some(UpdateAlert::new(
            Subject::Android,
            AlertKind::Applied,
            version,
        ));
    }

    /// The question and the news the user's desktop has not been told yet.
    pub fn untold(&self) -> Vec<UpdateAlert> {
        let state = self.state();
        [&state.applied, &state.asked]
            .into_iter()
            .flatten()
            .filter(|alert| !state.told.contains(&alert.key()))
            .cloned()
            .collect()
    }

    /// The user's desktop was told `alert`.
    pub fn told(&self, alert: &UpdateAlert) {
        let mut state = self.state();
        let key = alert.key();
        let ours = [&state.applied, &state.asked]
            .into_iter()
            .flatten()
            .any(|known| known.key() == key);
        if ours {
            state.told.insert(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr14_the_question_is_asked_once_and_dropped_when_the_image_no_longer_waits() {
        let restart = ImageRestart::default();
        assert!(restart.auto(), "on by default");
        assert!(restart.ask("16.0.2"));
        assert!(!restart.ask("16.0.2"), "once for each version");
        let untold = restart.untold();
        assert_eq!(untold.len(), 1);
        assert_eq!(untold[0].kind, AlertKind::Restart);
        restart.told(&untold[0]);
        assert!(restart.untold().is_empty());
        restart.waiting(None);
        assert_eq!(restart.asked(), None);
        restart.ask_restart_now("16.0.3");
        assert!(!restart.take_restart_now("16.0.2"));
        assert!(restart.take_restart_now("16.0.3"));
        assert!(!restart.take_restart_now("16.0.3"), "taken once");
        restart.ask("16.0.3");
        restart.set_auto(false);
        assert_eq!(
            restart.asked(),
            None,
            "off, the image waits for the next start"
        );
    }
}

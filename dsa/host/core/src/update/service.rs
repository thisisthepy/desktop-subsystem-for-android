//! Updates as the service runs them: the periodic check, and a user's Android that boots a
//! new image on trial.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ed25519_dalek::VerifyingKey;

use super::manifest::{self, ImageEntry};
use super::policy::{decide, Decision, InstalledImage};
use super::store::{Fetch, ImageStore};
use super::trial::{self, Boot, Outcome};
use super::UpdateError;
use crate::host_api::UserAndroid;
use crate::session::{Session, SessionState};

/// The latest news about the image, shared by every user's Android.
#[derive(Default)]
pub struct UpdateNotices(Mutex<Option<String>>);

impl UpdateNotices {
    pub fn set(&self, notice: impl Into<String>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(notice.into());
    }
    pub fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Reads the release list at `list_url` (signature at `list_url` + `.sig`), and downloads
/// the image to move to, if there is one. Returns the news for the user.
pub fn check(
    store: &ImageStore,
    fetch: &dyn Fetch,
    list_url: &str,
    keys: &[VerifyingKey],
) -> Result<Option<String>, UpdateError> {
    check_reporting(store, fetch, list_url, keys, &mut |_| {})
}

/// [`check`], calling `downloading` with the image it is about to download. A download
/// takes minutes, and the user can be told what is happening meanwhile.
pub fn check_reporting(
    store: &ImageStore,
    fetch: &dyn Fetch,
    list_url: &str,
    keys: &[VerifyingKey],
    downloading: &mut dyn FnMut(&ImageEntry),
) -> Result<Option<String>, UpdateError> {
    let mut list = Vec::new();
    fetch
        .fetch(list_url, &mut list)
        .map_err(|e| UpdateError::Download(format!("the release list: {e}")))?;
    let mut signature = Vec::new();
    fetch
        .fetch(&format!("{list_url}.sig"), &mut signature)
        .map_err(|e| UpdateError::BadSignature(format!("fetching the signature: {e}")))?;
    let signature = String::from_utf8_lossy(&signature);
    let manifest = manifest::verify(&list, &signature, keys)?;
    let state = store.state()?;
    if state.on_trial {
        return Ok(None);
    }
    match decide(&state.current, &manifest, &state.failed) {
        Decision::UpToDate => Ok(None),
        Decision::Unsupported(why) => Ok(Some(why)),
        Decision::Apply(image) => {
            downloading(&image);
            store.stage(&image, &manifest, fetch)?;
            Ok(Some(format!(
                "Android {} is downloaded and will be used the next time Android starts",
                image.version
            )))
        }
    }
}

/// Boots a session as [`trial::start`] needs it. The session's launcher reads the current
/// image from the store itself, so the image folder passed in is not used here.
pub struct SessionBoot(pub Arc<Session>);

impl Boot for SessionBoot {
    fn boot(
        &mut self,
        _image: &InstalledImage,
        _image_dir: &std::path::Path,
        timeout: Duration,
    ) -> Result<(), String> {
        let session = &self.0;
        let events = session.events();
        session.start();
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(event) if event.state == SessionState::Active => return Ok(()),
                Ok(event) if event.state == SessionState::Stopped => {
                    return Err(event
                        .reason
                        .map(|reason| reason.to_string())
                        .unwrap_or_else(|| "it stopped while starting".into()))
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "it did not finish booting within {} seconds",
                        timeout.as_secs()
                    ))
                }
            }
        }
    }

    fn halt(&mut self) {
        self.0.stop();
        self.0
            .wait_for(SessionState::Stopped, Duration::from_secs(120));
    }
}

const NO_IMAGE_YET: &str = "no Android image is installed yet; the service downloads one after \
     installation, and `dsa status` says when it is ready. Start Android again then";

impl UpdatingAndroid {
    /// Whether the current image is only a name, with no files to boot from.
    fn no_image_yet(&self) -> bool {
        self.store
            .state()
            .map(|state| !self.store.has_image(&state.current.version))
            .unwrap_or(false)
    }
}

/// A user's Android in a service that updates its image. A start while a new image is on
/// trial boots it on trial, and goes back to the previous image when it fails.
pub struct UpdatingAndroid {
    pub session: Arc<Session>,
    pub store: Arc<ImageStore>,
    pub notices: Arc<UpdateNotices>,
    pub boot_timeout: Duration,
    /// Set while a trial runs, so two users' starts do not run two trials at once.
    pub trial_running: Arc<AtomicBool>,
}

impl UserAndroid for UpdatingAndroid {
    fn state(&self) -> SessionState {
        self.session.state()
    }
    fn last_reason(&self) -> Option<String> {
        if self.no_image_yet() {
            return Some(NO_IMAGE_YET.into());
        }
        UserAndroid::last_reason(&*self.session)
    }
    fn stop(&self) {
        self.session.stop()
    }
    fn update_notice(&self) -> Option<String> {
        self.notices.get()
    }

    fn start(&self) {
        if self.no_image_yet() {
            return;
        }
        let state = self.store.state().ok();
        let on_trial = state.as_ref().is_some_and(|s| s.on_trial);
        // The first image since an installation without a bundled one has nothing before it.
        let first = state.as_ref().is_some_and(|s| {
            s.previous
                .as_ref()
                .is_none_or(|previous| !self.store.has_image(&previous.version))
        });
        if self.session.state() != SessionState::Stopped
            || !on_trial
            || self.trial_running.swap(true, Ordering::SeqCst)
        {
            return UserAndroid::start(&*self.session);
        }
        let events = self.session.events();
        let session = Arc::clone(&self.session);
        let store = Arc::clone(&self.store);
        let notices = Arc::clone(&self.notices);
        let running = Arc::clone(&self.trial_running);
        let timeout = self.boot_timeout;
        thread::spawn(move || {
            let outcome = trial::start(&store, &mut SessionBoot(session), timeout);
            notices.set(match outcome {
                Ok(Outcome::Booted {
                    image,
                    confirmed: true,
                }) if first => format!("Android {} is installed", image.version),
                Ok(Outcome::Booted {
                    image,
                    confirmed: true,
                }) => {
                    format!("Android was updated to {}", image.version)
                }
                Ok(Outcome::Booted { .. }) => return running.store(false, Ordering::SeqCst),
                Ok(Outcome::RolledBack {
                    failed,
                    reason,
                    image,
                }) => format!(
                    "Android {} did not start ({reason}), so Android {} is in use again",
                    failed.version, image.version
                ),
                Ok(Outcome::GaveUp { failed, reason }) => format!(
                    "Android {} did not start ({reason}); no other image is installed, so the \
                     service will download another image and Android can start once it has",
                    failed.version
                ),
                Err(error) => error.to_string(),
            });
            running.store(false, Ordering::SeqCst);
        });
        // Answer once the session has moved on, as a plain start does.
        let _ = events.recv_timeout(Duration::from_secs(5));
    }
}

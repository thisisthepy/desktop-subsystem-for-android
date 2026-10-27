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
use crate::ime::ImeEvent;
use crate::input::PointerEvent;
use crate::keys::KeyEvent;
use crate::memory::GuestMemoryReport;
use crate::session::{Session, SessionState};
use crate::tasks::{TaskRequest, TaskRequestError, TaskWatch};

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

/// How far the download of an image has got.
#[derive(Debug, Clone, Copy)]
pub struct Download<'a> {
    pub image: &'a ImageEntry,
    /// Bytes of the image's files downloaded so far.
    pub done: u64,
    /// Bytes of all its files, as the release list gives them.
    pub total: u64,
}

impl Download<'_> {
    /// The download in words, for `dsa status`.
    pub fn notice(&self) -> String {
        let percent = (self.done as u128 * 100)
            .checked_div(self.total as u128)
            .unwrap_or(100);
        format!(
            "downloading Android {}: {percent}% ({} of {} GB)",
            self.image.version,
            gigabytes(self.done),
            gigabytes(self.total)
        )
    }
}

/// Gigabytes with at most two decimals and no trailing zeros: 1.8, 0.45, 0.
fn gigabytes(bytes: u64) -> String {
    let text = format!("{:.2}", bytes as f64 / 1e9);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// [`check`], calling `downloading` once before the download of a new image starts
/// (`done` is 0) and again as it goes. A download takes minutes, and the user can be told
/// what is happening meanwhile.
pub fn check_reporting(
    store: &ImageStore,
    fetch: &dyn Fetch,
    list_url: &str,
    keys: &[VerifyingKey],
    downloading: &mut dyn FnMut(&Download),
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
            let total = image.files.iter().map(|file| file.size).sum();
            downloading(&Download {
                image: &image,
                done: 0,
                total,
            });
            store.stage_reporting(&image, &manifest, fetch, &mut |done| {
                downloading(&Download {
                    image: &image,
                    done,
                    total,
                })
            })?;
            Ok(Some(format!(
                "Android {} is downloaded and will be used the next time Android starts",
                image.version
            )))
        }
    }
}

/// When the image check runs next. A check scheduled while an image was in place stands,
/// unless none is left now (the first image failed its first boot): then waiting for the
/// regular interval would leave the user with no Android for that long, so it runs now if
/// it was due later than `retry`.
pub fn next_image_check(
    scheduled: Instant,
    now: Instant,
    has_image: bool,
    retry: Duration,
) -> Instant {
    if !has_image && scheduled > now + retry {
        now
    } else {
        scheduled
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
    fn vm_id(&self) -> Option<String> {
        UserAndroid::vm_id(&*self.session)
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        UserAndroid::watch_tasks(&*self.session)
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        UserAndroid::request(&*self.session, request)
    }
    fn freeze(&self) -> Result<(), TaskRequestError> {
        UserAndroid::freeze(&*self.session)
    }
    fn freeze_when_idle(&self, on: bool) {
        UserAndroid::freeze_when_idle(&*self.session, on)
    }
    fn guest_memory(&self) -> Option<GuestMemoryReport> {
        UserAndroid::guest_memory(&*self.session)
    }
    fn request_guest_memory(&self) -> Result<(), TaskRequestError> {
        UserAndroid::request_guest_memory(&*self.session)
    }
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError> {
        UserAndroid::pointer(&*self.session, event)
    }
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError> {
        UserAndroid::key(&*self.session, event)
    }
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError> {
        UserAndroid::ime(&*self.session, event)
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
                    "Android {} did not start ({reason}); no other image is installed. The \
                     service downloads another image if there is one, and the next \
                     version of DSA tries this one again",
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

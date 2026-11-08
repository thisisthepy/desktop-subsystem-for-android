//! Updates as the service runs them: the periodic check, and a user's Android that boots a
//! new image on trial.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ed25519_dalek::VerifyingKey;

use super::alerts::{AlertKind, Subject, UpdateAlert, UpdateAlerts};
use super::manifest::{self, ImageEntry};
use super::policy::{decide_for, Decision, InstalledImage};
use super::store::{Fetch, ImageStore};
use super::trial::{self, Boot, Outcome};
use super::versions::{AndroidVersions, Channel, ChooseError, VersionReport};
use super::UpdateError;
use crate::accessibility::{AccessibilityWatch, ActionRequest};
use crate::apk::{InstallOutcome, InstallPart, InstalledPackage};
use crate::apps::UninstallOutcome;
use crate::background::{BackgroundApp, BackgroundWatch, MediaSession};
use crate::host_api::InstallError;
use crate::host_api::UserAndroid;
use crate::ime::ImeEvent;
use crate::input::PointerEvent;
use crate::keys::KeyEvent;
use crate::memory::GuestMemoryReport;
use crate::print::{PrintEvent, PrintOutcome};
use crate::session::BackgroundRequest;
use crate::session::{BootProblem, Session, SessionState};
use crate::tasks::{AppSettingsPage, TaskRequest, TaskRequestError, TaskWatch};
use crate::widgets::{WidgetError, WidgetProvider, WidgetSize};
use std::sync::mpsc::Receiver;

/// The latest news about the image, shared by every user's Android: in words for `dsa status`
/// and the settings app, and as the alerts each user's desktop shows.
#[derive(Default)]
pub struct UpdateNotices {
    text: Mutex<Option<String>>,
    alerts: UpdateAlerts,
}

impl UpdateNotices {
    pub fn set(&self, notice: impl Into<String>) {
        *self.text.lock().unwrap_or_else(|e| e.into_inner()) = Some(notice.into());
    }
    pub fn get(&self) -> Option<String> {
        self.text.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Raises `alert` for every user's desktop, once for its version.
    pub fn raise(&self, alert: UpdateAlert) -> bool {
        self.alerts.raise(alert)
    }

    /// The alerts raised, and which each user was told.
    pub fn alerts(&self) -> &UpdateAlerts {
        &self.alerts
    }
}

/// A request for the update service to check for images now rather than at its next turn,
/// set when the user chooses another Android version.
#[derive(Default)]
pub struct CheckNow(AtomicBool);

impl CheckNow {
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    /// Whether a check was asked for since the last call.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
}

/// The Android version of a service that keeps its images in `store`.
pub struct StoreVersions {
    pub store: Arc<ImageStore>,
    pub notices: Arc<UpdateNotices>,
    pub check_now: Arc<CheckNow>,
    /// Whether every user of the computer boots from `store`, so that only an administrator
    /// may choose for them all. A store a user keeps for themselves is theirs to choose.
    pub shared_by_users: bool,
}

impl AndroidVersions for StoreVersions {
    fn needs_administrator(&self) -> bool {
        self.shared_by_users
    }

    fn report(&self) -> Result<VersionReport, String> {
        self.store
            .report(self.notices.get())
            .map_err(|e| e.to_string())
    }

    fn choose(&self, channel: Channel, erase_data: bool) -> Result<VersionReport, ChooseError> {
        self.store.choose(channel, erase_data)?;
        if self.store.switch_pending() {
            let report = self.report().map_err(ChooseError::Store)?;
            let notice = format!(
                "Android {} is chosen; the service downloads it now and it is used the next \
                 time Android starts",
                report.target_major
            );
            self.notices.set(notice);
        }
        self.check_now.request();
        self.report().map_err(ChooseError::Store)
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
    let moved_up = store.follow_supported(&manifest.supported_majors)?;
    let state = store.state()?;
    if state.on_trial {
        return Ok(moved_up);
    }
    match decide_for(
        &state.current,
        &manifest,
        &state.failed,
        state.channel,
        state.older_allowed,
    ) {
        Decision::UpToDate => Ok(moved_up),
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

/// [`check_reporting`], and raises on `notices` the alert that an image is downloaded and
/// waits for the next start, when the check put one in place. The service's periodic check
/// runs this, so every user's desktop is told before the update is applied.
pub fn check_alerting(
    store: &ImageStore,
    fetch: &dyn Fetch,
    list_url: &str,
    keys: &[VerifyingKey],
    notices: &UpdateNotices,
    downloading: &mut dyn FnMut(&Download),
) -> Result<Option<String>, UpdateError> {
    let before = store.state().ok().map(|state| state.current.version);
    let news = check_reporting(store, fetch, list_url, keys, downloading)?;
    if let Ok(after) = store.state() {
        if after.on_trial && before.as_ref() != Some(&after.current.version) {
            notices.raise(UpdateAlert::new(
                Subject::Android,
                AlertKind::Ready,
                after.current.version,
            ));
        }
    }
    Ok(news)
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
    /// A disk made under other signing keys stops the agent whichever image runs on it.
    fn image_to_blame(&self) -> bool {
        !matches!(
            self.0.problem(),
            Some(BootProblem::DiskFromOtherKeys { .. })
        )
    }

    fn boot(
        &mut self,
        _image: &InstalledImage,
        _image_dir: &std::path::Path,
        timeout: Duration,
    ) -> Result<(), String> {
        let session = &self.0;
        let events = session.events();
        session.start();
        let mut deadline = Instant::now() + timeout;
        loop {
            // A session already stalled for the image's sake fails the boot now, even when the
            // problem changed after the Stalled event (a missing prerequisite that came back,
            // then an agent that never connects): the event is not sent again for it.
            if session.state() == SessionState::Stalled {
                match session.problem() {
                    Some(BootProblem::Prerequisite(_)) | None => {}
                    Some(problem) => return Err(problem.to_string()),
                }
            }
            // What the guest cannot run without being missing says nothing about the image, so
            // the time on trial starts again once it is there.
            if matches!(session.problem(), Some(BootProblem::Prerequisite(_))) {
                deadline = Instant::now() + timeout;
            }
            let left = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(1));
            match events.recv_timeout(left) {
                Ok(event) if event.state == SessionState::Active => return Ok(()),
                Ok(event) if event.state == SessionState::Stalled => match session.problem() {
                    // Not the image's fault; the same when it moved on again in between.
                    Some(BootProblem::Prerequisite(_)) | None => {}
                    Some(problem) => return Err(problem.to_string()),
                },
                Ok(event) if event.state == SessionState::Stopped => {
                    return Err(event
                        .reason
                        .map(|reason| reason.to_string())
                        .unwrap_or_else(|| "it stopped while starting".into()))
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
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
        // A stop that failed leaves the session in Stopping, where a start is ignored; asking
        // again retries it, so the previous image is not kept from booting by it.
        for _ in 0..2 {
            self.0.stop();
            if self
                .0
                .wait_for(SessionState::Stopped, Duration::from_secs(120))
            {
                return;
            }
        }
    }
}

/// A user's rollback while it runs. The guest belongs to it until the previous image has
/// booted or failed to: a stop asked for meanwhile (`dsa stop`, a restart from the tray) is
/// remembered and made once the rollback is over. Stopping in the middle used to cancel the
/// previous image's boot, which left the user with no Android and a rollback reported as
/// failed although nothing was wrong with the previous image.
#[derive(Default)]
pub struct Rollback(Mutex<RollbackState>);

#[derive(Default)]
struct RollbackState {
    running: bool,
    stop_asked: bool,
}

impl Rollback {
    fn lock(&self) -> std::sync::MutexGuard<'_, RollbackState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a rollback runs now.
    pub fn running(&self) -> bool {
        self.lock().running
    }

    fn begin(&self) {
        *self.lock() = RollbackState {
            running: true,
            stop_asked: false,
        };
    }

    /// Ends the rollback. Returns whether a stop was asked for while it ran.
    fn end(&self) -> bool {
        let mut state = self.lock();
        state.running = false;
        std::mem::take(&mut state.stop_asked)
    }

    /// Remembers a stop while a rollback runs. Returns whether it was put off.
    fn put_off_stop(&self) -> bool {
        let mut state = self.lock();
        if state.running {
            state.stop_asked = true;
        }
        state.running
    }
}

/// [`SessionBoot`] for a trial, with the user's rollback to hold stops off while it runs.
struct TrialBoot {
    session: SessionBoot,
    rollback: Arc<Rollback>,
}

impl Boot for TrialBoot {
    fn boot(
        &mut self,
        image: &InstalledImage,
        image_dir: &std::path::Path,
        timeout: Duration,
    ) -> Result<(), String> {
        self.session.boot(image, image_dir, timeout)
    }
    fn halt(&mut self) {
        self.session.halt()
    }
    fn image_to_blame(&self) -> bool {
        self.session.image_to_blame()
    }
    fn rolling_back(&mut self, on: bool) {
        if on {
            self.rollback.begin();
        } else if self.rollback.end() {
            self.session.0.stop();
        }
    }
}

/// The alert for every user's desktop of how a trial ended: the new image is in use, or it
/// did not start and was undone. A trial that is still undecided, or that is waiting for its
/// confirmation, tells nothing yet.
fn alert_of(outcome: &Outcome) -> Option<UpdateAlert> {
    match outcome {
        Outcome::Booted {
            image,
            confirmed: true,
        } => Some(UpdateAlert::new(
            Subject::Android,
            AlertKind::Applied,
            image.version.clone(),
        )),
        Outcome::Booted { .. } | Outcome::Undecided { .. } => None,
        Outcome::RolledBack {
            failed,
            reason,
            image,
        }
        | Outcome::RolledBackNotReady {
            failed,
            reason,
            image,
            ..
        } => Some(UpdateAlert::failed(
            Subject::Android,
            failed.version.clone(),
            format!(
                "it did not start ({reason}), so Android {} is in use again",
                image.version
            ),
        )),
        Outcome::GaveUp { failed, reason } => Some(UpdateAlert::failed(
            Subject::Android,
            failed.version.clone(),
            format!("it did not start ({reason}), and no other image is installed"),
        )),
    }
}

/// What happens to an image that failed its trial, in words for the user.
fn not_retried(failed: &InstalledImage) -> String {
    format!(
        "Android {} is not tried again until a newer build of it, or a new version of DSA, \
         is available",
        failed.version
    )
}

const ROLLING_BACK: &str = "the new Android image did not start, so the service is going back \
     to the previous one; Android starts with it in a moment";

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
    /// This user's rollback, while one runs.
    pub rollback: Arc<Rollback>,
}

impl UserAndroid for UpdatingAndroid {
    /// While a rollback runs, Android is starting: the failed image's stall and the stop
    /// between the two boots are steps of that start, not where it ended.
    fn state(&self) -> SessionState {
        let state = self.session.state();
        match state {
            SessionState::Stalled | SessionState::Stopping | SessionState::Stopped
                if self.rollback.running() =>
            {
                SessionState::Starting
            }
            _ => state,
        }
    }
    fn last_reason(&self) -> Option<String> {
        if self.no_image_yet() {
            return Some(NO_IMAGE_YET.into());
        }
        UserAndroid::last_reason(&*self.session)
    }
    fn problem(&self) -> Option<String> {
        if self.rollback.running() {
            return Some(ROLLING_BACK.into());
        }
        UserAndroid::problem(&*self.session)
    }
    fn stop(&self) {
        if !self.rollback.put_off_stop() {
            self.session.stop()
        }
    }
    fn update_notice(&self) -> Option<String> {
        self.notices.get()
    }
    fn untold_update_alerts(&self, user: &str) -> Vec<UpdateAlert> {
        self.notices.alerts().untold(user)
    }
    fn update_alert_told(&self, user: &str, alert: &UpdateAlert) {
        self.notices.alerts().told(user, alert)
    }
    fn vm_id(&self) -> Option<String> {
        UserAndroid::vm_id(&*self.session)
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        UserAndroid::watch_tasks(&*self.session)
    }
    fn watch_log(&self, filter: crate::guest_log::LogFilter) -> Option<crate::guest_log::LogWatch> {
        UserAndroid::watch_log(&*self.session, filter)
    }
    fn dump_log(
        &self,
        filter: crate::guest_log::LogFilter,
    ) -> Result<std::sync::mpsc::Receiver<crate::guest_log::LogDumpEvent>, TaskRequestError> {
        UserAndroid::dump_log(&*self.session, filter)
    }
    fn recent_log(&self) -> Vec<crate::guest_log::LogEntry> {
        UserAndroid::recent_log(&*self.session)
    }
    fn collect_diagnostics(
        &self,
    ) -> Result<std::sync::mpsc::Receiver<crate::guest_log::DiagnosticsEvent>, TaskRequestError>
    {
        UserAndroid::collect_diagnostics(&*self.session)
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
    fn links_in_android(&self, on: bool) {
        UserAndroid::links_in_android(&*self.session, on)
    }
    fn install_arm_translation(
        &self,
        size: u64,
        sha256: &[u8],
        content: &mut dyn std::io::Read,
    ) -> Result<crate::arm_translation::GuestArmTranslation, crate::host_api::InstallError> {
        UserAndroid::install_arm_translation(&*self.session, size, sha256, content)
    }
    fn remove_arm_translation(
        &self,
    ) -> Result<crate::arm_translation::GuestArmTranslation, crate::host_api::InstallError> {
        UserAndroid::remove_arm_translation(&*self.session)
    }
    fn arm_translation(&self) -> Option<crate::arm_translation::GuestArmTranslation> {
        UserAndroid::arm_translation(&*self.session)
    }
    fn install_gapps(
        &self,
        size: u64,
        sha256: &[u8],
        content: &mut dyn std::io::Read,
    ) -> Result<crate::gapps::GuestGapps, crate::host_api::InstallError> {
        UserAndroid::install_gapps(&*self.session, size, sha256, content)
    }
    fn remove_gapps(&self) -> Result<crate::gapps::GuestGapps, crate::host_api::InstallError> {
        UserAndroid::remove_gapps(&*self.session)
    }
    fn gapps(&self) -> Option<crate::gapps::GuestGapps> {
        UserAndroid::gapps(&*self.session)
    }
    fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        UserAndroid::developer_mode(&*self.session, on, port)
    }
    fn adb_auto_connect(&self, on: bool) {
        UserAndroid::adb_auto_connect(&*self.session, on)
    }
    fn adb_auto_connect_state(&self) -> crate::adb_discovery::AutoConnectState {
        UserAndroid::adb_auto_connect_state(&*self.session)
    }
    fn watch_links(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        UserAndroid::watch_links(&*self.session)
    }
    fn open_file(&self, file: &crate::files::FileTransfer) -> Result<(), TaskRequestError> {
        UserAndroid::open_file(&*self.session, file)
    }
    fn share_folders(&self, on: bool, folders: Vec<String>) {
        UserAndroid::share_folders(&*self.session, on, folders)
    }
    fn android_files(&self) -> Result<String, String> {
        UserAndroid::android_files(&*self.session)
    }
    fn android_files_with_token(&self, token: &str) -> Result<String, String> {
        UserAndroid::android_files_with_token(&*self.session, token)
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
    fn share_clipboard(&self, on: bool) {
        UserAndroid::share_clipboard(&*self.session, on)
    }
    fn host_clipboard(&self, text: String) {
        UserAndroid::host_clipboard(&*self.session, text)
    }
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        UserAndroid::watch_clipboard(&*self.session)
    }
    fn watch_notifications(
        &self,
    ) -> Option<std::sync::mpsc::Receiver<crate::notifications::NotificationEvent>> {
        UserAndroid::watch_notifications(&*self.session)
    }
    fn watch_location(&self) -> Option<std::sync::mpsc::Receiver<crate::location::Wanted>> {
        UserAndroid::watch_location(&*self.session)
    }
    fn host_location(&self, event: crate::location::LocationEvent) {
        UserAndroid::host_location(&*self.session, event)
    }
    fn set_system_settings(&self, settings: crate::system_settings::SystemSettings) {
        UserAndroid::set_system_settings(&*self.session, settings)
    }
    fn gamepad(&self, event: crate::gamepads::GamepadEvent) {
        UserAndroid::gamepad(&*self.session, event)
    }
    fn watch_gamepad_rumble(
        &self,
    ) -> Option<std::sync::mpsc::Receiver<crate::gamepads::GamepadRumble>> {
        UserAndroid::watch_gamepad_rumble(&*self.session)
    }
    fn notification_action(
        &self,
        action: crate::notifications::NotificationAction,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::notification_action(&*self.session, action)
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
        let rollback = Arc::clone(&self.rollback);
        thread::spawn(move || {
            let mut guest = TrialBoot {
                session: SessionBoot(session),
                rollback,
            };
            let outcome = trial::start(&store, &mut guest, timeout);
            if let Some(alert) = outcome.as_ref().ok().and_then(alert_of) {
                notices.raise(alert);
            }
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
                    "Android {} did not start ({reason}), so Android {} is in use again. {}",
                    failed.version,
                    image.version,
                    not_retried(&failed)
                ),
                Ok(Outcome::RolledBackNotReady {
                    failed,
                    reason,
                    image,
                    why,
                }) => format!(
                    "Android {} did not start ({reason}), so Android {} is in use again, but \
                     it has not finished starting yet ({why}). Start Android again (dsa \
                     start) to boot Android {}. {}",
                    failed.version,
                    image.version,
                    image.version,
                    not_retried(&failed)
                ),
                Ok(Outcome::GaveUp { failed, reason }) => format!(
                    "Android {} did not start ({reason}); no other image is installed. The \
                     service downloads another image if there is one, and the next \
                     version of DSA tries this one again",
                    failed.version
                ),
                Ok(Outcome::Undecided { image, reason }) => format!(
                    "Android {} is kept on trial and is not to blame: {reason}",
                    image.version
                ),
                Err(error) => error.to_string(),
            });
            running.store(false, Ordering::SeqCst);
        });
        // Answer once the session has moved on, as a plain start does.
        let _ = events.recv_timeout(Duration::from_secs(5));
    }
    fn watch_print_jobs(&self) -> Option<Receiver<PrintEvent>> {
        UserAndroid::watch_print_jobs(&*self.session)
    }
    fn print_result(&self, job_id: u32, outcome: &PrintOutcome) -> Result<(), TaskRequestError> {
        UserAndroid::print_result(&*self.session, job_id, outcome)
    }
    fn watch_background(&self) -> Option<BackgroundWatch> {
        UserAndroid::watch_background(&*self.session)
    }
    fn background_apps(&self) -> (Vec<BackgroundApp>, Vec<MediaSession>) {
        UserAndroid::background_apps(&*self.session)
    }
    fn background_action(&self, request: BackgroundRequest) -> Result<(), TaskRequestError> {
        UserAndroid::background_action(&*self.session, request)
    }
    fn watch_widgets(&self) -> Option<crate::session::WidgetWatch> {
        UserAndroid::watch_widgets(&*self.session)
    }
    fn widgets(&self) -> Vec<(i32, WidgetProvider)> {
        UserAndroid::widgets(&*self.session)
    }
    fn widget_providers(&self) -> Result<Vec<WidgetProvider>, WidgetError> {
        UserAndroid::widget_providers(&*self.session)
    }
    fn add_widget(&self, provider: &str) -> Result<i32, WidgetError> {
        UserAndroid::add_widget(&*self.session, provider)
    }
    fn remove_widget(&self, widget_id: i32) -> Result<(), TaskRequestError> {
        UserAndroid::remove_widget(&*self.session, widget_id)
    }
    fn resize_widget(&self, widget_id: i32, size: WidgetSize) -> Result<(), TaskRequestError> {
        UserAndroid::resize_widget(&*self.session, widget_id, size)
    }
    fn widget_pointer(
        &self,
        pointer: dsa_protocol::control::WidgetPointer,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::widget_pointer(&*self.session, pointer)
    }
    fn install_apk(
        &self,
        name: &str,
        size: u64,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        UserAndroid::install_apk(&*self.session, name, size, content)
    }
    fn install_apk_for(
        &self,
        name: &str,
        size: u64,
        abi: Option<&str>,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        UserAndroid::install_apk_for(&*self.session, name, size, abi, content)
    }
    fn install_parts(
        &self,
        name: &str,
        parts: &[InstallPart],
        abi: Option<&str>,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        UserAndroid::install_parts(&*self.session, name, parts, abi, content)
    }
    fn set_run_as_arm(
        &self,
        package: &str,
        on: bool,
        erase_data: bool,
    ) -> Result<crate::native_abi::RunAsArmOutcome, TaskRequestError> {
        UserAndroid::set_run_as_arm(&*self.session, package, on, erase_data)
    }
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        UserAndroid::package_state(&*self.session, package)
    }
    fn refresh_apps(&self) -> Result<(), TaskRequestError> {
        UserAndroid::refresh_apps(&*self.session)
    }
    fn drag_out(
        &self,
        out: crate::drag::DragOut,
    ) -> Result<Option<crate::drag::DragOffer>, TaskRequestError> {
        UserAndroid::drag_out(&*self.session, out)
    }
    fn read_drag(
        &self,
        read: crate::drag::DragRead,
    ) -> Result<crate::drag::DragChunk, TaskRequestError> {
        UserAndroid::read_drag(&*self.session, read)
    }
    fn end_drag(&self, drag_id: u32, dropped: bool) -> Result<(), TaskRequestError> {
        UserAndroid::end_drag(&*self.session, drag_id, dropped)
    }
    fn open_app_settings(&self, package: Option<&str>) -> Result<(), TaskRequestError> {
        UserAndroid::open_app_settings(&*self.session, package)
    }
    fn open_app_settings_page(
        &self,
        package: Option<&str>,
        page: AppSettingsPage,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::open_app_settings_page(&*self.session, package, page)
    }
    fn uninstall_app(&self, package: &str) -> Result<UninstallOutcome, TaskRequestError> {
        UserAndroid::uninstall_app(&*self.session, package)
    }
    fn watch_accessibility(&self, task_id: i32) -> Option<AccessibilityWatch> {
        UserAndroid::watch_accessibility(&*self.session, task_id)
    }
    fn accessibility_action(
        &self,
        task_id: i32,
        action: &ActionRequest,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::accessibility_action(&*self.session, task_id, action)
    }
}

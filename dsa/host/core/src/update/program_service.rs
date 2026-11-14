//! Updating DSA itself as the service runs it: the check of the program list, the
//! download of the new setup file, and running it once no user's Android is running.
//!
//! ```text
//! <dir>/state.json            the downloaded update, and the version that failed here
//! <dir>/setup-process.json    the setup that runs, for a service that starts meanwhile
//! <dir>/<version>/<file>      the setup file, as the list's URL names it
//! <dir>/.download-<version>   a download in progress
//! ```
//!
//! Setup stops the service, replaces it and starts the new one. Before it runs, the state
//! records that it is installing; the service that starts next compares its own version
//! with that record and tells the user whether the update worked. A service that starts
//! while setup still runs (something started it again after setup stopped it) waits for
//! setup instead of calling the update failed, and a service still running once setup has
//! installed past it (exit code "installed" or "installed, restart to finish") restarts into
//! the new program files; so does one that finds the files newer than itself. A failed
//! version is not installed again on its own, since it would most likely fail the same way;
//! `dsa update` tries it again, and a newer version replaces it.
//!
//! The service follows one release channel ([`ReleaseChannel`]): on the stable channel it
//! reads the stable program list alone, and on the pre-release channel it reads the
//! pre-release list too and takes the higher version. A computer that moves from the
//! pre-release channel to the stable one keeps the version it has: nothing lower is ever
//! installed, so it waits until the stable list offers a newer version, and an update that
//! only the pre-release list offered and that is still waiting to be installed is dropped.
//!
//! Running setup is behind [`ProgramInstaller`]; the platform crates supply it.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::alerts::{AlertKind, Subject, UpdateAlert};
use super::manifest::is_plain_name;
use super::program::{
    self, ProgramDecision, ProgramError, ProgramManifest, ReleaseChannel, Version,
};
use super::service::UpdateNotices;
use super::store::{download_checked, DownloadFailure, Expected, Fetch};
use super::{from_hex, to_hex};
use crate::accessibility::{AccessibilityWatch, ActionRequest};
use crate::apk::{InstallOutcome, InstallPart, InstalledPackage};
use crate::apps::UninstallOutcome;
use crate::background::{BackgroundApp, BackgroundWatch, MediaSession};
use crate::host_api::InstallError;
use crate::host_api::{
    ProgramPhase, ProgramProgress, ProgramUpdate, UpdateProgram, UserAndroid, UserAndroids,
};
use crate::ime::ImeEvent;
use crate::input::PointerEvent;
use crate::keys::KeyEvent;
use crate::memory::GuestMemoryReport;
use crate::print::{PrintEvent, PrintOutcome};
use crate::session::BackgroundRequest;
use crate::session::SessionState;
use crate::tasks::{AppSettingsPage, TaskRequest, TaskRequestError, TaskWatch};
use crate::widgets::{WidgetError, WidgetProvider, WidgetSize};
use std::sync::mpsc::Receiver;

/// Runs a setup file, and knows what the platform's setup and service do around it.
pub trait ProgramInstaller: Send + Sync {
    /// Starts `setup` quietly, writing its log to `log`, as a process apart from the
    /// service: setup stops the service and replaces it, and must outlive it. Returns
    /// once setup has started.
    fn install(&self, setup: &Path, log: &Path) -> Result<Box<dyn RunningSetup>, String>;

    /// What setup's exit code `code` means. Only 0 counts as installed unless the platform
    /// says more: some setups end with a code of their own for "installed, and the computer
    /// should restart to finish".
    fn outcome(&self, code: u32) -> SetupOutcome {
        if code == 0 {
            SetupOutcome::Installed
        } else {
            SetupOutcome::Failed
        }
    }

    /// The setup that [`RunningSetup::id`] named, if it is still running. A service that
    /// starts while setup is still replacing it finds setup this way and waits for it,
    /// instead of reporting the update as failed while it is still going on.
    fn reattach(&self, _id: &str) -> Option<Box<dyn RunningSetup>> {
        None
    }

    /// The version of the program files installed now, read from the files rather than
    /// from this process. Setup can replace the files of a service that is still running,
    /// so the two can differ. `None` when it cannot be read.
    fn installed_version(&self) -> Option<String> {
        None
    }

    /// Whether some installation is running on this computer now, so the program files may
    /// be half replaced.
    fn installation_running(&self) -> bool {
        false
    }

    /// Stops this service and starts it again from the program files installed now. Returns
    /// once the restart is arranged; the service then stops on its own.
    fn restart_into_installed(&self) -> Result<(), String> {
        Err("this platform cannot restart the service by itself".into())
    }
}

/// A setup that has started.
pub trait RunningSetup: Send {
    /// Waits for setup to end and returns its exit code.
    fn wait(self: Box<Self>) -> Result<u32, String>;

    /// Names this setup for [`ProgramInstaller::reattach`], so a service that starts while
    /// it runs can find it again. `None` when the platform cannot.
    fn id(&self) -> Option<String> {
        None
    }
}

/// What setup's exit code says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupOutcome {
    /// The new version is installed.
    Installed,
    /// The new version is installed, and the computer should restart to finish (to remove
    /// files that were in use). The update worked; the restart only tidies up.
    InstalledRestartPending,
    /// Setup failed and the version before it stays.
    Failed,
}

/// What a service that is still running once setup has ended does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterSetup {
    /// Setup installed the new version, but this process is the one before it: it restarts
    /// into the files setup put in place.
    RestartIntoInstalled,
    /// The update failed; the text says why.
    Failed(String),
}

/// Decides what a service still running after setup ended does: setup ended with `code`,
/// which the platform read as `outcome`, while installing `wanted`; `on_disk` is the version
/// of the program files now, when it can be read. Setup that says it installed counts as
/// installed, a restart pending included, unless the files show an older version; and then
/// the service restarts, since the process still running is the old one.
pub fn after_setup(
    outcome: SetupOutcome,
    code: u32,
    wanted: &str,
    on_disk: Option<&str>,
) -> AfterSetup {
    if outcome == SetupOutcome::Failed {
        return AfterSetup::Failed(format!(
            "setup ended with exit code {code} and the service was not replaced"
        ));
    }
    let disk = on_disk.and_then(|disk| Version::parse(disk).ok().map(|v| (disk, v)));
    match (disk, Version::parse(wanted)) {
        (Some((text, disk)), Ok(wanted)) if disk < wanted => AfterSetup::Failed(format!(
            "setup ended with exit code {code}, yet the program files are still DSA {text}"
        )),
        _ => AfterSetup::RestartIntoInstalled,
    }
}

/// Whether a service of version `running` restarts into program files of version `on_disk`:
/// only when the files are newer, which means setup replaced them under it. An installation
/// still running (`installing`) may not be done replacing them, so the service waits for it.
pub fn restart_into_files(running: &str, on_disk: Option<&str>, installing: bool) -> bool {
    if installing {
        return false;
    }
    match (
        on_disk.and_then(|disk| Version::parse(disk).ok()),
        Version::parse(running),
    ) {
        (Some(disk), Ok(running)) => disk > running,
        _ => false,
    }
}

/// The setup that is running, as the service that started it records it, so a service that
/// starts meanwhile can find it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SetupRecord {
    version: String,
    id: String,
}

/// How the service updates the program.
#[derive(Debug, Clone)]
pub struct ProgramSettings {
    /// The signed program list of the stable channel. Its signature is at the same URL plus
    /// `.sig`. Without it or [`prerelease_list_url`](Self::prerelease_list_url) nothing is
    /// checked.
    pub list_url: Option<String>,
    /// The signed program list of the pre-release channel, read beside the stable one on that
    /// channel. Its signature is at the same URL plus `.sig`.
    pub prerelease_list_url: Option<String>,
    /// The channel followed unless [`ProgramUpdates::follow_channel`] says otherwise: the
    /// channel of the installer that put this version here.
    pub channel: ReleaseChannel,
    /// False turns program updates off: the list is never fetched.
    pub enabled: bool,
    /// Public keys, any of which may sign the list.
    pub keys: Vec<VerifyingKey>,
    /// The service's own folder for downloads and the update state.
    pub dir: PathBuf,
    /// Where setup writes its log.
    pub log_dir: PathBuf,
    /// The version of DSA that is running.
    pub running: String,
    /// How long `dsa update` waits for every Android to stop before it gives up.
    pub stop_timeout: Duration,
    /// How long the service may still be running after setup has ended before the update
    /// counts as failed. A successful setup stops the service before it ends.
    pub settle: Duration,
}

/// What a check of the program list found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Nothing to install; the text says what the list offered and why.
    NotNewer(String),
    /// The offered version already failed to install here; the text says so.
    FailedBefore(String),
    /// This version was downloaded now and waits to be installed.
    Downloaded(String),
    /// This version was downloaded before and still waits to be installed.
    AlreadyDownloaded(String),
}

/// What the program update tells the users, and whether setup is running. Every user's
/// Android reads it.
#[derive(Default)]
pub struct ProgramStatus {
    pub notices: UpdateNotices,
    installing: Mutex<Option<String>>,
    /// How far the update under way, or the last one, has got.
    progress: Mutex<Option<ProgramProgress>>,
}

impl ProgramStatus {
    /// Where an update of DSA stands: under way, just installed, or failed. `None` when no
    /// update has run since the service started.
    pub fn progress(&self) -> Option<ProgramProgress> {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn set_progress(&self, phase: ProgramPhase, version: &str) {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(ProgramProgress::new(phase, version));
    }
    fn clear_progress(&self) {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    /// Clears the progress only while it still says `phase`, so a later phase another thread
    /// has set meanwhile stays.
    fn clear_progress_if(&self, phase: ProgramPhase) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if progress.as_ref().is_some_and(|p| p.phase == phase) {
            *progress = None;
        }
    }
    /// The version setup is installing, while it runs.
    pub fn installing(&self) -> Option<String> {
        self.installing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn set_installing(&self, version: Option<String>) {
        *self.installing.lock().unwrap_or_else(|e| e.into_inner()) = version;
    }
}

/// The downloaded update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    version: String,
    /// The setup file's name in `<dir>/<version>/`.
    file: String,
    size: u64,
    sha256: String,
    /// The version that was running when it was downloaded.
    from: String,
    /// The channel whose list offered it. A download only the pre-release list offered is
    /// not installed once the computer follows the stable channel.
    #[serde(default = "stable")]
    list: ReleaseChannel,
    /// Set just before setup runs.
    #[serde(default)]
    installing: bool,
    /// Setup's log, once it has run.
    #[serde(default)]
    log: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramState {
    #[serde(default)]
    pending: Option<Pending>,
    /// The version whose installation failed here.
    #[serde(default)]
    failed: Option<String>,
}

fn stable() -> ReleaseChannel {
    ReleaseChannel::Stable
}

/// Whether a download from the list of `list` may be installed on `channel`.
fn allowed_on(channel: ReleaseChannel, list: ReleaseChannel) -> bool {
    channel == ReleaseChannel::Prerelease || list == ReleaseChannel::Stable
}

fn store_error(what: &str, path: &Path, error: impl std::fmt::Display) -> ProgramError {
    ProgramError::Store(format!("{what} {}: {error}", path.display()))
}

/// The service's program updates.
pub struct ProgramUpdates {
    settings: ProgramSettings,
    fetch: Box<dyn Fetch + Send + Sync>,
    installer: Box<dyn ProgramInstaller>,
    androids: Arc<dyn UserAndroids>,
    status: Arc<ProgramStatus>,
    /// One check or installation at a time.
    busy: Mutex<()>,
    /// Prefixes the log lines, such as "dsa-hostd 0.1.0".
    name: String,
    /// This, for the thread that watches setup.
    me: Weak<ProgramUpdates>,
    /// The user's setting that turns updates off while the service runs, if it follows one.
    switch: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    /// The user's setting that chooses the release channel, if it follows one.
    channel: OnceLock<Box<dyn Fn() -> ReleaseChannel + Send + Sync>>,
    /// Set once this service has asked to be restarted into newer program files.
    restarting: AtomicBool,
}

/// When the service checks and installs.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The first check after the service starts.
    pub first_check: Duration,
    pub check_interval: Duration,
    /// How often a downloaded update looks again for a moment when no Android is running.
    pub retry: Duration,
}

impl ProgramUpdates {
    /// Sets up program updates and tells the users how the last installation went, if
    /// the service has just been restarted by one.
    pub fn new(
        settings: ProgramSettings,
        fetch: Box<dyn Fetch + Send + Sync>,
        installer: Box<dyn ProgramInstaller>,
        androids: Arc<dyn UserAndroids>,
        status: Arc<ProgramStatus>,
        name: &str,
    ) -> Arc<ProgramUpdates> {
        let updates = Arc::new_cyclic(|me| ProgramUpdates {
            settings,
            fetch,
            installer,
            androids,
            status,
            busy: Mutex::new(()),
            name: name.to_string(),
            me: me.clone(),
            switch: OnceLock::new(),
            channel: OnceLock::new(),
            restarting: AtomicBool::new(false),
        });
        if let Err(error) = updates.report_last_install() {
            updates.log(&error.to_string());
        }
        updates
    }

    fn log(&self, line: &str) {
        eprintln!("{}: {line}", self.name);
    }

    /// Follows a setting the user changes while the service runs: when `switch` answers
    /// false, nothing is checked or installed, as when the configuration turns updates off.
    pub fn follow_switch(&self, switch: Box<dyn Fn() -> bool + Send + Sync>) {
        let _ = self.switch.set(switch);
    }

    fn switched_off(&self) -> bool {
        self.switch.get().is_some_and(|switch| !switch())
    }

    /// Follows the setting that chooses the release channel while the service runs: each
    /// check reads the lists of the channel `channel` answers.
    pub fn follow_channel(&self, channel: Box<dyn Fn() -> ReleaseChannel + Send + Sync>) {
        let _ = self.channel.set(channel);
    }

    /// The release channel followed now.
    pub fn channel(&self) -> ReleaseChannel {
        self.channel
            .get()
            .map_or(self.settings.channel, |channel| channel())
    }

    fn tell(&self, notice: String) {
        self.log(&notice);
        self.status.notices.set(notice);
    }

    fn state_path(&self) -> PathBuf {
        self.settings.dir.join("state.json")
    }

    fn state(&self) -> Result<ProgramState, ProgramError> {
        let path = self.state_path();
        match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| store_error("reading", &path, e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ProgramState::default()),
            Err(e) => Err(store_error("reading", &path, e)),
        }
    }

    fn write_state(&self, state: &ProgramState) -> Result<(), ProgramError> {
        let dir = &self.settings.dir;
        fs::create_dir_all(dir).map_err(|e| store_error("creating", dir, e))?;
        let path = self.state_path();
        let temporary = dir.join("state.json.new");
        let text = serde_json::to_vec_pretty(state).expect("the state serialises");
        let mut file =
            fs::File::create(&temporary).map_err(|e| store_error("writing", &temporary, e))?;
        file.write_all(&text)
            .and_then(|()| file.sync_all())
            .map_err(|e| store_error("writing", &temporary, e))?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| store_error("replacing", &path, e))
    }

    fn setup_record_path(&self) -> PathBuf {
        self.settings.dir.join("setup-process.json")
    }

    /// Records the setup that runs, so a service that starts meanwhile can find it. A
    /// record that cannot be written only costs that.
    fn record_setup(&self, version: &str, running: &dyn RunningSetup) {
        let Some(id) = running.id() else {
            return;
        };
        let record = SetupRecord {
            version: version.to_string(),
            id,
        };
        let path = self.setup_record_path();
        let text = serde_json::to_vec_pretty(&record).expect("the record serialises");
        if let Err(error) = fs::write(&path, text) {
            self.log(&format!("recording setup in {}: {error}", path.display()));
        }
    }

    fn forget_setup(&self) {
        let _ = fs::remove_file(self.setup_record_path());
    }

    /// The setup installing `version`, if it is still running.
    fn running_setup(&self, version: &str) -> Option<Box<dyn RunningSetup>> {
        let bytes = fs::read(self.setup_record_path()).ok()?;
        let record: SetupRecord = serde_json::from_slice(&bytes).ok()?;
        if record.version != version {
            return None;
        }
        self.installer.reattach(&record.id)
    }

    /// Waits for `running` on a thread of its own, as [`watch_setup`](Self::watch_setup).
    fn watch_in_background(
        &self,
        running: Box<dyn RunningSetup>,
        version: &str,
        log: &Path,
    ) -> Result<(), ProgramError> {
        let updates = self
            .me
            .upgrade()
            .expect("the updates are alive while they run");
        let (version, log) = (version.to_string(), log.to_path_buf());
        let watch = move || updates.watch_setup(running, &version, &log);
        thread::Builder::new()
            .name("dsa-setup".into())
            .spawn(watch)
            .map(|_| ())
            .map_err(|e| ProgramError::Install(format!("watching setup: {e}")))
    }

    fn setup_path(&self, pending: &Pending) -> PathBuf {
        self.settings.dir.join(&pending.version).join(&pending.file)
    }

    /// Removes every downloaded setup file except `keep`'s.
    fn remove_downloads(&self, keep: Option<&str>) {
        let Ok(entries) = fs::read_dir(&self.settings.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_download = Version::parse(&name).is_ok() || name.starts_with(".download-");
            if is_download && Some(name.as_str()) != keep {
                let _ = fs::remove_dir_all(entry.path());
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn failure_notice(&self, version: &str, detail: &str) -> String {
        format!(
            "updating DSA to {version} failed ({detail}); DSA {} is still installed",
            self.settings.running
        )
    }

    /// Tells the users that installing `version` failed, because of `detail`: in words, and
    /// on every user's desktop.
    fn tell_failure(&self, version: &str, detail: &str) {
        self.status.set_progress(ProgramPhase::Failed, version);
        self.tell(self.failure_notice(version, detail));
        self.status
            .notices
            .raise(UpdateAlert::failed(Subject::Program, version, detail));
    }

    /// Tells the users how the installation the state records went: the version running
    /// now is the one setup installed, or setup failed and the old version is still here.
    fn report_last_install(&self) -> Result<(), ProgramError> {
        let mut state = self.state()?;
        let Some(pending) = state.pending.clone() else {
            return Ok(());
        };
        let running = Version::parse(&self.settings.running).ok();
        let wanted = Version::parse(&pending.version).ok();
        let arrived = matches!((&running, &wanted), (Some(r), Some(w)) if r >= w);
        if pending.installing {
            if arrived {
                self.forget_setup();
                self.tell_applied();
                state = ProgramState::default();
                self.remove_downloads(None);
            } else if let Some(setup) = self.running_setup(&pending.version) {
                // This service was started while setup is still replacing it: setup is not
                // done, so nothing has failed yet. Android waits, as it does while setup
                // runs, and the service decides once setup has ended.
                self.log(&format!(
                    "setup is still installing DSA {}; this service waits for it to end",
                    pending.version
                ));
                self.status.set_installing(Some(pending.version.clone()));
                self.status
                    .set_progress(ProgramPhase::Installing, &pending.version);
                let log = pending.log.clone().unwrap_or_default();
                if let Err(error) = self.watch_in_background(setup, &pending.version, &log) {
                    self.status.set_installing(None);
                    self.status.clear_progress();
                    self.log(&error.to_string());
                }
                return Ok(());
            } else {
                self.forget_setup();
                let log = pending
                    .log
                    .as_ref()
                    .map(|log| format!("see {}", log.display()))
                    .unwrap_or_else(|| "setup left no log".into());
                self.tell_failure(&pending.version, &log);
                state.failed = Some(pending.version.clone());
                state.pending = Some(Pending {
                    installing: false,
                    ..pending
                });
            }
            return self.write_state(&state);
        }
        if arrived {
            // Installed some other way meanwhile, or by a setup this service took for failed:
            // a service that setup ran under was started again before setup was done, and
            // the version it reported as failed is the one running now.
            if state.failed.as_deref() == Some(pending.version.as_str()) {
                state.failed = None;
                self.tell_applied();
            }
            state.pending = None;
            self.remove_downloads(None);
            return self.write_state(&state);
        }
        Ok(())
    }

    /// Tells the users that the version running now is the one an update installed.
    fn tell_applied(&self) {
        self.status
            .set_progress(ProgramPhase::Applied, &self.settings.running);
        self.tell(format!("DSA was updated to {}", self.settings.running));
        self.status.notices.raise(UpdateAlert::new(
            Subject::Program,
            AlertKind::Applied,
            self.settings.running.clone(),
        ));
    }

    /// Restarts into the program files when they are newer than this service: setup
    /// replaced them while this process kept running, so every client from the new files is
    /// refused by an old service until it restarts. Not while this service's own setup or
    /// any other installation still runs. Returns whether a restart was asked for.
    pub fn restart_if_replaced(&self) -> bool {
        if self.restarting.load(Ordering::Relaxed) || self.status.installing().is_some() {
            return false;
        }
        let on_disk = self.installer.installed_version();
        let installing = self.installer.installation_running();
        if !restart_into_files(&self.settings.running, on_disk.as_deref(), installing) {
            return false;
        }
        let new_version = on_disk.clone().unwrap_or_default();
        self.log(&format!(
            "the program files are DSA {}, newer than this service (DSA {}): setup replaced \
             them while it ran; restarting into them",
            on_disk.unwrap_or_default(),
            self.settings.running
        ));
        self.restarting.store(true, Ordering::Relaxed);
        self.status
            .set_progress(ProgramPhase::Restarting, &new_version);
        if let Err(why) = self.installer.restart_into_installed() {
            self.status.clear_progress_if(ProgramPhase::Restarting);
            self.log(&format!(
                "restarting the service failed: {why}; restart the DSA service or the computer"
            ));
            return false;
        }
        true
    }

    /// Reads the program list and downloads the version it offers when that is newer
    /// than the running one. Never fetches anything while updates are turned off.
    pub fn check(&self) -> Result<CheckOutcome, ProgramError> {
        let _busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        self.check_locked(false)
    }

    /// The lists `channel` reads, with the channel each is published on: on the pre-release
    /// channel the pre-release list, then the stable list on either channel.
    fn lists(&self, channel: ReleaseChannel) -> Result<Vec<(ReleaseChannel, &str)>, ProgramError> {
        if !self.settings.enabled {
            return Err(ProgramError::Off(
                "program updates are turned off in the service's configuration".into(),
            ));
        }
        if self.switched_off() {
            return Err(ProgramError::Off(
                "program updates are turned off in the settings".into(),
            ));
        }
        let mut lists = Vec::new();
        if channel == ReleaseChannel::Prerelease {
            if let Some(url) = self.settings.prerelease_list_url.as_deref() {
                lists.push((ReleaseChannel::Prerelease, url));
            }
        }
        if let Some(url) = self.settings.list_url.as_deref() {
            lists.push((ReleaseChannel::Stable, url));
        }
        if lists.is_empty() {
            return Err(ProgramError::Off(format!(
                "the configuration names no program list for the {channel} channel"
            )));
        }
        Ok(lists)
    }

    /// Fetches the list at `url` and its signature, and checks both.
    fn read_list(&self, url: &str) -> Result<ProgramManifest, ProgramError> {
        let mut bytes = Vec::new();
        self.fetch
            .fetch(url, &mut bytes)
            .map_err(|e| ProgramError::Download(format!("the program list: {e}")))?;
        let mut signature = Vec::new();
        self.fetch
            .fetch(&format!("{url}.sig"), &mut signature)
            .map_err(|e| ProgramError::BadSignature(format!("fetching the signature: {e}")))?;
        program::verify(
            &bytes,
            &String::from_utf8_lossy(&signature),
            &self.settings.keys,
        )
    }

    /// Reads the lists of `channel` and picks the one to follow, with the channel it was
    /// published on. On the pre-release channel a list that cannot be read is logged and the
    /// other one is followed; only when no list can be read is the check an error.
    fn chosen_list(
        &self,
        channel: ReleaseChannel,
    ) -> Result<(ReleaseChannel, ProgramManifest), ProgramError> {
        let mut stable = None;
        let mut prerelease = None;
        let mut first_error = None;
        for (list, url) in self.lists(channel)? {
            match self.read_list(url) {
                Ok(read) if list == ReleaseChannel::Stable => stable = Some(read),
                Ok(read) => prerelease = Some(read),
                Err(error) => {
                    self.log(&format!("reading the {list} program list {url}: {error}"));
                    first_error.get_or_insert(error);
                }
            }
        }
        match program::choose(channel, stable.as_ref(), prerelease.as_ref()) {
            Some((list, chosen)) => Ok((list, chosen.clone())),
            None => Err(first_error.unwrap_or_else(|| {
                ProgramError::Off(format!(
                    "there is no program list for the {channel} channel"
                ))
            })),
        }
    }

    /// Drops a downloaded update that `channel` does not take: one only the pre-release list
    /// offered, after a switch to the stable channel.
    fn drop_pending_not_on(&self, channel: ReleaseChannel) -> Result<(), ProgramError> {
        let mut state = self.state()?;
        let Some(pending) = &state.pending else {
            return Ok(());
        };
        if pending.installing || allowed_on(channel, pending.list) {
            return Ok(());
        }
        self.log(&format!(
            "DSA {} came from the {} program list, so it is not installed on the {channel} \
             channel",
            pending.version, pending.list
        ));
        state.pending = None;
        self.write_state(&state)?;
        self.remove_downloads(None);
        Ok(())
    }

    /// [`check`](Self::check) with `busy` held. `retry_failed` downloads a version that
    /// failed here before, as `dsa update` asks.
    fn check_locked(&self, retry_failed: bool) -> Result<CheckOutcome, ProgramError> {
        let channel = self.channel();
        let (from, list) = self.chosen_list(channel)?;
        self.drop_pending_not_on(channel)?;
        let offered = match program::decide(&self.settings.running, &list)? {
            ProgramDecision::NotNewer(why) => {
                return Ok(CheckOutcome::NotNewer(format!(
                    "{why} (this computer follows the {channel} channel)"
                )))
            }
            ProgramDecision::Install(offered) => offered,
        };
        let mut state = self.state()?;
        if !retry_failed && state.failed.as_deref() == Some(list.version.as_str()) {
            return Ok(CheckOutcome::FailedBefore(format!(
                "DSA {} failed to install here before, so it is not installed again on its \
                 own; run `dsa update` to try again",
                list.version
            )));
        }
        if let Some(pending) = &mut state.pending {
            if pending.version == list.version
                && pending.sha256.eq_ignore_ascii_case(&list.setup.sha256)
                && self.matches(pending).is_ok()
            {
                let version = pending.version.clone();
                if pending.list != from {
                    // The same file, now offered by the other list: it follows that one.
                    pending.list = from;
                    self.write_state(&state)?;
                }
                return Ok(CheckOutcome::AlreadyDownloaded(version));
            }
        }
        self.status
            .set_progress(ProgramPhase::Downloading, &list.version);
        let downloaded = self.download(&list, &offered, from);
        self.status.clear_progress_if(ProgramPhase::Downloading);
        let pending = downloaded?;
        state.pending = Some(pending);
        self.write_state(&state)?;
        self.remove_downloads(Some(&list.version));
        self.tell(format!(
            "DSA {} is downloaded and installs when Android is not running (`dsa update` \
             installs it now)",
            list.version
        ));
        // `dsa update` installs what it downloads at once, so only the service's own check
        // tells the desktop that an update waits.
        if !retry_failed {
            self.status.notices.raise(UpdateAlert::new(
                Subject::Program,
                AlertKind::Ready,
                list.version.clone(),
            ));
        }
        Ok(CheckOutcome::Downloaded(list.version))
    }

    fn download(
        &self,
        list: &ProgramManifest,
        offered: &Version,
        from: ReleaseChannel,
    ) -> Result<Pending, ProgramError> {
        let dir = &self.settings.dir;
        // The file keeps the name the URL gives it, so setup sees the name it was
        // published under; the folder is the version, which is only digits and dots.
        let file = list
            .setup
            .url
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| is_plain_name(name))
            .unwrap_or("setup")
            .to_string();
        let staging = dir.join(format!(".download-{offered}"));
        let _ = fs::remove_file(&staging);
        fs::create_dir_all(dir).map_err(|e| store_error("creating", dir, e))?;
        let expected = Expected {
            name: &file,
            url: &list.setup.url,
            size: list.setup.size,
            sha256: &list.setup.sha256,
            list: "program list",
        };
        if let Err(failure) = download_checked(&expected, &staging, &*self.fetch, &mut |_| {}) {
            let _ = fs::remove_file(&staging);
            return Err(match failure {
                DownloadFailure::Download(why) => ProgramError::Download(why),
                DownloadFailure::Store(why) => ProgramError::Store(why),
            });
        }
        let pending = Pending {
            version: list.version.clone(),
            file,
            size: list.setup.size,
            sha256: list.setup.sha256.to_ascii_lowercase(),
            from: self.settings.running.clone(),
            list: from,
            installing: false,
            log: None,
        };
        let target = self.setup_path(&pending);
        let folder = target.parent().expect("the setup file is in a folder");
        let _ = fs::remove_dir_all(folder);
        fs::create_dir_all(folder).map_err(|e| store_error("creating", folder, e))?;
        fs::rename(&staging, &target).map_err(|e| store_error("placing", &target, e))?;
        Ok(pending)
    }

    /// Checks the downloaded file again, just before it runs.
    fn matches(&self, pending: &Pending) -> Result<(), ProgramError> {
        let path = self.setup_path(pending);
        let mut file = fs::File::open(&path).map_err(|e| store_error("opening", &path, e))?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let n = file
                .read(&mut buffer)
                .map_err(|e| store_error("reading", &path, e))?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            size += n as u64;
        }
        let actual: [u8; 32] = hash.finalize().into();
        if size != pending.size || from_hex::<32>(&pending.sha256) != Some(actual) {
            return Err(ProgramError::Download(format!(
                "{} changed after it was downloaded (now {size} bytes, SHA-256 {})",
                path.display(),
                to_hex(&actual)
            )));
        }
        Ok(())
    }

    /// The downloaded update that may be installed, if any: newer than the running
    /// version, and not one that failed here unless `retry_failed`.
    fn installable(&self, retry_failed: bool) -> Result<Option<Pending>, ProgramError> {
        let state = self.state()?;
        let Some(pending) = state.pending else {
            return Ok(None);
        };
        if pending.installing
            || (!retry_failed && state.failed.as_deref() == Some(&pending.version))
            || !allowed_on(self.channel(), pending.list)
        {
            return Ok(None);
        }
        let newer = match (
            Version::parse(&pending.version),
            Version::parse(&self.settings.running),
        ) {
            (Ok(pending), Ok(running)) => pending > running,
            _ => false,
        };
        Ok(newer.then_some(pending))
    }

    /// Installs the downloaded update if no user's Android is running. Returns the
    /// version setup is installing, or `None` when there is nothing to install yet or an
    /// Android is running, in which case it waits.
    pub fn install_when_idle(&self) -> Result<Option<String>, ProgramError> {
        if !self.settings.enabled || self.switched_off() {
            return Ok(None);
        }
        let _busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pending) = self.installable(false)? else {
            return Ok(None);
        };
        // Starts are refused from here on, so none can slip in between the look and setup.
        self.status.set_installing(Some(pending.version.clone()));
        if !self.androids.running().is_empty() {
            self.status.set_installing(None);
            return Ok(None);
        }
        self.install(pending).map(Some)
    }

    /// Installs now, as `dsa update` asks: checks first when nothing is downloaded, then
    /// stops every user's Android and runs setup without waiting for them to stop on
    /// their own.
    pub fn install_now(&self) -> Result<ProgramUpdate, ProgramError> {
        let _busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        self.lists(self.channel())?;
        if let Some(version) = self.status.installing() {
            return Ok(ProgramUpdate {
                installing: Some(version.clone()),
                detail: format!("DSA {version} is being installed already"),
            });
        }
        let pending = match self.installable(true)? {
            Some(pending) => pending,
            None => match self.check_locked(true)? {
                CheckOutcome::NotNewer(why) => {
                    return Ok(ProgramUpdate {
                        installing: None,
                        detail: format!("DSA {} is up to date: {why}", self.settings.running),
                    })
                }
                _ => self.installable(true)?.ok_or_else(|| {
                    ProgramError::Store("the downloaded update is not recorded".into())
                })?,
            },
        };
        self.status.set_installing(Some(pending.version.clone()));
        self.status
            .set_progress(ProgramPhase::StoppingAndroid, &pending.version);
        let not_stopped = self.androids.stop_all(self.settings.stop_timeout);
        if !not_stopped.is_empty() {
            self.status.set_installing(None);
            self.status.clear_progress();
            return Err(ProgramError::Refused(format!(
                "these users' Android did not stop in time: {}",
                not_stopped
                    .iter()
                    .map(|(user, state)| format!("{user} ({state})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let version = self.install(pending)?;
        Ok(ProgramUpdate {
            installing: Some(version.clone()),
            detail: format!(
                "installing DSA {version}; every Android was stopped, and the service \
                 restarts when setup is done"
            ),
        })
    }

    /// Runs setup for `pending`. The caller holds `busy` and has set `installing`.
    fn install(&self, pending: Pending) -> Result<String, ProgramError> {
        let version = pending.version.clone();
        let result = self.start_setup(pending);
        if let Err(error) = &result {
            self.status.set_installing(None);
            self.tell_failure(&version, &error.to_string());
        }
        result
    }

    fn start_setup(&self, mut pending: Pending) -> Result<String, ProgramError> {
        if let Err(error) = self.matches(&pending) {
            let mut state = self.state()?;
            state.pending = None;
            self.write_state(&state)?;
            self.remove_downloads(None);
            return Err(error);
        }
        let log_dir = &self.settings.log_dir;
        fs::create_dir_all(log_dir).map_err(|e| store_error("creating", log_dir, e))?;
        let log = log_dir.join(format!("setup-{}.log", pending.version));
        let mut state = self.state()?;
        pending.installing = true;
        pending.log = Some(log.clone());
        state.pending = Some(pending.clone());
        self.write_state(&state)?;
        self.status
            .set_progress(ProgramPhase::Installing, &pending.version);
        self.tell(format!(
            "installing DSA {}; the service restarts when it is done",
            pending.version
        ));
        let setup = self.setup_path(&pending);
        let running = match self.installer.install(&setup, &log) {
            Ok(running) => running,
            Err(why) => {
                state.failed = Some(pending.version.clone());
                pending.installing = false;
                state.pending = Some(pending);
                self.write_state(&state)?;
                return Err(ProgramError::Install(why));
            }
        };
        self.record_setup(&pending.version, running.as_ref());
        self.watch_in_background(running, &pending.version, &log)?;
        Ok(pending.version)
    }

    /// Waits for setup to end. A setup that worked has replaced this service by then. A
    /// service still running a little after setup ended either is the old one, which setup
    /// installed past (it was started again while setup ran, or setup could not stop it),
    /// and restarts into the new files; or setup failed, and the update is reported as
    /// failed.
    fn watch_setup(&self, running: Box<dyn RunningSetup>, version: &str, log: &Path) {
        let code = match running.wait() {
            Ok(code) => code,
            Err(why) => return self.log(&format!("waiting for setup: {why}")),
        };
        thread::sleep(self.settings.settle);
        let _busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut state) = self.state() else {
            return;
        };
        let Some(pending) = state.pending.as_mut() else {
            return;
        };
        if !pending.installing || pending.version != version {
            return;
        }
        self.forget_setup();
        let outcome = self.installer.outcome(code);
        let on_disk = self.installer.installed_version();
        let detail = match after_setup(outcome, code, version, on_disk.as_deref()) {
            AfterSetup::RestartIntoInstalled => {
                let restart = if outcome == SetupOutcome::InstalledRestartPending {
                    " (it asks for the computer to restart to remove files that were in use)"
                } else {
                    ""
                };
                self.log(&format!(
                    "setup installed DSA {version} with exit code {code}{restart}, and this \
                     service is still DSA {}; restarting into the new version",
                    self.settings.running
                ));
                // The record stays as it is, installing, so the new service reports the
                // update once it runs.
                self.restarting.store(true, Ordering::Relaxed);
                self.status.set_progress(ProgramPhase::Restarting, version);
                match self.installer.restart_into_installed() {
                    Ok(()) => return,
                    Err(why) => format!(
                        "setup installed it, but the service could not restart into it: {why}; \
                         restart the DSA service or the computer"
                    ),
                }
            }
            AfterSetup::Failed(why) => format!("{why}; see {}", log.display()),
        };
        pending.installing = false;
        state.failed = Some(version.to_string());
        if let Err(error) = self.write_state(&state) {
            self.log(&error.to_string());
        }
        self.status.set_installing(None);
        self.tell_failure(version, &detail);
    }

    /// Removes setup files no longer needed. The one setup ran from cannot be removed by
    /// the service that setup started while setup is still ending, so it goes on a later
    /// round.
    fn remove_leftovers(&self) {
        let _busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(state) = self.state() {
            self.remove_downloads(state.pending.as_ref().map(|p| p.version.as_str()));
        }
    }

    /// Checks at `timing` and installs a downloaded update whenever no Android is
    /// running, until `stop` is set.
    pub fn run(&self, stop: &AtomicBool, timing: Timing) {
        // The configuration cannot change while the service runs; the settings can, so a
        // loop that the settings turned off keeps waiting for them to turn it on again.
        // Without updates the loop still looks for program files that setup replaced under
        // this service, which a setup run by hand does too.
        let updates_on = self.settings.enabled
            && (self.settings.list_url.is_some() || self.settings.prerelease_list_url.is_some());
        let mut next_check = Instant::now() + timing.first_check;
        let mut next_install = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now >= next_install {
                self.restart_if_replaced();
            }
            // A service that asked to restart stops on its own soon and installs nothing
            // meanwhile.
            let active = updates_on && !self.restarting.load(Ordering::Relaxed);
            if active && now >= next_check {
                if self.switched_off() {
                    next_check = Instant::now() + timing.retry;
                } else {
                    match self.check() {
                        Ok(CheckOutcome::Downloaded(_)) => {}
                        Ok(CheckOutcome::NotNewer(why) | CheckOutcome::FailedBefore(why)) => {
                            self.log(&why)
                        }
                        Ok(CheckOutcome::AlreadyDownloaded(version)) => self.log(&format!(
                            "DSA {version} is downloaded and waits for no Android to be running"
                        )),
                        Err(error) => self.log(&format!("checking for a new version: {error}")),
                    }
                    next_check = Instant::now() + timing.check_interval;
                }
            }
            if now >= next_install {
                if active {
                    self.remove_leftovers();
                    if let Err(error) = self.install_when_idle() {
                        self.log(&error.to_string());
                    }
                }
                next_install = Instant::now() + timing.retry;
            }
            // Wakes once a second at most, to notice `stop` without keeping the CPU busy.
            thread::sleep(Duration::from_secs(1).min(timing.retry));
        }
    }
}

impl UpdateProgram for ProgramUpdates {
    fn update_now(&self) -> Result<ProgramUpdate, String> {
        self.install_now().map_err(|error| error.to_string())
    }
}

/// A user's Android in a service that updates itself: it does not start while setup
/// runs, and it tells the user about the program's updates beside the image's.
pub struct WithProgramStatus {
    pub android: Arc<dyn UserAndroid>,
    pub status: Arc<ProgramStatus>,
}

impl UserAndroid for WithProgramStatus {
    fn state(&self) -> SessionState {
        self.android.state()
    }
    fn last_reason(&self) -> Option<String> {
        match self.status.installing() {
            Some(version) if self.android.state() == SessionState::Stopped => Some(format!(
                "DSA {version} is being installed; start Android again when the service has \
                 restarted"
            )),
            _ => self.android.last_reason(),
        }
    }
    fn problem(&self) -> Option<String> {
        self.android.problem()
    }
    fn start(&self) {
        if self.status.installing().is_none() {
            self.android.start();
        }
    }
    fn stop(&self) {
        self.android.stop();
    }
    fn program_update(&self) -> Option<ProgramProgress> {
        self.status.progress()
    }
    fn update_notice(&self) -> Option<String> {
        let notices: Vec<String> = [self.android.update_notice(), self.status.notices.get()]
            .into_iter()
            .flatten()
            .collect();
        (!notices.is_empty()).then(|| notices.join("\n"))
    }
    fn untold_update_alerts(&self, user: &str) -> Vec<UpdateAlert> {
        let mut alerts = self.android.untold_update_alerts(user);
        alerts.extend(self.status.notices.alerts().untold(user));
        alerts
    }
    fn update_alert_told(&self, user: &str, alert: &UpdateAlert) {
        self.android.update_alert_told(user, alert);
        self.status.notices.alerts().told(user, alert);
    }
    fn image_auto_restart(&self, on: bool) {
        self.android.image_auto_restart(on)
    }
    fn restart_pending(&self) -> Option<String> {
        self.android.restart_pending()
    }
    fn restart_for_update(&self) -> Result<Option<String>, String> {
        // Setup stops every Android and starts the service again; Android starts from the
        // newest image afterwards anyway.
        if let Some(version) = self.status.installing() {
            return Err(format!(
                "DSA {version} is being installed; Android starts with the newest image once \
                 the service has restarted"
            ));
        }
        self.android.restart_for_update()
    }
    fn vm_id(&self) -> Option<String> {
        self.android.vm_id()
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        self.android.watch_tasks()
    }
    fn watch_log(&self, filter: crate::guest_log::LogFilter) -> Option<crate::guest_log::LogWatch> {
        self.android.watch_log(filter)
    }
    fn dump_log(
        &self,
        filter: crate::guest_log::LogFilter,
    ) -> Result<std::sync::mpsc::Receiver<crate::guest_log::LogDumpEvent>, TaskRequestError> {
        self.android.dump_log(filter)
    }
    fn recent_log(&self) -> Vec<crate::guest_log::LogEntry> {
        self.android.recent_log()
    }
    fn collect_diagnostics(
        &self,
    ) -> Result<std::sync::mpsc::Receiver<crate::guest_log::DiagnosticsEvent>, TaskRequestError>
    {
        self.android.collect_diagnostics()
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        self.android.request(request)
    }
    fn freeze(&self) -> Result<(), TaskRequestError> {
        self.android.freeze()
    }
    fn freeze_when_idle(&self, on: bool) {
        self.android.freeze_when_idle(on)
    }
    fn links_in_android(&self, on: bool) {
        self.android.links_in_android(on)
    }
    fn read_addon_image(
        &self,
        addon: crate::addon_disk::Addon,
        offset: u64,
        length: u32,
    ) -> Result<crate::host_api::AddonImagePart, crate::host_api::InstallError> {
        self.android.read_addon_image(addon, offset, length)
    }
    fn remove_arm_translation(
        &self,
    ) -> Result<crate::arm_translation::GuestArmTranslation, crate::host_api::InstallError> {
        self.android.remove_arm_translation()
    }
    fn arm_translation(&self) -> Option<crate::arm_translation::GuestArmTranslation> {
        self.android.arm_translation()
    }
    fn remove_gapps(&self) -> Result<crate::gapps::GuestGapps, crate::host_api::InstallError> {
        self.android.remove_gapps()
    }
    fn gapps(&self) -> Option<crate::gapps::GuestGapps> {
        self.android.gapps()
    }
    fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        self.android.developer_mode(on, port)
    }
    fn adb_auto_connect(&self, on: bool) {
        self.android.adb_auto_connect(on)
    }
    fn adb_auto_connect_state(&self) -> crate::adb_discovery::AutoConnectState {
        self.android.adb_auto_connect_state()
    }
    fn dns_answered_by_host(&self) -> Option<bool> {
        self.android.dns_answered_by_host()
    }
    fn watch_links(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        self.android.watch_links()
    }
    fn open_file(&self, file: &crate::files::FileTransfer) -> Result<(), TaskRequestError> {
        self.android.open_file(file)
    }
    fn share_folders(&self, on: bool, folders: Vec<String>) {
        self.android.share_folders(on, folders)
    }
    fn trust_host_certificates(&self, on: bool) {
        self.android.trust_host_certificates(on)
    }
    fn host_certificates(&self) -> Vec<crate::host_certificates::ImportedCertificate> {
        self.android.host_certificates()
    }
    fn set_root_elevation_config(&self, on: bool, apps: Vec<crate::root_elevation::AppLevel>) {
        self.android.set_root_elevation_config(on, apps)
    }
    fn approve_keep_root(&self, package: &str) -> Result<(), String> {
        self.android.approve_keep_root(package)
    }
    fn android_files(&self) -> Result<String, String> {
        self.android.android_files()
    }
    fn android_files_with_token(&self, token: &str) -> Result<String, String> {
        self.android.android_files_with_token(token)
    }
    fn guest_memory(&self) -> Option<GuestMemoryReport> {
        self.android.guest_memory()
    }
    fn request_guest_memory(&self) -> Result<(), TaskRequestError> {
        self.android.request_guest_memory()
    }
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError> {
        self.android.pointer(event)
    }
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError> {
        self.android.key(event)
    }
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError> {
        self.android.ime(event)
    }
    fn share_clipboard(&self, on: bool) {
        self.android.share_clipboard(on)
    }
    fn host_clipboard(&self, text: String) {
        self.android.host_clipboard(text)
    }
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        self.android.watch_clipboard()
    }
    fn watch_notifications(
        &self,
    ) -> Option<std::sync::mpsc::Receiver<crate::notifications::NotificationEvent>> {
        self.android.watch_notifications()
    }
    fn watch_location(&self) -> Option<std::sync::mpsc::Receiver<crate::location::Wanted>> {
        self.android.watch_location()
    }
    fn host_location(&self, event: crate::location::LocationEvent) {
        self.android.host_location(event)
    }
    fn set_system_settings(&self, settings: crate::system_settings::SystemSettings) {
        self.android.set_system_settings(settings)
    }
    fn gamepad(&self, event: crate::gamepads::GamepadEvent) {
        self.android.gamepad(event)
    }
    fn watch_gamepad_rumble(
        &self,
    ) -> Option<std::sync::mpsc::Receiver<crate::gamepads::GamepadRumble>> {
        self.android.watch_gamepad_rumble()
    }
    fn notification_action(
        &self,
        action: crate::notifications::NotificationAction,
    ) -> Result<(), TaskRequestError> {
        self.android.notification_action(action)
    }
    fn watch_print_jobs(&self) -> Option<Receiver<PrintEvent>> {
        UserAndroid::watch_print_jobs(&*self.android)
    }
    fn print_result(&self, job_id: u32, outcome: &PrintOutcome) -> Result<(), TaskRequestError> {
        UserAndroid::print_result(&*self.android, job_id, outcome)
    }
    fn backup(&self) -> Option<std::sync::Arc<crate::backup::BackupService>> {
        UserAndroid::backup(&*self.android)
    }
    fn backup_now(&self) -> Result<dsa_protocol::control::BackupRunResult, TaskRequestError> {
        UserAndroid::backup_now(&*self.android)
    }
    fn restore_backup(&self) -> Result<crate::session::RestoreOutcome, TaskRequestError> {
        UserAndroid::restore_backup(&*self.android)
    }
    fn watch_passkeys(&self) -> Option<Receiver<crate::passkeys::PasskeyEvent>> {
        UserAndroid::watch_passkeys(&*self.android)
    }
    fn passkey_result(
        &self,
        id: u64,
        result: crate::passkeys::CeremonyResult,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::passkey_result(&*self.android, id, result)
    }
    fn watch_geocoding(&self) -> Option<Receiver<crate::geocode::GeocodeQuery>> {
        UserAndroid::watch_geocoding(&*self.android)
    }
    fn geocode_result(
        &self,
        query_id: u32,
        outcome: &crate::geocode::GeocodeOutcome,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::geocode_result(&*self.android, query_id, outcome)
    }
    fn watch_shares(&self) -> Option<Receiver<crate::share::ShareRequest>> {
        UserAndroid::watch_shares(&*self.android)
    }
    fn share_result(
        &self,
        share_id: u32,
        outcome: &crate::share::ShareOutcome,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::share_result(&*self.android, share_id, outcome)
    }
    fn set_backup(
        &self,
        on: bool,
        delete_backups: bool,
    ) -> Result<crate::backup::BackupReport, TaskRequestError> {
        UserAndroid::set_backup(&*self.android, on, delete_backups)
    }
    fn keymint_key_state(&self) -> Option<crate::session::KeyMintKeyState> {
        UserAndroid::keymint_key_state(&*self.android)
    }
    fn forget_keymint_root_key(&self) {
        UserAndroid::forget_keymint_root_key(&*self.android)
    }
    fn serve_language(&self) -> Option<crate::language::relay::Served> {
        UserAndroid::serve_language(&*self.android)
    }
    fn language_answer(
        &self,
        server: u64,
        envelope: dsa_protocol::control::Envelope,
    ) -> Result<(), String> {
        UserAndroid::language_answer(&*self.android, server, envelope)
    }
    fn language_server_gone(&self, server: u64) {
        UserAndroid::language_server_gone(&*self.android, server)
    }
    fn watch_background(&self) -> Option<BackgroundWatch> {
        UserAndroid::watch_background(&*self.android)
    }
    fn watch_app_audio(&self) -> Option<crate::app_audio::AppAudioWatch> {
        UserAndroid::watch_app_audio(&*self.android)
    }
    fn set_app_volume(
        &self,
        change: &crate::app_audio::VolumeChange,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::set_app_volume(&*self.android, change)
    }
    fn background_apps(&self) -> (Vec<BackgroundApp>, Vec<MediaSession>) {
        UserAndroid::background_apps(&*self.android)
    }
    fn background_action(&self, request: BackgroundRequest) -> Result<(), TaskRequestError> {
        UserAndroid::background_action(&*self.android, request)
    }
    fn watch_widgets(&self) -> Option<crate::session::WidgetWatch> {
        UserAndroid::watch_widgets(&*self.android)
    }
    fn widgets(&self) -> Vec<(i32, WidgetProvider)> {
        UserAndroid::widgets(&*self.android)
    }
    fn widget_providers(&self) -> Result<Vec<WidgetProvider>, WidgetError> {
        UserAndroid::widget_providers(&*self.android)
    }
    fn add_widget(&self, provider: &str) -> Result<i32, WidgetError> {
        UserAndroid::add_widget(&*self.android, provider)
    }
    fn remove_widget(&self, widget_id: i32) -> Result<(), TaskRequestError> {
        UserAndroid::remove_widget(&*self.android, widget_id)
    }
    fn reconfigure_widget(&self, widget_id: i32) -> Result<(), WidgetError> {
        UserAndroid::reconfigure_widget(&*self.android, widget_id)
    }
    fn resize_widget(&self, widget_id: i32, size: WidgetSize) -> Result<(), TaskRequestError> {
        UserAndroid::resize_widget(&*self.android, widget_id, size)
    }
    fn widget_pointer(
        &self,
        pointer: dsa_protocol::control::WidgetPointer,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::widget_pointer(&*self.android, pointer)
    }
    fn install_apk(
        &self,
        name: &str,
        size: u64,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        UserAndroid::install_apk(&*self.android, name, size, content)
    }
    fn install_apk_for(
        &self,
        name: &str,
        size: u64,
        abi: Option<&str>,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        UserAndroid::install_apk_for(&*self.android, name, size, abi, content)
    }
    fn install_parts(
        &self,
        name: &str,
        parts: &[InstallPart],
        abi: Option<&str>,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        UserAndroid::install_parts(&*self.android, name, parts, abi, content)
    }
    fn set_run_as_arm(
        &self,
        package: &str,
        on: bool,
        erase_data: bool,
    ) -> Result<crate::native_abi::RunAsArmOutcome, TaskRequestError> {
        UserAndroid::set_run_as_arm(&*self.android, package, on, erase_data)
    }
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        UserAndroid::package_state(&*self.android, package)
    }
    fn refresh_apps(&self) -> Result<(), TaskRequestError> {
        UserAndroid::refresh_apps(&*self.android)
    }
    fn drag_out(
        &self,
        out: crate::drag::DragOut,
    ) -> Result<Option<crate::drag::DragOffer>, TaskRequestError> {
        UserAndroid::drag_out(&*self.android, out)
    }
    fn read_drag(
        &self,
        read: crate::drag::DragRead,
    ) -> Result<crate::drag::DragChunk, TaskRequestError> {
        UserAndroid::read_drag(&*self.android, read)
    }
    fn end_drag(&self, drag_id: u32, dropped: bool) -> Result<(), TaskRequestError> {
        UserAndroid::end_drag(&*self.android, drag_id, dropped)
    }
    fn open_app_settings(&self, package: Option<&str>) -> Result<(), TaskRequestError> {
        UserAndroid::open_app_settings(&*self.android, package)
    }
    fn open_app_settings_page(
        &self,
        package: Option<&str>,
        page: AppSettingsPage,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::open_app_settings_page(&*self.android, package, page)
    }
    fn uninstall_app(&self, package: &str) -> Result<UninstallOutcome, TaskRequestError> {
        UserAndroid::uninstall_app(&*self.android, package)
    }
    fn watch_accessibility(&self, task_id: i32) -> Option<AccessibilityWatch> {
        UserAndroid::watch_accessibility(&*self.android, task_id)
    }
    fn accessibility_action(
        &self,
        task_id: i32,
        action: &ActionRequest,
    ) -> Result<(), TaskRequestError> {
        UserAndroid::accessibility_action(&*self.android, task_id, action)
    }
}

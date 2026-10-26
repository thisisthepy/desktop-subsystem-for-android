//! Updating DSA itself as the service runs it: the check of the program list, the
//! download of the new setup file, and running it once no user's Android is running.
//!
//! ```text
//! <dir>/state.json            the downloaded update, and the version that failed here
//! <dir>/<version>/<file>      the setup file, as the list's URL names it
//! <dir>/.download-<version>   a download in progress
//! ```
//!
//! Setup stops the service, replaces it and starts the new one. Before it runs, the state
//! records that it is installing; the service that starts next compares its own version
//! with that record and tells the user whether the update worked. A failed version is not
//! installed again on its own, since it would most likely fail the same way; `dsa update`
//! tries it again, and a newer version replaces it.
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

use super::manifest::is_plain_name;
use super::program::{self, ProgramDecision, ProgramError, ProgramManifest, Version};
use super::service::UpdateNotices;
use super::store::{download_checked, DownloadFailure, Expected, Fetch};
use super::{from_hex, to_hex};
use crate::host_api::{ProgramUpdate, UpdateProgram, UserAndroid, UserAndroids};
use crate::ime::ImeEvent;
use crate::input::PointerEvent;
use crate::keys::KeyEvent;
use crate::session::SessionState;
use crate::tasks::{TaskRequest, TaskRequestError, TaskWatch};

/// Runs a setup file.
pub trait ProgramInstaller: Send + Sync {
    /// Starts `setup` quietly, writing its log to `log`, as a process apart from the
    /// service: setup stops the service and replaces it, and must outlive it. Returns
    /// once setup has started.
    fn install(&self, setup: &Path, log: &Path) -> Result<Box<dyn RunningSetup>, String>;
}

/// A setup that has started.
pub trait RunningSetup: Send {
    /// Waits for setup to end and returns its exit code.
    fn wait(self: Box<Self>) -> Result<u32, String>;
}

/// How the service updates the program.
#[derive(Debug, Clone)]
pub struct ProgramSettings {
    /// The signed program list. Its signature is at the same URL plus `.sig`. Without it
    /// nothing is checked.
    pub list_url: Option<String>,
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
}

impl ProgramStatus {
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
                self.tell(format!("DSA was updated to {}", self.settings.running));
                state = ProgramState::default();
                self.remove_downloads(None);
            } else {
                let log = pending
                    .log
                    .as_ref()
                    .map(|log| format!("see {}", log.display()))
                    .unwrap_or_else(|| "setup left no log".into());
                self.tell(self.failure_notice(&pending.version, &log));
                state.failed = Some(pending.version.clone());
                state.pending = Some(Pending {
                    installing: false,
                    ..pending
                });
            }
            return self.write_state(&state);
        }
        if arrived {
            // Installed some other way meanwhile.
            state.pending = None;
            self.remove_downloads(None);
            return self.write_state(&state);
        }
        Ok(())
    }

    /// Reads the program list and downloads the version it offers when that is newer
    /// than the running one. Never fetches anything while updates are turned off.
    pub fn check(&self) -> Result<CheckOutcome, ProgramError> {
        let _busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        self.check_locked(false)
    }

    fn list_url(&self) -> Result<&str, ProgramError> {
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
        self.settings
            .list_url
            .as_deref()
            .ok_or_else(|| ProgramError::Off("the configuration names no program list".into()))
    }

    /// [`check`](Self::check) with `busy` held. `retry_failed` downloads a version that
    /// failed here before, as `dsa update` asks.
    fn check_locked(&self, retry_failed: bool) -> Result<CheckOutcome, ProgramError> {
        let list_url = self.list_url()?;
        let mut bytes = Vec::new();
        self.fetch
            .fetch(list_url, &mut bytes)
            .map_err(|e| ProgramError::Download(format!("the program list: {e}")))?;
        let mut signature = Vec::new();
        self.fetch
            .fetch(&format!("{list_url}.sig"), &mut signature)
            .map_err(|e| ProgramError::BadSignature(format!("fetching the signature: {e}")))?;
        let list = program::verify(
            &bytes,
            &String::from_utf8_lossy(&signature),
            &self.settings.keys,
        )?;
        let offered = match program::decide(&self.settings.running, &list)? {
            ProgramDecision::NotNewer(why) => return Ok(CheckOutcome::NotNewer(why)),
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
        if let Some(pending) = &state.pending {
            if pending.version == list.version
                && pending.sha256.eq_ignore_ascii_case(&list.setup.sha256)
                && self.matches(pending).is_ok()
            {
                return Ok(CheckOutcome::AlreadyDownloaded(pending.version.clone()));
            }
        }
        let pending = self.download(&list, &offered)?;
        state.pending = Some(pending);
        self.write_state(&state)?;
        self.remove_downloads(Some(&list.version));
        self.tell(format!(
            "DSA {} is downloaded and installs when Android is not running (`dsa update` \
             installs it now)",
            list.version
        ));
        Ok(CheckOutcome::Downloaded(list.version))
    }

    fn download(&self, list: &ProgramManifest, offered: &Version) -> Result<Pending, ProgramError> {
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
        self.list_url()?;
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
        let not_stopped = self.androids.stop_all(self.settings.stop_timeout);
        if !not_stopped.is_empty() {
            self.status.set_installing(None);
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
            self.tell(self.failure_notice(&version, &error.to_string()));
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
        let updates = self
            .me
            .upgrade()
            .expect("the updates are alive while they run");
        let version = pending.version.clone();
        let watch = move || updates.watch_setup(running, &version, &log);
        thread::Builder::new()
            .name("dsa-setup".into())
            .spawn(watch)
            .map_err(|e| ProgramError::Install(format!("watching setup: {e}")))?;
        Ok(pending.version)
    }

    /// Waits for setup to end. A setup that worked has replaced this service by then, so
    /// a service still running a little after setup ended means the update failed.
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
        pending.installing = false;
        state.failed = Some(version.to_string());
        if let Err(error) = self.write_state(&state) {
            self.log(&error.to_string());
        }
        self.status.set_installing(None);
        self.tell(self.failure_notice(
            version,
            &format!(
                "setup ended with exit code {code} and the service was not replaced; see {}",
                log.display()
            ),
        ));
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
        if !self.settings.enabled || self.settings.list_url.is_none() {
            return;
        }
        let mut next_check = Instant::now() + timing.first_check;
        let mut next_install = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now >= next_check && self.switched_off() {
                next_check = Instant::now() + timing.retry;
            } else if now >= next_check {
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
            if now >= next_install {
                self.remove_leftovers();
                if let Err(error) = self.install_when_idle() {
                    self.log(&error.to_string());
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
    fn start(&self) {
        if self.status.installing().is_none() {
            self.android.start();
        }
    }
    fn stop(&self) {
        self.android.stop();
    }
    fn update_notice(&self) -> Option<String> {
        let notices: Vec<String> = [self.android.update_notice(), self.status.notices.get()]
            .into_iter()
            .flatten()
            .collect();
        (!notices.is_empty()).then(|| notices.join("\n"))
    }
    fn vm_id(&self) -> Option<String> {
        self.android.vm_id()
    }
    fn guest_memory(&self) -> Option<crate::memory::GuestMemory> {
        self.android.guest_memory()
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        self.android.watch_tasks()
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        self.android.request(request)
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
}

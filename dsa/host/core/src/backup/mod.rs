//! App data backups of a user's Android, kept encrypted in a backup folder of the user's own.
//!
//! The guest's backup transport is the agent's (`HostBackupTransport`, the transport Android's
//! backup manager uses). It sends each app's data to the host: key-value records, the tar
//! stream of a full backup, and the app's APKs, so a restore after a data reset can install
//! the app before its data comes back. The host keeps them in the user's backup folder
//! ([`store`]), each file encrypted with the user's backup key ([`crypto`]). The key is kept
//! by the host's credential store ([`crate::host_keys`]) and never reaches the guest.
//!
//! The folder lives outside the guest's disk, under the user's profile, so a data reset (which
//! replaces only that disk) leaves it, and [`BackupService`] restores from it: the session
//! installs the apps the backup has APKs of, then asks the agent to restore every app's data,
//! which Android's backup manager reads back through the transport. On another computer, or
//! for another user, the same folder reads the same once its recovery key
//! ([`crypto::recovery_key`]) is entered there.
//!
//! Without the key the folder is unreadable: the files are sealed with XChaCha20-Poly1305
//! under a key derived from it, and even the list of apps is in the encrypted index.
//!
//! The guest's messages are handled one at a time on a thread of the service's own, so the
//! session's thread never waits on the disk. Each is answered through the reply the session
//! gives [`BackupService::attach`].
//!
//! Backup is the user's choice, and off until they turn it on ([`BackupService::set_enabled`],
//! from the settings app or `dsa backup on`): it keeps a second copy of their apps' data on
//! the computer's disk. While it is off the service makes no folder and no key, answers every
//! request of the guest's transport as unavailable, and the session tells the agent to keep
//! Android's backup manager from backing up to the host. Turning it off again stops all of
//! that; the backups already made stay for a later restore unless the user has them deleted.
//! The choice is kept by a [`BackupSwitch`], outside the backup folder.

pub mod crypto;
pub mod records;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dsa_protocol::control::{self, envelope::Body, BackupDataKind, BackupStatus};

use crate::apk;
use crate::host_keys::{HostKeyStore, KeyError, KeyPurpose, Secret};
use crypto::{BackupKey, DecryptingReader};
use store::{FileEntry, Folder, PackageEntry};

/// The largest piece of data in one message, either way.
pub const PIECE_BYTES: usize = 256 * 1024;

/// The most a key-value backup of one package may hold. The records are merged in memory.
pub const MAX_KEY_VALUE_BYTES: u64 = 64 * 1024 * 1024;

/// The most a full backup of one package may hold.
pub const MAX_FULL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The most one APK may hold.
pub const MAX_APK_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How long a backup of every app may take before the host stops waiting.
pub const BACKUP_RUN_WAIT: Duration = Duration::from_secs(60 * 60);

/// How long a restore of every app's data may take before the host stops waiting.
pub const RESTORE_WAIT: Duration = Duration::from_secs(60 * 60);

/// The folder's name under the user's profile (their home folder), on every host.
pub const FOLDER_NAME: &str = "DSA Backup";

/// The package Android's backup manager keeps the package manager's metadata under. It has no
/// APKs, but its data is restored first.
pub const PACKAGE_MANAGER: &str = "@pm@";

/// Where a user's backup stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    pub available: bool,
    pub problem: String,
    pub folder: PathBuf,
    pub last_backup_ms: i64,
    pub packages: u32,
    pub size: u64,
    pub last_error: String,
    pub last_restore_ms: i64,
    /// A backup or restore just run: the apps it covered, those that failed, and the apps a
    /// restore installed.
    pub succeeded: u32,
    pub failed: u32,
    pub installed: u32,
    /// Whether the user turned backup on.
    pub enabled: bool,
}

/// What a person reads when backup is off and something asks for it.
pub const BACKUP_OFF: &str = "backup is off; turn it on in the settings app or with dsa backup on";

/// Keeps whether the user turned backup on. Off when nothing was ever kept.
pub trait BackupSwitch: Send + Sync {
    fn enabled(&self) -> Result<bool, String>;
    fn set_enabled(&self, on: bool) -> Result<(), String>;
}

/// A [`BackupSwitch`] in memory, off at first: for a host that keeps the choice nowhere else,
/// and for tests.
#[derive(Default)]
pub struct MemorySwitch(std::sync::atomic::AtomicBool);

impl BackupSwitch for MemorySwitch {
    fn enabled(&self) -> Result<bool, String> {
        Ok(self.0.load(Ordering::SeqCst))
    }
    fn set_enabled(&self, on: bool) -> Result<(), String> {
        self.0.store(on, Ordering::SeqCst);
        Ok(())
    }
}

/// The choice as a file keeps it.
#[derive(serde::Serialize, serde::Deserialize)]
struct SwitchFile {
    #[serde(default)]
    enabled: bool,
}

/// A [`BackupSwitch`] in a JSON file of the service's own settings folder
/// (`<settings>/users/<user>.backup.json`, `{"enabled": true}`), which users cannot write to,
/// so the choice the service acts on is the one the user made through it. A missing file is
/// off.
pub struct FileSwitch {
    path: PathBuf,
}

impl FileSwitch {
    pub fn new(path: impl Into<PathBuf>) -> FileSwitch {
        FileSwitch { path: path.into() }
    }

    /// The switch of `user` in the service's settings folder `settings_dir`.
    pub fn for_user(settings_dir: &Path, user: &str) -> Result<FileSwitch, String> {
        let stem = crate::settings::user_file_stem(user)?;
        Ok(FileSwitch::new(
            settings_dir
                .join("users")
                .join(format!("{stem}.backup.json")),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BackupSwitch for FileSwitch {
    fn enabled(&self) -> Result<bool, String> {
        match std::fs::read(&self.path) {
            Ok(data) => serde_json::from_slice::<SwitchFile>(&data)
                .map(|file| file.enabled)
                .map_err(|e| format!("{} cannot be read: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{} cannot be read: {e}", self.path.display())),
        }
    }

    fn set_enabled(&self, on: bool) -> Result<(), String> {
        let data = serde_json::to_vec(&SwitchFile { enabled: on }).map_err(|e| e.to_string())?;
        crate::host_keys::write_replacing(&self.path, &data)
            .map_err(|e| format!("{} cannot be written: {e}", self.path.display()))
    }
}

/// Makes the backup folder before its first use, as the platform wants it made (on Windows,
/// with an access list that keeps it the user's although the service writes it).
pub type PrepareFolder = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// One package in the backup, for a restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackedUpPackage {
    pub package: String,
    pub version_code: i64,
    pub key_value: bool,
    pub full: bool,
    /// The APKs at `version_code`, with their sizes.
    pub apks: Vec<(String, u64)>,
}

/// An upload under way.
struct Upload {
    file: store::NewFile,
    offset: u64,
}

/// A read under way, positioned at `offset`.
struct Fetch {
    reader: DecryptingReader<File>,
    key: (String, i32, String),
    offset: u64,
    size: u64,
}

/// A user's backup service: the folder, the key, and what is under way.
pub struct BackupService {
    folder_dir: PathBuf,
    keys: Arc<dyn HostKeyStore>,
    made_on: String,
    state: Mutex<State>,
    queue: Mutex<Option<Sender<Body>>>,
    runs: Mutex<HashMap<u64, Sender<control::BackupRunResult>>>,
    next_run: AtomicU64,
    file_access: Option<FileAccess>,
    switch: Arc<dyn BackupSwitch>,
    prepare: Option<PrepareFolder>,
}

/// Runs what it is given with the file rights of the user the folder belongs to, as
/// [`crate::file_share::ShareHost::as_user`] does; `Err` says why it could not.
///
/// A service that runs as another account (a Linux service running as root) gives one, so
/// that every file of the folder is made, read and removed with the user's own rights and is
/// the user's own. Without it, the folder is used with the process's rights, as on a host
/// whose service runs as the user.
pub type FileAccess = Arc<dyn Fn(&mut dyn FnMut()) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
struct State {
    folder: Option<Folder>,
    uploads: HashMap<u64, Upload>,
    fetches: HashMap<u64, Fetch>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Why the backup cannot be used, as the guest is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable(pub String);

/// An APK's name is a plain file name ending in `.apk`.
pub fn valid_apk_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.ends_with(".apk")
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_package(package: &str) -> bool {
    package == PACKAGE_MANAGER || apk::valid_package_name(package)
}

fn answer(request_id: u64, status: BackupStatus, message: impl Into<String>) -> Body {
    Body::BackupAnswer(control::BackupAnswer {
        request_id,
        status: status as i32,
        message: message.into(),
        apks_wanted: false,
    })
}

impl BackupService {
    /// The backup of the user whose credential store is `keys`, kept in `folder`. `made_on`
    /// names this computer, for a folder made here.
    pub fn new(folder: PathBuf, keys: Arc<dyn HostKeyStore>, made_on: &str) -> BackupService {
        BackupService {
            folder_dir: folder,
            keys,
            made_on: made_on.to_string(),
            state: Mutex::new(State::default()),
            queue: Mutex::new(None),
            runs: Mutex::new(HashMap::new()),
            next_run: AtomicU64::new(1),
            file_access: None,
            switch: Arc::new(MemorySwitch::default()),
            prepare: None,
        }
    }

    /// Uses the folder only through `access`, with the rights of the user it belongs to.
    pub fn with_file_access(mut self, access: FileAccess) -> BackupService {
        self.file_access = Some(access);
        self
    }

    /// Keeps whether the user turned backup on with `switch`. Without one, the choice lasts
    /// only as long as the service, and starts off.
    pub fn with_switch(mut self, switch: Arc<dyn BackupSwitch>) -> BackupService {
        self.switch = switch;
        self
    }

    /// Has `prepare` make the folder before the first backup writes to it.
    pub fn with_prepare(mut self, prepare: PrepareFolder) -> BackupService {
        self.prepare = Some(prepare);
        self
    }

    /// Whether the user turned backup on. A choice that cannot be read counts as off, so a
    /// broken settings file never starts backups the user did not ask for.
    pub fn enabled(&self) -> bool {
        match self.switch.enabled() {
            Ok(on) => on,
            Err(why) => {
                eprintln!("backup: whether backup is on cannot be read, so it is off: {why}");
                false
            }
        }
    }

    /// Turns backup on or off. Off, nothing under way goes on; with `delete_backups` the files
    /// the backup folder holds and the backup key are removed too (files of the user's own in
    /// the folder stay). Returns where the backup stands after it.
    pub fn set_enabled(&self, on: bool, delete_backups: bool) -> Result<BackupReport, String> {
        if on && delete_backups {
            return Err("backups are deleted only when backup is turned off".into());
        }
        self.switch.set_enabled(on)?;
        eprintln!("backup: turned {}", if on { "on" } else { "off" });
        if !on {
            self.let_go(|state| {
                state.folder = None;
                state.uploads.clear();
                state.fetches.clear();
            });
            if delete_backups {
                self.delete_backups()?;
            }
        }
        Ok(self.report())
    }

    /// Removes what the backup made: the folder's files and the backup key.
    fn delete_backups(&self) -> Result<(), String> {
        self.as_owner(|| store::delete(&self.folder_dir).map_err(Unavailable))
            .map_err(|Unavailable(why)| why)?;
        self.keys
            .remove(KeyPurpose::Backup)
            .map_err(|e| format!("the backups were deleted, but not their key: {e}"))?;
        eprintln!(
            "backup: deleted the backups in {} and their key",
            self.folder_dir.display()
        );
        Ok(())
    }

    /// Runs `f` with the folder owner's file rights, when the service was given a way to.
    fn as_owner<T>(&self, f: impl FnOnce() -> Result<T, Unavailable>) -> Result<T, Unavailable> {
        let Some(access) = &self.file_access else {
            return f();
        };
        let mut f = Some(f);
        let mut out = None;
        access(&mut || {
            if let Some(f) = f.take() {
                out = Some(f());
            }
        })
        .map_err(|why| {
            Unavailable(format!(
                "{} cannot be used with its owner's rights: {why}",
                self.folder_dir.display()
            ))
        })?;
        out.unwrap_or_else(|| {
            Err(Unavailable(format!(
                "{} was not used: nothing ran with its owner's rights",
                self.folder_dir.display()
            )))
        })
    }

    pub fn folder(&self) -> &Path {
        &self.folder_dir
    }

    /// Starts the thread that handles the guest's messages; each answer goes to `reply`.
    pub fn attach(self: &Arc<Self>, reply: Arc<dyn Fn(Body) + Send + Sync>) {
        let (tx, rx) = mpsc::channel::<Body>();
        let service = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("dsa-backup".into())
            .spawn(move || {
                for body in rx {
                    let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        service.handle(body, &*reply)
                    }));
                    if handled.is_err() {
                        eprintln!("backup: a message from Android could not be handled");
                    }
                }
            });
        match spawned {
            Ok(_) => *self.queue.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx),
            Err(error) => eprintln!("backup: no thread to keep backups on: {error}"),
        }
    }

    /// Takes a message of the guest's backup transport, if `body` is one; `false` for any
    /// other message.
    pub fn guest_message(&self, body: Body) -> bool {
        match body {
            Body::BackupRunResult(result) => {
                let waiting = self
                    .runs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&result.request_id);
                match waiting {
                    Some(waiting) => {
                        let _ = waiting.send(result);
                    }
                    None => eprintln!(
                        "backup: dropped the result of run {}, which nobody waits for",
                        result.request_id
                    ),
                }
                true
            }
            body @ (Body::BackupUpload(_)
            | Body::BackupCommit(_)
            | Body::BackupDiscard(_)
            | Body::BackupClear(_)
            | Body::BackupQuery(_)
            | Body::BackupFetch(_)) => {
                let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                match queue.as_ref() {
                    Some(queue) => {
                        let _ = queue.send(body);
                    }
                    None => {
                        eprintln!("backup: dropped a message from Android: nothing keeps backups")
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The key, made with the folder when neither exists. A folder that exists while the
    /// credential store has no key needs its recovery key; making a new key then would leave
    /// the folder unreadable.
    fn key(&self) -> Result<BackupKey, Unavailable> {
        let kept = self
            .keys
            .load(KeyPurpose::Backup)
            .map_err(|e| Unavailable(key_problem(&e)))?;
        if let Some(secret) = kept {
            return Ok(BackupKey::new(&secret));
        }
        match store::read_metadata(&self.folder_dir) {
            Ok(Some(_)) => Err(Unavailable(format!(
                "{} holds a backup, but this computer has no key for it; enter its recovery key \
                 (dsa backup key import) to use it here",
                self.folder_dir.display()
            ))),
            Ok(None) => {
                let secret =
                    crate::host_keys::load_or_create(self.keys.as_ref(), KeyPurpose::Backup)
                        .map_err(|e| Unavailable(key_problem(&e)))?;
                Ok(BackupKey::new(&secret))
            }
            Err(e) => Err(Unavailable(e.to_string())),
        }
    }

    /// Runs `f` on the open folder, opening it first. While backup is off only a folder an
    /// earlier backup made is opened, to be read: nothing makes one, or its key.
    fn with_folder<T>(
        &self,
        f: impl FnOnce(&mut State) -> Result<T, String>,
    ) -> Result<T, Unavailable> {
        let enabled = self.enabled();
        self.as_owner(|| {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.folder.is_none() {
                let exists = matches!(store::read_metadata(&self.folder_dir), Ok(Some(_)));
                if !enabled && !exists {
                    return Err(Unavailable(BACKUP_OFF.into()));
                }
                if !exists && !self.folder_dir.is_dir() {
                    if let Some(prepare) = &self.prepare {
                        prepare(&self.folder_dir).map_err(Unavailable)?;
                    }
                }
                let key = self.key()?;
                let folder = Folder::open(&self.folder_dir, key, &self.made_on)
                    .map_err(|e| Unavailable(e.to_string()))?;
                state.folder = Some(folder);
            }
            f(&mut state).map_err(Unavailable)
        })
    }

    /// Where the backup stands. It never makes a folder or a key. While backup is off it is
    /// not available, with no problem, and still counts what an earlier backup left.
    pub fn report(&self) -> BackupReport {
        let enabled = self.enabled();
        let mut report = BackupReport {
            folder: self.folder_dir.clone(),
            enabled,
            ..BackupReport::default()
        };
        let exists = match self.as_owner(|| Ok(store::read_metadata(&self.folder_dir))) {
            Ok(metadata) => matches!(metadata, Ok(Some(_))),
            Err(Unavailable(why)) => {
                report.problem = why;
                return report;
            }
        };
        if !enabled {
            if exists {
                // What is left can be counted, so the user knows what deleting it removes.
                if let Ok(index) =
                    self.with_folder(|state| Ok(state.folder.as_ref().expect("open").index.clone()))
                {
                    report.last_backup_ms = index.last_backup_ms;
                    report.last_restore_ms = index.last_restore_ms;
                    report.packages = index
                        .packages
                        .keys()
                        .filter(|p| p.as_str() != PACKAGE_MANAGER)
                        .count() as u32;
                    report.size = index.packages.values().map(PackageEntry::size).sum();
                }
            }
            return report;
        }
        if !exists {
            // A first backup makes the folder; until then, say only whether it could.
            match self.keys.load(KeyPurpose::Backup) {
                Ok(_) => report.available = true,
                Err(e) => report.problem = key_problem(&e),
            }
            return report;
        }
        match self.with_folder(|state| {
            let folder = state.folder.as_ref().expect("open");
            Ok(folder.index.clone())
        }) {
            Ok(index) => {
                report.available = true;
                report.last_backup_ms = index.last_backup_ms;
                report.last_restore_ms = index.last_restore_ms;
                report.last_error = index.last_error.clone();
                report.packages = index
                    .packages
                    .keys()
                    .filter(|p| p.as_str() != PACKAGE_MANAGER)
                    .count() as u32;
                report.size = index.packages.values().map(PackageEntry::size).sum();
            }
            Err(Unavailable(why)) => report.problem = why,
        }
        report
    }

    /// The recovery key of the user's backup key, made with the folder's first use when there
    /// is none yet.
    pub fn recovery_key(&self) -> Result<String, Unavailable> {
        // While backup is off a key an earlier backup made is shown, but none is made.
        if self.enabled() {
            let _ = self.as_owner(|| self.key())?;
        }
        let secret = self
            .keys
            .load(KeyPurpose::Backup)
            .map_err(|e| Unavailable(key_problem(&e)))?
            .ok_or_else(|| {
                Unavailable(if self.enabled() {
                    "the backup key was not kept".into()
                } else {
                    format!("there is no backup key yet: {BACKUP_OFF}")
                })
            })?;
        Ok(crypto::recovery_key(&secret))
    }

    /// Keeps the key `text` carries as the user's backup key. A key that does not open the
    /// folder is refused, and the key kept stays.
    pub fn use_recovery_key(&self, text: &str) -> Result<(), String> {
        if !self.enabled() {
            return Err(format!(
                "a recovery key is kept only while backup is on: {BACKUP_OFF}"
            ));
        }
        let secret: Secret = crypto::parse_recovery_key(text)?;
        let key = BackupKey::new(&secret);
        let metadata = self
            .as_owner(|| Ok(store::read_metadata(&self.folder_dir)))
            .map_err(|Unavailable(why)| why)?;
        if let Ok(Some(metadata)) = metadata {
            if metadata.key_id != key.id_hex() {
                return Err(format!(
                    "this recovery key is not the one {} was made with",
                    self.folder_dir.display()
                ));
            }
        }
        self.keys
            .store(KeyPurpose::Backup, &secret)
            .map_err(|e| key_problem(&e))?;
        self.let_go(|state| {
            state.folder = None;
            state.uploads.clear();
            state.fetches.clear();
        });
        eprintln!("backup: a recovery key was entered; the backup folder is read with it");
        Ok(())
    }

    /// Changes `state` with the owner's file rights, since letting go of an upload removes
    /// its unfinished file. Without those rights it changes all the same: an unfinished file
    /// left behind is only removed later.
    fn let_go(&self, change: impl FnOnce(&mut State)) {
        let mut change = Some(change);
        let done = self.as_owner(|| {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(change) = change.take() {
                change(&mut state);
            }
            Ok(())
        });
        if let (Err(Unavailable(why)), Some(change)) = (done, change.take()) {
            eprintln!("backup: {why}");
            change(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }

    /// The restore set's token and the packages the backup holds.
    pub fn contents(&self) -> Result<(u64, Vec<BackedUpPackage>), Unavailable> {
        self.with_folder(|state| {
            let folder = state.folder.as_ref().expect("open");
            let packages = folder
                .index
                .packages
                .iter()
                .map(|(package, entry)| BackedUpPackage {
                    package: package.clone(),
                    version_code: entry.version_code,
                    key_value: entry.key_value.is_some(),
                    full: entry.full.is_some(),
                    apks: if entry.apk_version == entry.version_code {
                        entry
                            .apks
                            .iter()
                            .map(|(name, file)| (name.clone(), file.size))
                            .collect()
                    } else {
                        Vec::new()
                    },
                })
                .collect();
            Ok((folder.metadata.token, packages))
        })
    }

    /// The APKs of `package`, read one after another in the order of `names`.
    pub fn apks(&self, package: &str, names: &[String]) -> Result<Box<dyn Read + Send>, String> {
        self.with_folder(|state| {
            let folder = state.folder.as_ref().expect("open");
            let entry = folder
                .index
                .packages
                .get(package)
                .ok_or_else(|| format!("the backup holds nothing of {package}"))?;
            let mut chain: Box<dyn Read + Send> = Box::new(std::io::empty());
            for name in names {
                let file = entry
                    .apks
                    .get(name)
                    .ok_or_else(|| format!("the backup holds no {name} of {package}"))?;
                let reader = folder.open_file(file).map_err(|e| e.to_string())?;
                chain = Box::new(chain.chain(reader));
            }
            Ok(chain)
        })
        .map_err(|Unavailable(why)| why)
    }

    /// Records that a restore ran, and what went wrong in it, if anything.
    pub fn restored(&self, error: &str) {
        let _ = self.with_folder(|state| {
            let folder = state.folder.as_mut().expect("open");
            folder.index.last_restore_ms = now_ms();
            folder.index.last_error = error.to_string();
            folder.save_index().map_err(|e| e.to_string())
        });
    }

    /// Records what went wrong in a backup run, or that nothing did.
    pub fn ran(&self, error: &str) {
        let _ = self.with_folder(|state| {
            let folder = state.folder.as_mut().expect("open");
            folder.index.last_error = error.to_string();
            folder.save_index().map_err(|e| e.to_string())
        });
    }

    /// Sends a request to the agent through `send` and waits at most `wait` for its result.
    /// `make` builds the request from its number.
    pub fn run_in_guest(
        &self,
        make: impl FnOnce(u64) -> Body,
        send: impl FnOnce(Body) -> Result<(), String>,
        wait: Duration,
    ) -> Result<control::BackupRunResult, String> {
        let request_id = self.next_run.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        let result = send(make(request_id)).and_then(|()| {
            rx.recv_timeout(wait).map_err(|_| {
                format!(
                    "Android did not finish within {} minutes",
                    wait.as_secs() / 60
                )
            })
        });
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        result
    }

    /// Android stopped: nothing under way goes on, and nobody waits for it.
    pub fn guest_gone(&self) {
        self.let_go(|state| {
            state.uploads.clear();
            state.fetches.clear();
        });
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn handle(&self, body: Body, reply: &dyn Fn(Body)) {
        if !self.enabled() {
            // Nothing is read or written while backup is off; the agent keeps Android's
            // backup manager from asking, and anything that asks all the same is told why.
            if let Some(answer) = refusal(&body, BACKUP_OFF) {
                reply(answer);
            }
            return;
        }
        match body {
            Body::BackupUpload(upload) => reply(self.upload(upload)),
            Body::BackupCommit(commit) => reply(self.commit(commit)),
            Body::BackupDiscard(discard) => self.let_go(|state| {
                state.uploads.remove(&discard.request_id);
                state.fetches.remove(&discard.request_id);
            }),
            Body::BackupClear(clear) => reply(self.clear(clear)),
            Body::BackupQuery(query) => reply(self.query(query.request_id)),
            Body::BackupFetch(fetch) => reply(self.fetch(fetch)),
            _ => {}
        }
    }

    fn unavailable(request_id: u64, Unavailable(why): Unavailable) -> Body {
        answer(request_id, BackupStatus::NotAvailable, why)
    }

    fn upload(&self, upload: control::BackupUpload) -> Body {
        let id = upload.request_id;
        if upload.data.len() > PIECE_BYTES {
            return answer(
                id,
                BackupStatus::Error,
                format!("a piece is at most {PIECE_BYTES} bytes"),
            );
        }
        let result = self.with_folder(|state| {
            if upload.offset == 0 && !state.uploads.contains_key(&id) {
                let file = state
                    .folder
                    .as_ref()
                    .expect("open")
                    .new_file()
                    .map_err(|e| e.to_string())?;
                state.uploads.insert(id, Upload { file, offset: 0 });
            }
            let Some(current) = state.uploads.get_mut(&id) else {
                return Ok(Err((
                    BackupStatus::Error,
                    format!("upload {id} did not start at offset 0"),
                )));
            };
            if current.offset != upload.offset {
                let expected = current.offset;
                state.uploads.remove(&id);
                return Ok(Err((
                    BackupStatus::Error,
                    format!(
                        "upload {id} sent offset {}, where {expected} was next",
                        upload.offset
                    ),
                )));
            }
            if current.offset + upload.data.len() as u64 > MAX_FULL_BYTES.max(MAX_APK_BYTES) {
                state.uploads.remove(&id);
                return Ok(Err((
                    BackupStatus::QuotaExceeded,
                    "the app's backup is larger than the host keeps".to_string(),
                )));
            }
            if let Err(e) = current.file.write_all(&upload.data) {
                state.uploads.remove(&id);
                return Ok(Err((
                    BackupStatus::Error,
                    format!("the backup could not be written: {e}"),
                )));
            }
            current.offset += upload.data.len() as u64;
            Ok(Ok(()))
        });
        match result {
            Ok(Ok(())) => answer(id, BackupStatus::Ok, ""),
            Ok(Err((status, why))) => answer(id, status, why),
            Err(unavailable) => Self::unavailable(id, unavailable),
        }
    }

    fn commit(&self, commit: control::BackupCommit) -> Body {
        let id = commit.request_id;
        let kind = BackupDataKind::try_from(commit.kind).unwrap_or(BackupDataKind::Unspecified);
        if !valid_package(&commit.package) {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .uploads
                .remove(&id);
            return answer(id, BackupStatus::Error, "that is not a package name");
        }
        let limit = match kind {
            BackupDataKind::KeyValue => MAX_KEY_VALUE_BYTES,
            BackupDataKind::Full => MAX_FULL_BYTES,
            BackupDataKind::Apk => MAX_APK_BYTES,
            BackupDataKind::Unspecified => {
                return answer(id, BackupStatus::Error, "the kind of data is not known")
            }
        };
        if kind == BackupDataKind::Apk
            && (!valid_apk_name(&commit.apk_name)
                || !commit.apk_names.iter().all(|n| valid_apk_name(n))
                || !commit.apk_names.contains(&commit.apk_name))
        {
            return answer(id, BackupStatus::Error, "that is not an APK's name");
        }
        if commit.size > limit {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .uploads
                .remove(&id);
            return answer(
                id,
                BackupStatus::QuotaExceeded,
                format!(
                    "{} bytes is more than the host keeps for one app's data of this kind ({limit})",
                    commit.size
                ),
            );
        }
        let result = self.with_folder(|state| {
            let upload = state.uploads.remove(&id);
            let folder = state.folder.as_mut().expect("open");
            let file = match upload {
                Some(upload) => upload.file,
                None if commit.size == 0 => folder.new_file().map_err(|e| e.to_string())?,
                None => return Ok(Err(format!("upload {id} sent nothing"))),
            };
            if file.written() != commit.size {
                return Ok(Err(format!(
                    "upload {id} holds {} bytes, not the {} it said",
                    file.written(),
                    commit.size
                )));
            }
            let kept = file.keep(folder).map_err(|e| e.to_string())?;
            let entry = folder
                .index
                .packages
                .get(&commit.package)
                .cloned()
                .unwrap_or_default();
            let mut entry = entry;
            let mut removed: Vec<FileEntry> = Vec::new();
            match kind {
                BackupDataKind::KeyValue => {
                    let data = folder.read_file(&kept).map_err(|e| e.to_string())?;
                    folder.remove_file(&kept);
                    let records = match records::parse(&data) {
                        Ok(records) => records,
                        Err(why) => return Ok(Err(why)),
                    };
                    let mut map = BTreeMap::new();
                    if commit.incremental {
                        if let Some(old) = &entry.key_value {
                            let old_data = folder.read_file(old).map_err(|e| e.to_string())?;
                            for (key, value) in records::parse(&old_data)? {
                                if let Some(value) = value {
                                    map.insert(key, value);
                                }
                            }
                        }
                    }
                    records::apply(&mut map, records);
                    let merged = folder
                        .write_file(&records::encode(&map))
                        .map_err(|e| e.to_string())?;
                    if let Some(old) = entry.key_value.replace(merged) {
                        removed.push(old);
                    }
                }
                BackupDataKind::Full => {
                    if let Some(old) = entry.full.replace(kept) {
                        removed.push(old);
                    }
                }
                BackupDataKind::Apk => {
                    if entry.apk_version != commit.version_code {
                        removed.extend(std::mem::take(&mut entry.apks).into_values());
                        entry.apk_version = commit.version_code;
                    }
                    let gone: Vec<String> = entry
                        .apks
                        .keys()
                        .filter(|name| !commit.apk_names.contains(name))
                        .cloned()
                        .collect();
                    for name in gone {
                        removed.extend(entry.apks.remove(&name));
                    }
                    if let Some(old) = entry.apks.insert(commit.apk_name.clone(), kept) {
                        removed.push(old);
                    }
                }
                BackupDataKind::Unspecified => unreachable!("refused above"),
            }
            if kind != BackupDataKind::Apk {
                entry.version_code = commit.version_code;
                entry.backed_up_ms = now_ms();
                folder.index.last_backup_ms = entry.backed_up_ms;
            }
            let apks_wanted = kind != BackupDataKind::Apk
                && commit.package != PACKAGE_MANAGER
                && (entry.apk_version != commit.version_code || entry.apks.is_empty());
            folder.index.packages.insert(commit.package.clone(), entry);
            folder.save_index().map_err(|e| e.to_string())?;
            for old in removed {
                folder.remove_file(&old);
            }
            Ok(Ok(apks_wanted))
        });
        match result {
            Ok(Ok(apks_wanted)) => Body::BackupAnswer(control::BackupAnswer {
                request_id: id,
                status: BackupStatus::Ok as i32,
                message: String::new(),
                apks_wanted,
            }),
            Ok(Err(why)) => answer(id, BackupStatus::Error, why),
            Err(unavailable) => Self::unavailable(id, unavailable),
        }
    }

    fn clear(&self, clear: control::BackupClear) -> Body {
        let id = clear.request_id;
        let result = self.with_folder(|state| {
            let folder = state.folder.as_mut().expect("open");
            if let Some(entry) = folder.index.packages.remove(&clear.package) {
                folder.save_index().map_err(|e| e.to_string())?;
                for file in entry.files() {
                    folder.remove_file(file);
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => answer(id, BackupStatus::Ok, ""),
            Err(unavailable) => Self::unavailable(id, unavailable),
        }
    }

    fn query(&self, request_id: u64) -> Body {
        match self.with_folder(|state| {
            let folder = state.folder.as_ref().expect("open");
            let packages = folder
                .index
                .packages
                .iter()
                .map(|(package, entry)| control::BackupPackage {
                    package: package.clone(),
                    key_value: entry.key_value.is_some(),
                    full: entry.full.is_some(),
                    version_code: entry.version_code,
                    apk_names: if entry.apk_version == entry.version_code {
                        entry.apks.keys().cloned().collect()
                    } else {
                        Vec::new()
                    },
                    backed_up_unix_ms: entry.backed_up_ms,
                    size: entry.size(),
                })
                .collect();
            let name = if folder.index.made_on.is_empty() {
                "DSA backup".to_string()
            } else {
                format!("DSA backup of {}", folder.index.made_on)
            };
            Ok((folder.metadata.token, name, packages))
        }) {
            Ok((token, name, packages)) => Body::BackupContents(control::BackupContents {
                request_id,
                status: BackupStatus::Ok as i32,
                message: String::new(),
                token,
                name,
                packages,
            }),
            Err(Unavailable(why)) => Body::BackupContents(control::BackupContents {
                request_id,
                status: BackupStatus::NotAvailable as i32,
                message: why,
                ..Default::default()
            }),
        }
    }

    fn fetch(&self, fetch: control::BackupFetch) -> Body {
        let id = fetch.request_id;
        let refuse = |status: BackupStatus, why: String| {
            Body::BackupFetchData(control::BackupFetchData {
                request_id: id,
                status: status as i32,
                message: why,
                ..Default::default()
            })
        };
        let result = self.with_folder(|state| {
            let wanted = (fetch.package.clone(), fetch.kind, fetch.apk_name.clone());
            let token = state.folder.as_ref().expect("open").metadata.token;
            if fetch.token != token {
                state.fetches.remove(&id);
                return Ok(Err((
                    BackupStatus::NotFound,
                    "that restore set is not this computer's backup".to_string(),
                )));
            }
            let reuse = state
                .fetches
                .get(&id)
                .is_some_and(|f| f.key == wanted && f.offset == fetch.offset);
            if !reuse {
                state.fetches.remove(&id);
                let folder = state.folder.as_ref().expect("open");
                let Some(entry) = folder.index.packages.get(&fetch.package) else {
                    return Ok(Err((
                        BackupStatus::NotFound,
                        format!("the backup holds nothing of {}", fetch.package),
                    )));
                };
                let file = match BackupDataKind::try_from(fetch.kind) {
                    Ok(BackupDataKind::KeyValue) => entry.key_value.clone(),
                    Ok(BackupDataKind::Full) => entry.full.clone(),
                    Ok(BackupDataKind::Apk) => entry.apks.get(&fetch.apk_name).cloned(),
                    _ => None,
                };
                let Some(file) = file else {
                    return Ok(Err((
                        BackupStatus::NotFound,
                        format!("the backup holds no such data of {}", fetch.package),
                    )));
                };
                let mut reader = folder.open_file(&file).map_err(|e| e.to_string())?;
                // A read that starts again part way skips what came before.
                std::io::copy(&mut (&mut reader).take(fetch.offset), &mut std::io::sink())
                    .map_err(|e| format!("the backup's data cannot be read: {e}"))?;
                state.fetches.insert(
                    id,
                    Fetch {
                        reader,
                        key: wanted,
                        offset: fetch.offset,
                        size: file.size,
                    },
                );
            }
            let current = state.fetches.get_mut(&id).expect("just made");
            let mut data = Vec::with_capacity(PIECE_BYTES);
            if let Err(e) = (&mut current.reader)
                .take(PIECE_BYTES as u64)
                .read_to_end(&mut data)
            {
                state.fetches.remove(&id);
                return Ok(Err((
                    BackupStatus::Error,
                    format!("the backup's data cannot be read: {e}"),
                )));
            }
            let offset = current.offset;
            current.offset += data.len() as u64;
            let size = current.size;
            let last = current.offset >= size;
            if last {
                state.fetches.remove(&id);
            }
            Ok(Ok((size, offset, data, last)))
        });
        match result {
            Ok(Ok((size, offset, data, last))) => Body::BackupFetchData(control::BackupFetchData {
                request_id: id,
                status: BackupStatus::Ok as i32,
                message: String::new(),
                size,
                offset,
                data,
                last,
            }),
            Ok(Err((status, why))) => refuse(status, why),
            Err(Unavailable(why)) => refuse(BackupStatus::NotAvailable, why),
        }
    }
}

/// The answer that tells the guest's transport the host keeps no backup, with `why`, for a
/// request that is answered; `None` for one that is not (a discard).
pub fn refusal(body: &Body, why: &str) -> Option<Body> {
    let request_id = match body {
        Body::BackupUpload(control::BackupUpload { request_id, .. })
        | Body::BackupCommit(control::BackupCommit { request_id, .. })
        | Body::BackupClear(control::BackupClear { request_id, .. }) => *request_id,
        Body::BackupQuery(query) => {
            return Some(Body::BackupContents(control::BackupContents {
                request_id: query.request_id,
                status: BackupStatus::NotAvailable as i32,
                message: why.to_string(),
                ..Default::default()
            }))
        }
        Body::BackupFetch(fetch) => {
            return Some(Body::BackupFetchData(control::BackupFetchData {
                request_id: fetch.request_id,
                status: BackupStatus::NotAvailable as i32,
                message: why.to_string(),
                ..Default::default()
            }))
        }
        _ => return None,
    };
    Some(answer(request_id, BackupStatus::NotAvailable, why))
}

/// What a person reads when the backup key cannot be had.
fn key_problem(error: &KeyError) -> String {
    match error {
        KeyError::Unavailable(why) => format!("backups are off: {why}"),
        KeyError::Unreadable(why) => format!(
            "the backup key cannot be opened here ({why}); enter the backup's recovery key \
             (dsa backup key import)"
        ),
        KeyError::Failed(why) => format!("the backup key cannot be had: {why}"),
    }
}

/// The backup folder of a user whose home (profile) folder is `home`.
pub fn folder_in(home: &Path) -> PathBuf {
    home.join(FOLDER_NAME)
}

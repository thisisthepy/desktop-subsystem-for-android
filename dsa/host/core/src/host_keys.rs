//! The secrets the host keeps for each user: the key their backup folder is encrypted with,
//! and the root key of their Android's KeyMint.
//!
//! Each secret is 32 random bytes, made the first time it is needed and kept by a
//! [`HostKeyStore`] the platform supplies. None of them is ever written to the guest's disk, so
//! a copy of that disk alone holds no key.
//!
//! The KeyMint root key is sealed by the computer's own hardware wherever it has some that
//! works: the TPM on Windows and Linux, the Secure Enclave on a Mac. Only where there is none
//! is it kept by the user's credential store alone (DPAPI on Windows, the Secret Service on
//! Linux, the login keychain on macOS). The backup key is always kept by the credential store:
//! a backup is restored on another computer with its recovery key, so nothing about it is
//! bound to this computer's hardware.
//!
//! [`SealedKeys`] is the part every platform shares. Each secret lives in a file of the user's
//! DSA folder, `<purpose>.sealed`, which names the [`Sealer`] that made it, and it is opened with
//! that sealer whichever one a new secret would get now. So:
//!
//! - a key sealed by the credential store because the computer had no usable hardware is
//!   sealed again by the hardware once it works ([`SealedKeys::load`] does it, keeping the
//!   same secret), so nothing wrapped under the key is lost on the way;
//! - a key sealed by hardware that stopped working (a TPM turned off, locked out or cleared)
//!   is refused with that reason, and never replaced: a new KeyMint root key would leave every
//!   key Android wrapped under the old one unreadable for good. Only a data reset, which
//!   leaves Android nothing wrapped under the old key, removes it ([`HostKeyStore::remove`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long each secret is, in bytes.
pub const SECRET_LEN: usize = 32;

/// What a secret is for. Each purpose has a secret of its own, so a leak of one says nothing of
/// another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyPurpose {
    /// The key the user's backup folder is encrypted with.
    Backup,
    /// The root key the guest's KeyMint wraps every key it makes with.
    KeyMintRoot,
}

impl KeyPurpose {
    /// The secret's name in the credential store and in file names: stable, ASCII.
    pub fn name(self) -> &'static str {
        match self {
            KeyPurpose::Backup => "backup-key",
            KeyPurpose::KeyMintRoot => "keymint-root-key",
        }
    }

    /// What a person reads when the credential store asks about the secret.
    pub fn description(self) -> &'static str {
        match self {
            KeyPurpose::Backup => "Desktop Subsystem for Android backup key",
            KeyPurpose::KeyMintRoot => "Desktop Subsystem for Android KeyMint root key",
        }
    }

    /// Whether the computer's hardware seals this secret where it can. Only the KeyMint root
    /// key: the backup key has to open on another computer with its recovery key.
    pub fn sealed_by_hardware(self) -> bool {
        self == KeyPurpose::KeyMintRoot
    }

    /// The purpose whose [`KeyPurpose::name`] is `name`.
    pub fn named(name: &str) -> Option<KeyPurpose> {
        [KeyPurpose::Backup, KeyPurpose::KeyMintRoot]
            .into_iter()
            .find(|purpose| purpose.name() == name)
    }
}

/// 32 secret bytes, wiped from memory when dropped. `Debug` never shows them.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; SECRET_LEN]);

impl Secret {
    pub fn from_bytes(bytes: [u8; SECRET_LEN]) -> Secret {
        Secret(bytes)
    }

    /// A secret from a slice of exactly [`SECRET_LEN`] bytes.
    pub fn from_slice(bytes: &[u8]) -> Option<Secret> {
        <[u8; SECRET_LEN]>::try_from(bytes).ok().map(Secret)
    }

    /// A fresh secret from the operating system's random source.
    pub fn generate() -> Result<Secret, String> {
        let mut bytes = [0u8; SECRET_LEN];
        getrandom::getrandom(&mut bytes)
            .map_err(|e| format!("the system's random source failed: {e}"))?;
        Ok(Secret(bytes))
    }

    pub fn bytes(&self) -> &[u8; SECRET_LEN] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // A volatile write, so the wipe is not optimised away.
            // SAFETY: a valid, aligned pointer to a byte this value owns.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Why a secret could not be had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// The host has no credential store to keep it in, or the user turned it off, or the
    /// hardware that sealed it cannot be used just now (a TPM that is locked out). The text
    /// says which, and what to do.
    Unavailable(String),
    /// The store has the secret but cannot open it here: it was sealed for another user or
    /// another computer, or the hardware that sealed it was cleared. The text says which.
    Unreadable(String),
    /// The store failed; the text says how.
    Failed(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Unavailable(why) | KeyError::Unreadable(why) | KeyError::Failed(why) => {
                f.write_str(why)
            }
        }
    }
}

/// How a kept secret is protected on this computer, as the settings app and `dsa status` show
/// it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyProtection {
    /// What sealed it: `tpm`, `secure-enclave`, `dpapi`, `secret-service` or `keychain`.
    pub sealed_by: String,
    /// Whether hardware of this computer holds what seals it.
    pub hardware: bool,
    /// What sealed it, in words.
    pub description: String,
    /// When it is not sealed by hardware but its purpose would be: why the hardware could not
    /// be used.
    pub fallback_reason: String,
}

/// Keeps the secrets of one user in the host's credential store.
pub trait HostKeyStore: Send + Sync {
    /// The secret kept for `purpose`; `Ok(None)` when none is kept yet.
    fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError>;
    /// Keeps `secret` for `purpose`, in place of what was kept.
    fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError>;
    /// Removes the secret kept for `purpose`, if any: the backup key when the user deletes
    /// their backups, the KeyMint root key when their Android's data is reset.
    fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
        Err(KeyError::Failed(format!(
            "this computer's credential store cannot remove the {}",
            purpose.description()
        )))
    }
    /// How the secret kept for `purpose` is protected; `Ok(None)` when none is kept, or the
    /// store cannot tell without opening it. It never opens the secret, so it never prompts.
    fn protection(&self, purpose: KeyPurpose) -> Result<Option<KeyProtection>, KeyError> {
        let _ = purpose;
        Ok(None)
    }
}

/// The secret kept for `purpose`, made and kept first when there is none yet. A secret that is
/// kept but cannot be opened is an error and is never replaced: a new one would make
/// everything the old one protects unreadable for good.
pub fn load_or_create(store: &dyn HostKeyStore, purpose: KeyPurpose) -> Result<Secret, KeyError> {
    if let Some(secret) = store.load(purpose)? {
        return Ok(secret);
    }
    let secret = Secret::generate().map_err(KeyError::Failed)?;
    store.store(purpose, &secret)?;
    // Read back, so a store that keeps what it cannot give back fails now, not later.
    match store.load(purpose)? {
        Some(kept) if kept == secret => Ok(secret),
        _ => Err(KeyError::Failed(format!(
            "the credential store did not keep the {}",
            purpose.description()
        ))),
    }
}

/// Seals bytes so that only this host's user can open them again: DPAPI under the user's
/// account, the computer's TPM, the Mac's Secure Enclave. `label` names what is sealed and is
/// bound into the seal where the sealer can, so a sealed secret of one purpose does not open as
/// another.
pub trait Sealer: Send + Sync {
    /// The sealer's name, which the sealed file records: `dpapi`, `tpm+dpapi`, `tpm2`. A file
    /// is opened with the sealer it names.
    fn name(&self) -> &str;
    fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError>;
    fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError>;
    /// What sealed it, for [`KeyProtection::sealed_by`]: `tpm`, `secure-enclave`, `dpapi`,
    /// `secret-service`, `keychain`.
    fn kind(&self) -> &str {
        self.name()
    }
    /// Whether hardware of this computer holds what seals it.
    fn hardware(&self) -> bool {
        false
    }
    /// What seals, in words, such as "the computer's TPM 2.0".
    fn description(&self) -> String {
        self.name().to_string()
    }
    /// Lets go of what the sealer kept elsewhere for `label` (a keyring item that holds the
    /// secret itself), once the secret is sealed by another or removed. Most sealers keep
    /// everything in what they return, and have nothing to let go of.
    fn forget(&self, label: &str) -> Result<(), KeyError> {
        let _ = label;
        Ok(())
    }
}

/// What a sealed file starts with, before the sealer's name.
pub const SEALED_MAGIC: &[u8] = b"DSA sealed key 1\n";

/// `<sealer name>\n<sealed bytes>` after the magic.
pub fn parse_sealed(data: &[u8]) -> Option<(&str, &[u8])> {
    let rest = data.strip_prefix(SEALED_MAGIC)?;
    let end = rest.iter().position(|&b| b == b'\n')?;
    let name = std::str::from_utf8(&rest[..end]).ok()?;
    Some((name, &rest[end + 1..]))
}

/// The name of the sealer a sealed file records, if `data` is one.
pub fn sealer_of(data: &[u8]) -> Option<&str> {
    parse_sealed(data).map(|(name, _)| name)
}

/// A sealed file's bytes: the magic, the sealer's name and what it sealed.
pub fn sealed_file(sealer: &str, sealed: &[u8]) -> Vec<u8> {
    let mut data = SEALED_MAGIC.to_vec();
    data.extend_from_slice(sealer.as_bytes());
    data.push(b'\n');
    data.extend_from_slice(sealed);
    data
}

/// The file the secret for `purpose` is sealed in, in `dir`.
pub fn sealed_path(dir: &Path, purpose: KeyPurpose) -> PathBuf {
    dir.join(format!("{}.sealed", purpose.name()))
}

/// Reads the sealed file of `purpose` in `dir`; `None` when there is none.
fn read_sealed(dir: &Path, purpose: KeyPurpose) -> Result<Option<Vec<u8>>, KeyError> {
    let path = sealed_path(dir, purpose);
    match std::fs::read(&path) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(KeyError::Failed(format!(
            "the {} cannot be read from {}: {e}",
            purpose.description(),
            path.display()
        ))),
    }
}

fn not_sealed_here(path: &Path) -> KeyError {
    KeyError::Unreadable(format!("{} is not a sealed key DSA made", path.display()))
}

/// The secret in `sealed`, which `sealer` sealed for `purpose`.
fn open_with(
    sealer: &dyn Sealer,
    sealed: &[u8],
    purpose: KeyPurpose,
    path: &Path,
) -> Result<Secret, KeyError> {
    let plain = sealer.unseal(sealed, purpose.name())?;
    Secret::from_slice(&plain).ok_or_else(|| {
        KeyError::Unreadable(format!(
            "the {} in {} is {} bytes long, not {SECRET_LEN}",
            purpose.description(),
            path.display(),
            plain.len()
        ))
    })
}

/// A [`HostKeyStore`] that keeps each secret sealed by one [`Sealer`] in a file of `dir`,
/// `<purpose>.sealed`. The file names the sealer, so a file sealed by another one (a TPM that
/// was cleared, a computer without one) is refused with that reason rather than read wrongly.
pub struct SealedFileStore {
    dir: PathBuf,
    sealer: Arc<dyn Sealer>,
}

impl SealedFileStore {
    pub fn new(dir: impl Into<PathBuf>, sealer: Arc<dyn Sealer>) -> SealedFileStore {
        SealedFileStore {
            dir: dir.into(),
            sealer,
        }
    }

    /// The file the secret for `purpose` is sealed in.
    pub fn path(&self, purpose: KeyPurpose) -> PathBuf {
        sealed_path(&self.dir, purpose)
    }
}

impl HostKeyStore for SealedFileStore {
    fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
        let path = self.path(purpose);
        let Some(data) = read_sealed(&self.dir, purpose)? else {
            return Ok(None);
        };
        let Some((name, sealed)) = parse_sealed(&data) else {
            return Err(not_sealed_here(&path));
        };
        if name != self.sealer.name() {
            return Err(KeyError::Unreadable(format!(
                "the {} in {} was sealed by {name}, which this computer does not seal with \
                 ({}); it opens only where it was made",
                purpose.description(),
                path.display(),
                self.sealer.name()
            )));
        }
        open_with(self.sealer.as_ref(), sealed, purpose, &path).map(Some)
    }

    fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
        let sealed = self.sealer.seal(secret.bytes(), purpose.name())?;
        write_replacing(
            &self.path(purpose),
            &sealed_file(self.sealer.name(), &sealed),
        )
        .map_err(|e| {
            KeyError::Failed(format!(
                "the {} cannot be kept in {}: {e}",
                purpose.description(),
                self.dir.display()
            ))
        })
    }

    fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
        remove_sealed(&self.dir, purpose, |_| Ok(Arc::clone(&self.sealer)))
    }

    fn protection(&self, purpose: KeyPurpose) -> Result<Option<KeyProtection>, KeyError> {
        Ok(read_sealed(&self.dir, purpose)?.map(|_| KeyProtection {
            sealed_by: self.sealer.kind().to_string(),
            hardware: self.sealer.hardware(),
            description: self.sealer.description(),
            fallback_reason: String::new(),
        }))
    }
}

/// Removes the sealed file of `purpose` in `dir`, after `sealer_named` has let go of what its
/// sealer kept elsewhere.
fn remove_sealed(
    dir: &Path,
    purpose: KeyPurpose,
    sealer_named: impl Fn(&str) -> Result<Arc<dyn Sealer>, KeyError>,
) -> Result<(), KeyError> {
    let path = sealed_path(dir, purpose);
    let Some(data) = read_sealed(dir, purpose)? else {
        return Ok(());
    };
    if let Some(name) = sealer_of(&data) {
        if let Ok(sealer) = sealer_named(name) {
            sealer.forget(purpose.name())?;
        }
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(KeyError::Failed(format!(
            "the {} in {} cannot be removed: {e}",
            purpose.description(),
            path.display()
        ))),
    }
}

/// Writes `data` to `path` through a file beside it, so a crash leaves the old file or the new
/// one, never half of either.
pub fn write_replacing(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let temporary = path.with_file_name(name);
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

/// A [`HostKeyStore`] for a host that has no credential store: every secret is unavailable,
/// with `why`.
pub struct NoKeyStore(pub String);

impl HostKeyStore for NoKeyStore {
    fn load(&self, _: KeyPurpose) -> Result<Option<Secret>, KeyError> {
        Err(KeyError::Unavailable(self.0.clone()))
    }
    fn store(&self, _: KeyPurpose, _: &Secret) -> Result<(), KeyError> {
        Err(KeyError::Unavailable(self.0.clone()))
    }
    fn remove(&self, _: KeyPurpose) -> Result<(), KeyError> {
        Ok(())
    }
}

/// Whether the computer's hardware can seal now.
#[derive(Clone)]
pub enum Hardware {
    /// It can, with this sealer.
    Usable(Arc<dyn Sealer>),
    /// It cannot; the text says why (no TPM, a TPM turned off, locked out or too old, a Mac
    /// without a Secure Enclave, a copy of DSA not signed to use it).
    Unusable(String),
}

/// The sealers a computer has for one user.
pub trait SealerSource: Send + Sync {
    /// The computer's hardware sealer, if it works now. It may try the hardware (seal and open
    /// a test secret), so it can take a moment.
    fn hardware(&self, purpose: KeyPurpose) -> Hardware;
    /// The sealer of the user's credential store, for what hardware does not seal.
    fn fallback(&self, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError>;
    /// The sealer named `name`, to open a secret it sealed; an error says why this computer
    /// cannot open it.
    fn named(&self, name: &str, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError>;
}

/// A [`Sealer`] that keeps the secret itself in a credential store that keeps secrets rather
/// than seals bytes (the Secret Service, a keychain): what it returns is empty, and the sealed
/// file only records that the store holds the secret. [`Sealer::forget`] removes it from the
/// store once the secret is sealed by the hardware.
pub struct StoredSealer {
    name: &'static str,
    description: String,
    store: Arc<dyn HostKeyStore>,
}

impl StoredSealer {
    /// `name` is both the sealer's name and its kind (`secret-service`, `keychain`).
    pub fn new(
        name: &'static str,
        description: &str,
        store: Arc<dyn HostKeyStore>,
    ) -> StoredSealer {
        StoredSealer {
            name,
            description: description.to_string(),
            store,
        }
    }

    fn purpose(label: &str) -> Result<KeyPurpose, KeyError> {
        KeyPurpose::named(label)
            .ok_or_else(|| KeyError::Failed(format!("{label} is not a key DSA keeps")))
    }
}

impl Sealer for StoredSealer {
    fn name(&self) -> &str {
        self.name
    }

    fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        let purpose = Self::purpose(label)?;
        let secret = Secret::from_slice(plain).ok_or_else(|| {
            KeyError::Failed(format!(
                "the {} is not {SECRET_LEN} bytes",
                purpose.description()
            ))
        })?;
        self.store.store(purpose, &secret)?;
        Ok(Vec::new())
    }

    fn unseal(&self, _: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        let purpose = Self::purpose(label)?;
        match self.store.load(purpose)? {
            Some(secret) => Ok(secret.bytes().to_vec()),
            None => Err(KeyError::Unreadable(format!(
                "{} no longer holds the {}",
                self.description,
                purpose.description()
            ))),
        }
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn forget(&self, label: &str) -> Result<(), KeyError> {
        self.store.remove(Self::purpose(label)?)
    }
}

/// Whether hardware can seal now: `Ok`, or why not.
pub type HardwareCheck = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The sealers of a computer with one hardware sealer and one credential store: the hardware
/// whenever `usable` says it works, the store otherwise. Each opens what it sealed.
pub struct HardwareOrStore {
    hardware: Arc<dyn Sealer>,
    usable: HardwareCheck,
    store: Arc<dyn Sealer>,
}

impl HardwareOrStore {
    pub fn new(
        hardware: Arc<dyn Sealer>,
        usable: HardwareCheck,
        store: Arc<dyn Sealer>,
    ) -> HardwareOrStore {
        HardwareOrStore {
            hardware,
            usable,
            store,
        }
    }
}

impl SealerSource for HardwareOrStore {
    fn hardware(&self, _: KeyPurpose) -> Hardware {
        match (self.usable)() {
            Ok(()) => Hardware::Usable(Arc::clone(&self.hardware)),
            Err(why) => Hardware::Unusable(why),
        }
    }

    fn fallback(&self, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
        Ok(Arc::clone(&self.store))
    }

    fn named(&self, name: &str, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
        if name == self.hardware.name() {
            Ok(Arc::clone(&self.hardware))
        } else if name == self.store.name() {
            Ok(Arc::clone(&self.store))
        } else {
            Err(KeyError::Unreadable(format!(
                "the {} was sealed by {name}, which this computer does not seal with",
                purpose.description()
            )))
        }
    }
}

/// How long [`SealedKeys::protection`] trusts what it last found of the hardware.
const HARDWARE_CHECK_KEPT: Duration = Duration::from_secs(60);

/// A user's secrets, each in a sealed file of `dir`, opened with the sealer its file names and
/// sealed anew with the computer's hardware where the purpose is sealed by hardware and the
/// hardware works, else with the credential store's sealer.
pub struct SealedKeys {
    dir: PathBuf,
    sealers: Arc<dyn SealerSource>,
    /// What [`SealerSource::hardware`] last said for the KeyMint root key, and when.
    hardware_seen: Mutex<Option<(Instant, Hardware)>>,
}

impl SealedKeys {
    pub fn new(dir: impl Into<PathBuf>, sealers: Arc<dyn SealerSource>) -> SealedKeys {
        SealedKeys {
            dir: dir.into(),
            sealers,
            hardware_seen: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file the secret for `purpose` is sealed in.
    pub fn path(&self, purpose: KeyPurpose) -> PathBuf {
        sealed_path(&self.dir, purpose)
    }

    /// Asks the source about the hardware, and remembers the answer.
    fn hardware(&self, purpose: KeyPurpose) -> Hardware {
        let found = self.sealers.hardware(purpose);
        *self.hardware_seen.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), found.clone()));
        found
    }

    /// What the hardware was found to be lately, asking again when that was long ago.
    fn hardware_lately(&self, purpose: KeyPurpose) -> Hardware {
        let seen = self
            .hardware_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match seen {
            Some((at, found)) if at.elapsed() < HARDWARE_CHECK_KEPT => found,
            _ => self.hardware(purpose),
        }
    }

    /// Seals `secret`, which `old` sealed, with the hardware now that it works, keeping the
    /// same secret. The new file replaces the old one only once it is known to open again; any
    /// failure leaves the old one as it was.
    fn seal_again(&self, purpose: KeyPurpose, secret: &Secret, old: &dyn Sealer) {
        let Hardware::Usable(hardware) = self.hardware(purpose) else {
            return;
        };
        let label = purpose.name();
        let moved = hardware.seal(secret.bytes(), label).and_then(|sealed| {
            if hardware.unseal(&sealed, label)? != secret.bytes() {
                return Err(KeyError::Failed(
                    "it did not open again to the same key".into(),
                ));
            }
            write_replacing(&self.path(purpose), &sealed_file(hardware.name(), &sealed))
                .map_err(|e| KeyError::Failed(e.to_string()))
        });
        match moved {
            Ok(()) => {
                eprintln!(
                    "keys: the {} was kept by {}; it is sealed by {} now",
                    purpose.description(),
                    old.description(),
                    hardware.description()
                );
                if let Err(why) = old.forget(label) {
                    eprintln!(
                        "keys: the copy {} kept of the {} was not removed: {why}",
                        old.description(),
                        purpose.description()
                    );
                }
            }
            Err(why) => eprintln!(
                "keys: the {} stays with {}: {} could not seal it ({why})",
                purpose.description(),
                old.description(),
                hardware.description()
            ),
        }
    }
}

impl HostKeyStore for SealedKeys {
    fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
        let path = self.path(purpose);
        let Some(data) = read_sealed(&self.dir, purpose)? else {
            return Ok(None);
        };
        let Some((name, sealed)) = parse_sealed(&data) else {
            return Err(not_sealed_here(&path));
        };
        let sealer = self.sealers.named(name, purpose)?;
        let secret = open_with(sealer.as_ref(), sealed, purpose, &path)?;
        if purpose.sealed_by_hardware() && !sealer.hardware() {
            self.seal_again(purpose, &secret, sealer.as_ref());
        }
        Ok(Some(secret))
    }

    fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
        // The backup key is replaced when the user brings a recovery key; the KeyMint root key
        // never is, since Android's keys are wrapped under the one kept.
        if purpose == KeyPurpose::KeyMintRoot && self.path(purpose).exists() {
            return Err(KeyError::Failed(format!(
                "a {} is kept in {} already, and it is never replaced",
                purpose.description(),
                self.dir.display()
            )));
        }
        let sealer = if purpose.sealed_by_hardware() {
            match self.hardware(purpose) {
                Hardware::Usable(hardware) => hardware,
                Hardware::Unusable(why) => {
                    let fallback = self.sealers.fallback(purpose)?;
                    eprintln!(
                        "keys: the {} is kept by {}: {why}",
                        purpose.description(),
                        fallback.description()
                    );
                    fallback
                }
            }
        } else {
            self.sealers.fallback(purpose)?
        };
        let sealed = sealer.seal(secret.bytes(), purpose.name())?;
        write_replacing(&self.path(purpose), &sealed_file(sealer.name(), &sealed)).map_err(|e| {
            KeyError::Failed(format!(
                "the {} cannot be kept in {}: {e}",
                purpose.description(),
                self.dir.display()
            ))
        })
    }

    fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
        remove_sealed(&self.dir, purpose, |name| self.sealers.named(name, purpose))
    }

    fn protection(&self, purpose: KeyPurpose) -> Result<Option<KeyProtection>, KeyError> {
        let path = self.path(purpose);
        let Some(data) = read_sealed(&self.dir, purpose)? else {
            return Ok(None);
        };
        let Some(name) = sealer_of(&data) else {
            return Err(not_sealed_here(&path));
        };
        let sealer = match self.sealers.named(name, purpose) {
            Ok(sealer) => sealer,
            Err(why) => {
                return Ok(Some(KeyProtection {
                    sealed_by: name.to_string(),
                    hardware: false,
                    description: name.to_string(),
                    fallback_reason: why.to_string(),
                }))
            }
        };
        let fallback_reason = if sealer.hardware() || !purpose.sealed_by_hardware() {
            String::new()
        } else {
            match self.hardware_lately(purpose) {
                Hardware::Unusable(why) => why,
                Hardware::Usable(hardware) => format!(
                    "{} can seal it now; it is sealed there the next time Android starts",
                    hardware.description()
                ),
            }
        };
        Ok(Some(KeyProtection {
            sealed_by: sealer.kind().to_string(),
            hardware: sealer.hardware(),
            description: sealer.description(),
            fallback_reason,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A sealer that records its name in what it seals, and opens only its own.
    struct NamedSealer {
        name: &'static str,
        hardware: bool,
        forgotten: Arc<Mutex<Vec<String>>>,
    }

    impl Sealer for NamedSealer {
        fn name(&self) -> &str {
            self.name
        }
        fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let mut out = format!("{}:{label}:", self.name).into_bytes();
            out.extend(plain.iter().map(|b| b ^ 0x5a));
            Ok(out)
        }
        fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let head = format!("{}:{label}:", self.name).into_bytes();
            sealed
                .strip_prefix(head.as_slice())
                .map(|rest| rest.iter().map(|b| b ^ 0x5a).collect())
                .ok_or_else(|| KeyError::Unreadable("not sealed here".into()))
        }
        fn kind(&self) -> &str {
            if self.hardware {
                "tpm"
            } else {
                "dpapi"
            }
        }
        fn hardware(&self) -> bool {
            self.hardware
        }
        fn description(&self) -> String {
            format!("the {} sealer", self.name)
        }
        fn forget(&self, label: &str) -> Result<(), KeyError> {
            self.forgotten
                .lock()
                .unwrap()
                .push(format!("{}:{label}", self.name));
            Ok(())
        }
    }

    /// A computer whose hardware can be made to work or not.
    #[derive(Default)]
    struct FakeComputer {
        hardware: AtomicBool,
        forgotten: Arc<Mutex<Vec<String>>>,
    }

    impl FakeComputer {
        fn sealer(&self, name: &'static str) -> Arc<dyn Sealer> {
            Arc::new(NamedSealer {
                name,
                hardware: name == "tpm2",
                forgotten: Arc::clone(&self.forgotten),
            })
        }
    }

    impl SealerSource for FakeComputer {
        fn hardware(&self, _: KeyPurpose) -> Hardware {
            if self.hardware.load(Ordering::SeqCst) {
                Hardware::Usable(self.sealer("tpm2"))
            } else {
                Hardware::Unusable("this computer has no TPM".into())
            }
        }
        fn fallback(&self, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
            Ok(self.sealer("store"))
        }
        fn named(&self, name: &str, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
            match name {
                "store" => Ok(self.sealer("store")),
                "tpm2" if self.hardware.load(Ordering::SeqCst) => Ok(self.sealer("tpm2")),
                "tpm2" => Err(KeyError::Unreadable(
                    "the TPM that sealed it was cleared".into(),
                )),
                other => Err(KeyError::Unreadable(format!("unknown sealer {other}"))),
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-host-keys-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn keys(dir: &Path, hardware: bool) -> (SealedKeys, Arc<FakeComputer>) {
        let computer = Arc::new(FakeComputer::default());
        computer.hardware.store(hardware, Ordering::SeqCst);
        (SealedKeys::new(dir, computer.clone()), computer)
    }

    fn sealer_in(keys: &SealedKeys, purpose: KeyPurpose) -> String {
        let data = std::fs::read(keys.path(purpose)).unwrap();
        sealer_of(&data).unwrap().to_string()
    }

    #[test]
    fn fr36_the_keymint_root_key_is_sealed_by_hardware_and_the_backup_key_never() {
        let dir = scratch("choice");
        let (keys, _) = keys(&dir, true);
        load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        load_or_create(&keys, KeyPurpose::Backup).unwrap();
        assert_eq!(sealer_in(&keys, KeyPurpose::KeyMintRoot), "tpm2");
        assert_eq!(sealer_in(&keys, KeyPurpose::Backup), "store");
        let protection = keys.protection(KeyPurpose::KeyMintRoot).unwrap().unwrap();
        assert!(protection.hardware);
        assert_eq!(protection.sealed_by, "tpm");
        assert!(protection.fallback_reason.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_without_usable_hardware_the_credential_store_seals_and_says_why() {
        let dir = scratch("fallback");
        let (keys, _) = keys(&dir, false);
        assert_eq!(keys.protection(KeyPurpose::KeyMintRoot).unwrap(), None);
        load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        assert_eq!(sealer_in(&keys, KeyPurpose::KeyMintRoot), "store");
        let protection = keys.protection(KeyPurpose::KeyMintRoot).unwrap().unwrap();
        assert!(!protection.hardware);
        assert_eq!(protection.sealed_by, "dpapi");
        assert_eq!(protection.fallback_reason, "this computer has no TPM");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_a_key_kept_without_hardware_is_sealed_by_it_once_it_works() {
        let dir = scratch("reseal");
        let (keys, computer) = keys(&dir, false);
        let first = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        computer.hardware.store(true, Ordering::SeqCst);
        let again = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        assert_eq!(
            first, again,
            "the same key, so nothing wrapped under it is lost"
        );
        assert_eq!(sealer_in(&keys, KeyPurpose::KeyMintRoot), "tpm2");
        assert_eq!(
            *computer.forgotten.lock().unwrap(),
            vec!["store:keymint-root-key".to_string()],
            "the credential store's copy is let go of"
        );
        assert_eq!(keys.load(KeyPurpose::KeyMintRoot).unwrap(), Some(first));
        // The backup key stays with the credential store.
        load_or_create(&keys, KeyPurpose::Backup).unwrap();
        load_or_create(&keys, KeyPurpose::Backup).unwrap();
        assert_eq!(sealer_in(&keys, KeyPurpose::Backup), "store");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_a_key_sealed_by_hardware_that_is_gone_is_refused_and_never_replaced() {
        let dir = scratch("gone");
        let (keys, computer) = keys(&dir, true);
        load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        let before = std::fs::read(keys.path(KeyPurpose::KeyMintRoot)).unwrap();
        computer.hardware.store(false, Ordering::SeqCst);
        let error = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap_err();
        assert!(matches!(error, KeyError::Unreadable(_)), "{error:?}");
        assert!(keys
            .store(KeyPurpose::KeyMintRoot, &Secret::from_bytes([1; 32]))
            .is_err());
        assert_eq!(
            std::fs::read(keys.path(KeyPurpose::KeyMintRoot)).unwrap(),
            before
        );
        // A data reset lets it go, and the next start makes a new one.
        keys.remove(KeyPurpose::KeyMintRoot).unwrap();
        assert!(!keys.path(KeyPurpose::KeyMintRoot).exists());
        load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_the_backup_key_is_replaced_by_a_recovery_key_and_removed_with_the_backups() {
        let dir = scratch("recovery");
        let (keys, computer) = keys(&dir, true);
        load_or_create(&keys, KeyPurpose::Backup).unwrap();
        let recovered = Secret::from_bytes([9; 32]);
        keys.store(KeyPurpose::Backup, &recovered).unwrap();
        assert_eq!(keys.load(KeyPurpose::Backup).unwrap(), Some(recovered));
        keys.remove(KeyPurpose::Backup).unwrap();
        assert_eq!(keys.load(KeyPurpose::Backup).unwrap(), None);
        assert_eq!(
            *computer.forgotten.lock().unwrap(),
            vec!["store:backup-key".to_string()]
        );
        keys.remove(KeyPurpose::Backup).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_a_file_that_is_not_a_sealed_key_is_unreadable() {
        let dir = scratch("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        let (keys, _) = keys(&dir, true);
        std::fs::write(keys.path(KeyPurpose::Backup), b"not a key").unwrap();
        assert!(matches!(
            keys.load(KeyPurpose::Backup),
            Err(KeyError::Unreadable(_))
        ));
        assert!(keys.protection(KeyPurpose::Backup).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A keyring in memory, as the Secret Service or a keychain keeps secrets.
    #[derive(Default)]
    struct Keyring(Mutex<std::collections::HashMap<&'static str, Secret>>);

    impl HostKeyStore for Keyring {
        fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
            Ok(self.0.lock().unwrap().get(purpose.name()).cloned())
        }
        fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
            self.0
                .lock()
                .unwrap()
                .insert(purpose.name(), secret.clone());
            Ok(())
        }
        fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
            self.0.lock().unwrap().remove(purpose.name());
            Ok(())
        }
    }

    #[test]
    fn fr36_a_key_a_keyring_kept_moves_to_the_hardware_and_leaves_the_keyring() {
        let dir = scratch("keyring");
        let keyring = Arc::new(Keyring::default());
        let works = Arc::new(AtomicBool::new(false));
        let hardware: Arc<dyn Sealer> = Arc::new(NamedSealer {
            name: "tpm2",
            hardware: true,
            forgotten: Arc::default(),
        });
        let source = HardwareOrStore::new(
            hardware,
            {
                let works = Arc::clone(&works);
                Box::new(move || {
                    if works.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err("no TPM".into())
                    }
                })
            },
            Arc::new(StoredSealer::new(
                "secret-service",
                "your keyring",
                keyring.clone(),
            )),
        );
        let keys = SealedKeys::new(&dir, Arc::new(source));
        let first = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        assert_eq!(sealer_in(&keys, KeyPurpose::KeyMintRoot), "secret-service");
        assert_eq!(
            keyring.load(KeyPurpose::KeyMintRoot).unwrap(),
            Some(first.clone())
        );
        let protection = keys.protection(KeyPurpose::KeyMintRoot).unwrap().unwrap();
        assert_eq!(protection.sealed_by, "secret-service");
        assert_eq!(protection.fallback_reason, "no TPM");

        works.store(true, Ordering::SeqCst);
        assert_eq!(keys.load(KeyPurpose::KeyMintRoot).unwrap(), Some(first));
        assert_eq!(sealer_in(&keys, KeyPurpose::KeyMintRoot), "tpm2");
        assert_eq!(
            keyring.load(KeyPurpose::KeyMintRoot).unwrap(),
            None,
            "the keyring keeps no second copy once the TPM seals it"
        );
        // A keyring that lost its item says so rather than giving another key.
        let lost = SealedKeys::new(
            scratch("keyring-lost"),
            Arc::new(HardwareOrStore::new(
                Arc::new(NamedSealer {
                    name: "tpm2",
                    hardware: true,
                    forgotten: Arc::default(),
                }),
                Box::new(|| Err("no TPM".into())),
                Arc::new(StoredSealer::new(
                    "secret-service",
                    "your keyring",
                    Arc::new(Keyring::default()),
                )),
            )),
        );
        std::fs::create_dir_all(lost.dir()).unwrap();
        std::fs::write(
            lost.path(KeyPurpose::KeyMintRoot),
            sealed_file("secret-service", &[]),
        )
        .unwrap();
        assert!(matches!(
            lost.load(KeyPurpose::KeyMintRoot),
            Err(KeyError::Unreadable(_))
        ));
        let _ = std::fs::remove_dir_all(lost.dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_the_sealed_file_names_its_sealer() {
        let data = sealed_file("tpm+dpapi", b"\x00\x01\n\x02");
        assert_eq!(sealer_of(&data), Some("tpm+dpapi"));
        assert_eq!(
            parse_sealed(&data),
            Some(("tpm+dpapi", &b"\x00\x01\n\x02"[..]))
        );
        assert_eq!(sealer_of(b"something else"), None);
    }
}

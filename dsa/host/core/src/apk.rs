//! Installing APKs from the host in the user's Android.
//!
//! A client opens the APK on the host (the service never opens a user's files itself) and
//! streams it to the service over the local API. The service streams it on to the guest's
//! agent in chunks over the control channel, never more than [`CHUNKS_IN_FLIGHT`] ahead of
//! what the agent says it has written, and the agent writes them into an install session of
//! the guest's package installer. Neither the service nor the agent holds the whole APK in
//! memory. The agent answers with how the install ended, as an [`InstallStatus`] with the
//! guest's own words beside it.
//!
//! An app that comes as a base APK and split APKs, with any expansion (OBB) files, goes the same
//! way as one install made of [`InstallPart`]s: their bytes follow one another on the stream,
//! the agent writes every APK into the same install session, and it copies the expansion files
//! into the app's folder under `Android/obb/` once the app is installed. What is in a bundle,
//! and which of its splits a device takes, is [`crate::apk_bundle`]'s.
//!
//! Once an app is installed, the agent's report of the apps a launcher lists carries it to the
//! host like any other installed app, and the window manager gives it a Start menu entry.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use dsa_protocol::control;
use dsa_protocol::host_api::types;

use crate::native_abi::NativeLibraries;

/// The largest APK that is taken, in bytes: 2 GiB.
///
/// Apps put at most a few hundred MiB in one APK and ship the rest as asset packs or expansion
/// files, so this takes any real APK while a file dropped by mistake is refused before it
/// fills the guest's storage.
pub const MAX_APK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The largest install of several files that is taken, in bytes: 8 GiB, all the parts
/// together.
///
/// Games put gigabytes in their expansion files, which go with the APKs; each APK still has
/// the limit of [`MAX_APK_BYTES`].
pub const MAX_INSTALL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// The most files one install takes. Real bundles hold a few dozen splits.
pub const MAX_PARTS: usize = 256;

/// The size of one chunk on the control channel: 128 KiB.
///
/// Inside the guest each chunk reaches the agent in a one-way binder call, and one-way calls
/// to a process share half of its 1 MiB binder buffer. A call that does not fit is dropped,
/// so the host keeps at most [`CHUNKS_IN_FLIGHT`] chunks (256 KiB) ahead of the agent's
/// progress reports, which leaves room for the agent's other frames.
pub const CHUNK_BYTES: usize = 128 * 1024;

/// How many chunks the host sends before the agent has said it wrote the first of them.
pub const CHUNKS_IN_FLIGHT: u64 = 2;

/// The size of one piece of an APK on the local API: 1 MiB, well below the transport's
/// largest frame, and large enough that an APK needs few frames.
pub const LOCAL_CHUNK_BYTES: usize = 1024 * 1024;

/// The longest file name taken, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// How an install ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallStatus {
    Installed,
    /// The app does not run on this Android: its ABI, its minimum version or a feature it
    /// needs is not there.
    Incompatible,
    /// The app is installed already, signed with another key.
    ConflictingSignature,
    /// The file is not an APK Android can read, or it is not signed.
    InvalidApk,
    InsufficientStorage,
    /// A newer version of the app is installed.
    Downgrade,
    /// A policy of the Android does not allow the app.
    Blocked,
    Other,
}

impl InstallStatus {
    /// Every status, in the order of the protocol.
    pub const ALL: [InstallStatus; 8] = [
        InstallStatus::Installed,
        InstallStatus::Incompatible,
        InstallStatus::ConflictingSignature,
        InstallStatus::InvalidApk,
        InstallStatus::InsufficientStorage,
        InstallStatus::Downgrade,
        InstallStatus::Blocked,
        InstallStatus::Other,
    ];

    /// A stable code for programs, such as the settings app, that say the reason in the
    /// user's language.
    pub fn code(self) -> &'static str {
        match self {
            InstallStatus::Installed => "installed",
            InstallStatus::Incompatible => "incompatible",
            InstallStatus::ConflictingSignature => "conflicting-signature",
            InstallStatus::InvalidApk => "invalid-apk",
            InstallStatus::InsufficientStorage => "insufficient-storage",
            InstallStatus::Downgrade => "downgrade",
            InstallStatus::Blocked => "blocked",
            InstallStatus::Other => "other",
        }
    }

    /// What the status means and what the user can do, as a sentence.
    pub fn describe(self) -> &'static str {
        match self {
            InstallStatus::Installed => "The app is installed",
            InstallStatus::Incompatible => {
                "The app does not run on this Android: it needs another processor type, a newer \
                 Android version or a feature this device does not have"
            }
            InstallStatus::ConflictingSignature => {
                "The app is installed already, signed with another key; uninstall it in Android \
                 first"
            }
            InstallStatus::InvalidApk => "The file is not a signed APK that Android can read",
            InstallStatus::InsufficientStorage => "Android has no room for the app",
            InstallStatus::Downgrade => {
                "A newer version of the app is installed; uninstall it in Android first"
            }
            InstallStatus::Blocked => "A policy of this Android does not allow the app",
            InstallStatus::Other => "Android did not install the app",
        }
    }

    pub fn to_control(self) -> control::InstallStatus {
        match self {
            InstallStatus::Installed => control::InstallStatus::Success,
            InstallStatus::Incompatible => control::InstallStatus::Incompatible,
            InstallStatus::ConflictingSignature => control::InstallStatus::ConflictingSignature,
            InstallStatus::InvalidApk => control::InstallStatus::InvalidApk,
            InstallStatus::InsufficientStorage => control::InstallStatus::InsufficientStorage,
            InstallStatus::Downgrade => control::InstallStatus::Downgrade,
            InstallStatus::Blocked => control::InstallStatus::Blocked,
            InstallStatus::Other => control::InstallStatus::Other,
        }
    }

    /// The status a control message carries. An unknown or missing one is `Other`.
    pub fn from_control(status: i32) -> InstallStatus {
        match control::InstallStatus::try_from(status) {
            Ok(control::InstallStatus::Success) => InstallStatus::Installed,
            Ok(control::InstallStatus::Incompatible) => InstallStatus::Incompatible,
            Ok(control::InstallStatus::ConflictingSignature) => InstallStatus::ConflictingSignature,
            Ok(control::InstallStatus::InvalidApk) => InstallStatus::InvalidApk,
            Ok(control::InstallStatus::InsufficientStorage) => InstallStatus::InsufficientStorage,
            Ok(control::InstallStatus::Downgrade) => InstallStatus::Downgrade,
            Ok(control::InstallStatus::Blocked) => InstallStatus::Blocked,
            Ok(control::InstallStatus::Other)
            | Ok(control::InstallStatus::Unspecified)
            | Err(_) => InstallStatus::Other,
        }
    }

    pub fn to_wire(self) -> types::InstallStatus {
        match self {
            InstallStatus::Installed => types::InstallStatus::Success,
            InstallStatus::Incompatible => types::InstallStatus::Incompatible,
            InstallStatus::ConflictingSignature => types::InstallStatus::ConflictingSignature,
            InstallStatus::InvalidApk => types::InstallStatus::InvalidApk,
            InstallStatus::InsufficientStorage => types::InstallStatus::InsufficientStorage,
            InstallStatus::Downgrade => types::InstallStatus::Downgrade,
            InstallStatus::Blocked => types::InstallStatus::Blocked,
            InstallStatus::Other => types::InstallStatus::Other,
        }
    }

    /// The status a local API answer carries. An unknown or missing one is `Other`.
    pub fn from_wire(status: i32) -> InstallStatus {
        match types::InstallStatus::try_from(status) {
            Ok(types::InstallStatus::Success) => InstallStatus::Installed,
            Ok(types::InstallStatus::Incompatible) => InstallStatus::Incompatible,
            Ok(types::InstallStatus::ConflictingSignature) => InstallStatus::ConflictingSignature,
            Ok(types::InstallStatus::InvalidApk) => InstallStatus::InvalidApk,
            Ok(types::InstallStatus::InsufficientStorage) => InstallStatus::InsufficientStorage,
            Ok(types::InstallStatus::Downgrade) => InstallStatus::Downgrade,
            Ok(types::InstallStatus::Blocked) => InstallStatus::Blocked,
            Ok(types::InstallStatus::Other) | Ok(types::InstallStatus::Unspecified) | Err(_) => {
                InstallStatus::Other
            }
        }
    }
}

impl fmt::Display for InstallStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// How an install ended, as Android said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub status: InstallStatus,
    /// The package installed, or that the APK is, when Android said.
    pub package_name: Option<String>,
    /// Android's own words; empty when it gave none.
    pub message: String,
}

impl InstallOutcome {
    pub fn installed(&self) -> bool {
        self.status == InstallStatus::Installed
    }

    pub fn from_control(result: &control::InstallResult) -> InstallOutcome {
        InstallOutcome {
            status: InstallStatus::from_control(result.status),
            package_name: Some(result.package_name.clone()).filter(|name| !name.is_empty()),
            message: result.message.clone(),
        }
    }

    pub fn to_wire(&self) -> types::InstallResult {
        types::InstallResult {
            status: self.status.to_wire() as i32,
            package_name: self.package_name.clone().unwrap_or_default(),
            message: self.message.clone(),
        }
    }

    pub fn from_wire(result: types::InstallResult) -> InstallOutcome {
        InstallOutcome {
            status: InstallStatus::from_wire(result.status),
            package_name: Some(result.package_name).filter(|name| !name.is_empty()),
            message: result.message,
        }
    }
}

impl fmt::Display for InstallOutcome {
    /// One sentence for the user: what happened, the package, and Android's words.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.status.describe())?;
        if let Some(package) = &self.package_name {
            write!(f, " ({package})")?;
        }
        if !self.message.is_empty() && !self.installed() {
            write!(f, ". Android said: {}", self.message)?;
        }
        Ok(())
    }
}

/// One file of an install made of several: an APK of a split app, or an expansion file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstallPart {
    /// The file's name, without any folder: `base.apk`, `split_<name>.apk`, or the expansion
    /// file's own name, such as `main.42.com.example.game.obb`.
    pub name: String,
    pub size: u64,
    /// An expansion file, which Android gets in `Android/obb/<package>/` once the app is
    /// installed.
    pub obb: bool,
}

impl InstallPart {
    pub fn to_control(&self) -> control::InstallPart {
        control::InstallPart {
            name: self.name.clone(),
            size: self.size,
            obb: self.obb,
        }
    }

    pub fn to_wire(&self) -> types::InstallPart {
        types::InstallPart {
            name: self.name.clone(),
            size: self.size,
            obb: self.obb,
        }
    }

    pub fn from_wire(part: types::InstallPart) -> InstallPart {
        InstallPart {
            name: part.name,
            size: part.size,
            obb: part.obb,
        }
    }
}

/// Refuses an install of several files that Android cannot take or that is too large, saying
/// why, before anything is sent: no APK, an APK after an expansion file, a name that is not a
/// file's or is taken twice, an empty part, an APK above [`MAX_APK_BYTES`], more than
/// [`MAX_PARTS`] parts, or more than [`MAX_INSTALL_BYTES`] in all. `name` is the bundle's.
/// Returns the size of all the parts together.
pub fn check_parts(name: &str, parts: &[InstallPart]) -> Result<u64, String> {
    check_file_name(name)?;
    if !parts.iter().any(|part| !part.obb) {
        return Err(format!("{name:?} holds no APK to install"));
    }
    if parts.len() > MAX_PARTS {
        return Err(format!(
            "{name:?} has {} files to install; at most {MAX_PARTS} can be installed together",
            parts.len()
        ));
    }
    let mut total = 0u64;
    let mut seen_obb = false;
    for (index, part) in parts.iter().enumerate() {
        check_file_name(&part.name)?;
        let ending = if part.obb { ".obb" } else { ".apk" };
        if part.name.len() <= ending.len() || !ends_with_ignore_case(&part.name, ending) {
            return Err(format!(
                "{:?} does not end in {ending}, so it is not {}",
                part.name,
                if part.obb {
                    "an expansion file"
                } else {
                    "an APK"
                }
            ));
        }
        if parts[..index]
            .iter()
            .any(|other| other.name.eq_ignore_ascii_case(&part.name))
        {
            return Err(format!("{:?} is in the install twice", part.name));
        }
        if part.size == 0 {
            return Err(format!("{:?} is empty", part.name));
        }
        if part.obb {
            seen_obb = true;
        } else {
            if seen_obb {
                return Err(format!(
                    "{:?} comes after an expansion file; the APKs go first",
                    part.name
                ));
            }
            if part.size > MAX_APK_BYTES {
                return Err(format!(
                    "{:?} is {} MiB; APKs up to {} MiB can be installed",
                    part.name,
                    part.size.div_ceil(1024 * 1024),
                    MAX_APK_BYTES / (1024 * 1024)
                ));
            }
        }
        total = total.saturating_add(part.size);
    }
    if total > MAX_INSTALL_BYTES {
        return Err(format!(
            "{name:?} is {} MiB to install; up to {} MiB can be installed at once",
            total.div_ceil(1024 * 1024),
            MAX_INSTALL_BYTES / (1024 * 1024)
        ));
    }
    Ok(total)
}

fn ends_with_ignore_case(name: &str, ending: &str) -> bool {
    let tail = &name.as_bytes()[name.len().saturating_sub(ending.len())..];
    name.len() >= ending.len() && tail.eq_ignore_ascii_case(ending.as_bytes())
}

/// A package installed in the user's Android, as its package manager describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub version_code: i64,
    pub version_name: String,
    /// The SHA-256 digest of each certificate the installed package is signed with.
    pub signer_sha256: Vec<Vec<u8>>,
}

impl InstalledPackage {
    pub fn from_control(state: &control::PackageState) -> Option<InstalledPackage> {
        state.installed.then(|| InstalledPackage {
            version_code: state.version_code,
            version_name: state.version_name.clone(),
            signer_sha256: state.signer_sha256.clone(),
        })
    }

    pub fn to_wire(installed: Option<&InstalledPackage>) -> types::PackageState {
        match installed {
            Some(package) => types::PackageState {
                installed: true,
                version_code: package.version_code,
                version_name: package.version_name.clone(),
                signer_sha256: package.signer_sha256.clone(),
            },
            None => types::PackageState::default(),
        }
    }

    pub fn from_wire(state: types::PackageState) -> Option<InstalledPackage> {
        state.installed.then_some(InstalledPackage {
            version_code: state.version_code,
            version_name: state.version_name,
            signer_sha256: state.signer_sha256,
        })
    }
}

/// Whether `name` can be an Android package name: at most 255 bytes of dot-separated parts,
/// each of ASCII letters, digits and underscores.
pub fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// What installing an APK would do, as Android would decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallKind {
    /// The app is not installed.
    New,
    /// A lower version is installed, signed with the same key.
    Update,
    /// The same version is installed, signed with the same key; Android installs it again.
    Reinstall,
    /// A higher version is installed. Android refuses to go back to a lower one.
    Downgrade,
    /// The installed app is signed with another key. Android refuses to replace it.
    SignatureMismatch,
}

impl InstallKind {
    /// What Android does with an APK of `version_code`, signed by `signers` (the current
    /// signer, then the earlier ones its proof of rotation names), when `installed` is what
    /// the user's Android has of its package.
    pub fn decide(
        version_code: i64,
        signers: &[Vec<u8>],
        installed: Option<&InstalledPackage>,
    ) -> InstallKind {
        let Some(installed) = installed else {
            return InstallKind::New;
        };
        // An update must be signed by a key the installed app is signed with now, or by one
        // its signing history moved on from. An installed app that reports no signer cannot
        // be compared, and Android has the last word on it.
        let same_signer = installed.signer_sha256.is_empty()
            || installed
                .signer_sha256
                .iter()
                .any(|installed| signers.iter().any(|signer| signer == installed));
        if !same_signer {
            InstallKind::SignatureMismatch
        } else if version_code < installed.version_code {
            InstallKind::Downgrade
        } else if version_code == installed.version_code {
            InstallKind::Reinstall
        } else {
            InstallKind::Update
        }
    }

    /// Whether Android would refuse the install.
    pub fn blocked(self) -> bool {
        matches!(
            self,
            InstallKind::Downgrade | InstallKind::SignatureMismatch
        )
    }

    /// A stable code for programs, such as the install window, that say it in the user's
    /// language.
    pub fn code(self) -> &'static str {
        match self {
            InstallKind::New => "new",
            InstallKind::Update => "update",
            InstallKind::Reinstall => "reinstall",
            InstallKind::Downgrade => "downgrade",
            InstallKind::SignatureMismatch => "signature-mismatch",
        }
    }
}

/// An APK on the host, opened to be streamed to Android.
pub struct ApkFile {
    /// The file's name, without any folder.
    pub name: String,
    pub size: u64,
    pub file: File,
    /// The native libraries the APK holds, by ABI, which the service chooses the ABI it is
    /// installed for from. Empty when it has none, or when the file cannot be read as an APK,
    /// which Android then says.
    pub native_libraries: NativeLibraries,
}

impl ApkFile {
    /// Opens the APK at `path`, refusing it before anything is sent when it is not an APK
    /// file or is too large.
    pub fn open(path: &Path) -> Result<ApkFile, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} names no file", path.display()))?;
        ApkFile::open_as(path, name)
    }

    /// Opens the APK at `path` as [`open`](Self::open) does, under the name `name`, for a
    /// file whose content says it is an APK while its own name does not end in `.apk`.
    pub fn open_as(path: &Path, name: &str) -> Result<ApkFile, String> {
        check_name(name)?;
        let mut file =
            File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        check_size(metadata.len())?;
        let native_libraries = NativeLibraries::read(&mut file).unwrap_or_default();
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(ApkFile {
            name: name.to_string(),
            size: metadata.len(),
            file,
            native_libraries,
        })
    }

    /// The APK's content, read as it is sent.
    pub fn reader(&mut self) -> &mut dyn Read {
        &mut self.file
    }
}

/// Refuses an APK larger than [`MAX_APK_BYTES`], or an empty one, saying why.
pub fn check_size(size: u64) -> Result<(), String> {
    if size == 0 {
        return Err("the file is empty, so it is not an APK".into());
    }
    if size > MAX_APK_BYTES {
        return Err(format!(
            "the file is {} MiB; APKs up to {} MiB can be installed",
            size.div_ceil(1024 * 1024),
            MAX_APK_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Refuses a name that is not a file's: an empty one, one that has a folder separator or a
/// control character, or one that is longer than [`MAX_NAME_BYTES`].
pub fn check_file_name(name: &str) -> Result<(), String> {
    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "the file's name is longer than {MAX_NAME_BYTES} bytes"
        ));
    }
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(format!("{name:?} is not a file name"));
    }
    Ok(())
}

/// Refuses a name that is not an APK file's: one that does not end in `.apk`, that has a
/// folder separator or a control character, or that is longer than [`MAX_NAME_BYTES`].
pub fn check_name(name: &str) -> Result<(), String> {
    check_file_name(name)?;
    let is_apk = name.len() > 4 && ends_with_ignore_case(name, ".apk");
    if !is_apk {
        return Err(format!(
            "{name:?} is not an APK file; only files ending in .apk can be installed"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr11_only_an_apk_file_name_is_taken() {
        assert!(check_name("notes.apk").is_ok());
        assert!(check_name("NOTES.APK").is_ok());
        assert!(check_name("\u{d55c}\u{ae00}.apk").is_ok());
        for bad in [
            "",
            ".apk",
            "notes.apks",
            "notes.zip",
            "a/b.apk",
            "a\\b.apk",
            "a\nb.apk",
            &format!("{}.apk", "a".repeat(252)),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?} was accepted");
        }
    }

    fn part(name: &str, size: u64, obb: bool) -> InstallPart {
        InstallPart {
            name: name.into(),
            size,
            obb,
        }
    }

    #[test]
    fn fr11_the_files_of_a_split_install_are_checked_before_they_are_sent() {
        let parts = [
            part("base.apk", 10, false),
            part("split_config.x86_64.apk", 5, false),
            part("main.42.com.example.notes.obb", 3, true),
        ];
        assert_eq!(check_parts("notes.xapk", &parts), Ok(18));
        for (bad, why) in [
            (vec![part("main.1.a.obb", 3, true)], "no APK"),
            (
                vec![part("main.1.a.obb", 3, true), part("base.apk", 1, false)],
                "APKs go first",
            ),
            (
                vec![part("base.apk", 1, false), part("BASE.APK", 1, false)],
                "twice",
            ),
            (vec![part("base.apk", 0, false)], "empty"),
            (vec![part("base.zip", 1, false)], "not an APK"),
            (
                vec![part("base.apk", 1, false), part("main.obb.apk", 1, true)],
                "not an expansion file",
            ),
            (vec![part("../base.apk", 1, false)], "not a file name"),
            (vec![part("base.apk", MAX_APK_BYTES + 1, false)], "2048 MiB"),
            (
                vec![
                    part("base.apk", 1, false),
                    part("main.1.a.obb", MAX_INSTALL_BYTES, true),
                ],
                "at once",
            ),
        ] {
            let refused = check_parts("notes.xapk", &bad).unwrap_err();
            assert!(refused.contains(why), "{bad:?}: {refused}");
        }
        assert!(check_parts("a/b.xapk", &parts).is_err());
        let wire = parts[2].to_wire();
        assert!(wire.obb);
        assert_eq!(InstallPart::from_wire(wire), parts[2]);
        assert_eq!(parts[1].to_control().name, "split_config.x86_64.apk");
    }

    #[test]
    fn fr11_an_apk_larger_than_the_limit_or_empty_is_refused() {
        assert!(check_size(MAX_APK_BYTES).is_ok());
        let refused = check_size(MAX_APK_BYTES + 1).unwrap_err();
        assert!(refused.contains("2048 MiB"), "{refused}");
        assert!(check_size(0).is_err());
    }

    #[test]
    fn fr11_every_status_crosses_both_protocols_unchanged() {
        for status in InstallStatus::ALL {
            assert_eq!(
                InstallStatus::from_control(status.to_control() as i32),
                status
            );
            assert_eq!(InstallStatus::from_wire(status.to_wire() as i32), status);
        }
        assert_eq!(InstallStatus::from_control(0), InstallStatus::Other);
        assert_eq!(InstallStatus::from_wire(99), InstallStatus::Other);
    }

    #[test]
    fn fr11_a_failure_names_its_reason_and_androids_words() {
        let outcome = InstallOutcome {
            status: InstallStatus::ConflictingSignature,
            package_name: Some("com.example.notes".into()),
            message: "INSTALL_FAILED_UPDATE_INCOMPATIBLE".into(),
        };
        let text = outcome.to_string();
        assert!(text.contains("signed with another key"), "{text}");
        assert!(text.contains("com.example.notes"), "{text}");
        assert!(
            text.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE"),
            "{text}"
        );
    }

    fn installed(version_code: i64, signer: u8) -> InstalledPackage {
        InstalledPackage {
            version_code,
            version_name: format!("{version_code}.0"),
            signer_sha256: vec![vec![signer; 32]],
        }
    }

    #[test]
    fn fr11_an_apk_is_a_new_install_an_update_or_refused_as_android_would() {
        let signer = vec![vec![1u8; 32]];
        assert_eq!(InstallKind::decide(5, &signer, None), InstallKind::New);
        assert_eq!(
            InstallKind::decide(5, &signer, Some(&installed(4, 1))),
            InstallKind::Update
        );
        assert_eq!(
            InstallKind::decide(5, &signer, Some(&installed(5, 1))),
            InstallKind::Reinstall
        );
        assert_eq!(
            InstallKind::decide(5, &signer, Some(&installed(6, 1))),
            InstallKind::Downgrade
        );
        assert_eq!(
            InstallKind::decide(5, &signer, Some(&installed(4, 2))),
            InstallKind::SignatureMismatch
        );
        assert!(InstallKind::Downgrade.blocked() && InstallKind::SignatureMismatch.blocked());
        assert!(!InstallKind::Update.blocked() && !InstallKind::New.blocked());
    }

    #[test]
    fn fr11_an_update_signed_by_a_rotated_key_whose_history_names_the_old_one_is_taken() {
        // The APK's current signer, then the older one its proof of rotation names.
        let signers = vec![vec![9u8; 32], vec![1u8; 32]];
        assert_eq!(
            InstallKind::decide(5, &signers, Some(&installed(4, 1))),
            InstallKind::Update
        );
    }

    #[test]
    fn fr11_an_installed_package_crosses_both_protocols_unchanged() {
        let package = installed(42, 7);
        let wire = InstalledPackage::to_wire(Some(&package));
        assert_eq!(InstalledPackage::from_wire(wire), Some(package.clone()));
        assert_eq!(
            InstalledPackage::from_wire(InstalledPackage::to_wire(None)),
            None
        );
        let control = control::PackageState {
            request_id: 3,
            installed: true,
            version_code: 42,
            version_name: "42.0".into(),
            signer_sha256: vec![vec![7u8; 32]],
        };
        assert_eq!(InstalledPackage::from_control(&control), Some(package));
    }

    #[test]
    fn fr11_an_apk_is_opened_with_its_name_and_size() {
        let dir = std::env::temp_dir().join(format!("dsa-apk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.apk");
        std::fs::write(&path, b"PK\x03\x04apk").unwrap();
        let mut apk = ApkFile::open(&path).unwrap();
        assert_eq!(apk.name, "notes.apk");
        assert_eq!(apk.size, 7);
        let mut content = Vec::new();
        apk.reader().read_to_end(&mut content).unwrap();
        assert_eq!(content, b"PK\x03\x04apk");
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(ApkFile::open(&dir.join("notes.txt")).is_err());
        assert!(ApkFile::open(&dir.join("missing.apk")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

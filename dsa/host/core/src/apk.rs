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
//! Once an app is installed, the agent's report of the apps a launcher lists carries it to the
//! host like any other installed app, and the window manager gives it a Start menu entry.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use dsa_protocol::control;
use dsa_protocol::host_api::types;

/// The largest APK that is taken, in bytes: 2 GiB.
///
/// Apps put at most a few hundred MiB in one APK and ship the rest as asset packs or expansion
/// files, so this takes any real APK while a file dropped by mistake is refused before it
/// fills the guest's storage.
pub const MAX_APK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

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
            Ok(control::InstallStatus::ConflictingSignature) => {
                InstallStatus::ConflictingSignature
            }
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

/// An APK on the host, opened to be streamed to Android.
pub struct ApkFile {
    /// The file's name, without any folder.
    pub name: String,
    pub size: u64,
    pub file: File,
}

impl ApkFile {
    /// Opens the APK at `path`, refusing it before anything is sent when it is not an APK
    /// file or is too large.
    pub fn open(path: &Path) -> Result<ApkFile, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} names no file", path.display()))?;
        check_name(name)?;
        let file =
            File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        check_size(metadata.len())?;
        Ok(ApkFile {
            name: name.to_string(),
            size: metadata.len(),
            file,
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

/// Refuses a name that is not an APK file's: one that does not end in `.apk`, that has a
/// folder separator or a control character, or that is longer than [`MAX_NAME_BYTES`].
pub fn check_name(name: &str) -> Result<(), String> {
    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "the file's name is longer than {MAX_NAME_BYTES} bytes"
        ));
    }
    if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(format!("{name:?} is not a file name"));
    }
    let is_apk = name.len() > 4
        && name
            .get(name.len() - 4..)
            .is_some_and(|end| end.eq_ignore_ascii_case(".apk"));
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
            "", ".apk", "notes.apks", "notes.zip", "a/b.apk", "a\\b.apk", "a\nb.apk",
            &format!("{}.apk", "a".repeat(252)),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?} was accepted");
        }
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
        assert!(text.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE"), "{text}");
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

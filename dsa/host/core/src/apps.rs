//! Managing the apps installed in the user's Android: uninstalling one, and how it went.
//!
//! The list of installed apps itself is part of the tasks' state ([`crate::tasks`]), because
//! the window manager follows it for the Start menu. Only apps the user installed can be
//! uninstalled: an app that is part of the Android image stays, and Android says so.

use std::fmt;

use dsa_protocol::control;
use dsa_protocol::host_api::types;

/// A short name for a shortcut's ID that any file name or bundle identifier can carry: an
/// FNV-1a hash of it, in hex. Android takes any text as an ID, which a file name cannot always.
pub fn shortcut_tag(id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// How long the service waits for Android to uninstall an app.
pub const UNINSTALL_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the service waits for Android to read its apps again, sizes included.
pub const REFRESH_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Whether an app was uninstalled, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallStatus {
    Uninstalled,
    /// No app of that package is installed.
    NotInstalled,
    /// The app is part of the Android image.
    SystemApp,
    /// Android refused: the app is a device administrator, or a policy keeps it.
    Blocked,
    Other,
}

impl UninstallStatus {
    /// A stable name for programs, such as the settings app.
    pub fn code(self) -> &'static str {
        match self {
            UninstallStatus::Uninstalled => "uninstalled",
            UninstallStatus::NotInstalled => "not-installed",
            UninstallStatus::SystemApp => "system-app",
            UninstallStatus::Blocked => "blocked",
            UninstallStatus::Other => "other",
        }
    }

    /// What the status means, as a sentence.
    pub fn describe(self) -> &'static str {
        match self {
            UninstallStatus::Uninstalled => "The app is uninstalled",
            UninstallStatus::NotInstalled => "No app of this package is installed in Android",
            UninstallStatus::SystemApp => {
                "The app is part of Android and cannot be uninstalled; only apps you installed can"
            }
            UninstallStatus::Blocked => {
                "Android does not allow the app to be uninstalled: it is a device administrator, \
                 or a policy keeps it"
            }
            UninstallStatus::Other => "Android did not uninstall the app",
        }
    }

    /// The status a control message carries. An unknown or missing one is `Other`.
    pub fn from_control(status: i32) -> UninstallStatus {
        match control::UninstallStatus::try_from(status) {
            Ok(control::UninstallStatus::Success) => UninstallStatus::Uninstalled,
            Ok(control::UninstallStatus::NotInstalled) => UninstallStatus::NotInstalled,
            Ok(control::UninstallStatus::SystemApp) => UninstallStatus::SystemApp,
            Ok(control::UninstallStatus::Blocked) => UninstallStatus::Blocked,
            Ok(control::UninstallStatus::Other)
            | Ok(control::UninstallStatus::Unspecified)
            | Err(_) => UninstallStatus::Other,
        }
    }

    pub fn to_wire(self) -> types::UninstallStatus {
        match self {
            UninstallStatus::Uninstalled => types::UninstallStatus::Success,
            UninstallStatus::NotInstalled => types::UninstallStatus::NotInstalled,
            UninstallStatus::SystemApp => types::UninstallStatus::SystemApp,
            UninstallStatus::Blocked => types::UninstallStatus::Blocked,
            UninstallStatus::Other => types::UninstallStatus::Other,
        }
    }

    /// The status the local API carries. An unknown or missing one is `Other`.
    pub fn from_wire(status: i32) -> UninstallStatus {
        match types::UninstallStatus::try_from(status) {
            Ok(types::UninstallStatus::Success) => UninstallStatus::Uninstalled,
            Ok(types::UninstallStatus::NotInstalled) => UninstallStatus::NotInstalled,
            Ok(types::UninstallStatus::SystemApp) => UninstallStatus::SystemApp,
            Ok(types::UninstallStatus::Blocked) => UninstallStatus::Blocked,
            Ok(types::UninstallStatus::Other)
            | Ok(types::UninstallStatus::Unspecified)
            | Err(_) => UninstallStatus::Other,
        }
    }
}

/// How an uninstall ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub package_name: String,
    pub status: UninstallStatus,
    /// Android's own words, for the log and a bug report.
    pub message: String,
}

/// The longest message from Android that is passed on.
const MAX_MESSAGE: usize = 512;

impl UninstallOutcome {
    pub fn uninstalled(&self) -> bool {
        self.status == UninstallStatus::Uninstalled
    }

    pub fn from_control(result: &control::UninstallResult) -> UninstallOutcome {
        UninstallOutcome {
            package_name: result.package_name.clone(),
            status: UninstallStatus::from_control(result.status),
            message: result.message.chars().take(MAX_MESSAGE).collect(),
        }
    }

    pub fn to_wire(&self) -> types::UninstallResult {
        types::UninstallResult {
            package_name: self.package_name.clone(),
            status: self.status.to_wire() as i32,
            message: self.message.clone(),
        }
    }

    pub fn from_wire(result: types::UninstallResult) -> UninstallOutcome {
        UninstallOutcome {
            package_name: result.package_name,
            status: UninstallStatus::from_wire(result.status),
            message: result.message.chars().take(MAX_MESSAGE).collect(),
        }
    }
}

impl fmt::Display for UninstallOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.status.describe())?;
        if !self.uninstalled() && !self.message.trim().is_empty() {
            write!(f, " ({})", self.message.trim())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr23_every_status_survives_the_control_channel_and_the_local_api() {
        for status in [
            UninstallStatus::Uninstalled,
            UninstallStatus::NotInstalled,
            UninstallStatus::SystemApp,
            UninstallStatus::Blocked,
            UninstallStatus::Other,
        ] {
            assert_eq!(UninstallStatus::from_wire(status.to_wire() as i32), status);
        }
        assert_eq!(
            UninstallStatus::from_control(control::UninstallStatus::SystemApp as i32),
            UninstallStatus::SystemApp
        );
        assert_eq!(UninstallStatus::from_control(99), UninstallStatus::Other);
    }
}

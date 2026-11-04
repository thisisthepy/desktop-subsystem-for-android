//! Where the host keeps its files, as a port each platform fills in.
//!
//! The core never builds a path from the host's own folders; it is handed a [`HostPaths`]
//! and asks it. There are two roots and nothing else: the machine's, which only the service
//! writes, and each user's, which that user's own processes write. Every place below is in
//! one of them, so uninstalling knows what to remove.
//!
//! | Place | Root | Holds |
//! |---|---|---|
//! | [`machine_data`](HostPaths::machine_data) | machine | the service's configuration |
//! | [`images`](HostPaths::images) | machine | the Android image store |
//! | [`updates`](HostPaths::updates) | machine | downloaded setup files and the update state |
//! | [`settings`](HostPaths::settings) | machine | the settings users change, kept by the service |
//! | [`machine_logs`](HostPaths::machine_logs) | machine | the service's log and each guest's console |
//! | [`machine_tmp`](HostPaths::machine_tmp) | machine | files that cannot be avoided, such as each boot's initrd |
//! | [`user_data`](HostPaths::user_data) | user | the user's Android disk and its image version |
//! | [`apps`](HostPaths::apps) | user | each app's own window program |
//! | [`user_logs`](HostPaths::user_logs) | user | the logs of the processes in the user's session |
//! | [`user_tmp`](HostPaths::user_tmp) | user | files that cannot be avoided, in user processes |
//!
//! A user is named the way the platform names one (a SID on Windows, a user ID on Unix
//! hosts). Finding a user's root can fail (an account without a profile), so the user
//! places return why.

use std::path::{Path, PathBuf};

/// Where the host keeps its files. See the module documentation for what each place holds.
pub trait HostPaths: Send + Sync {
    /// The machine's root, which holds the service's configuration.
    fn machine_data(&self) -> PathBuf;
    /// The Android image store.
    fn images(&self) -> PathBuf;
    /// Downloaded setup files and the update state.
    fn updates(&self) -> PathBuf;
    /// The settings users change from the settings app.
    fn settings(&self) -> PathBuf;
    /// The service's log and each guest's console log.
    fn machine_logs(&self) -> PathBuf;
    /// Temporary files of the service.
    fn machine_tmp(&self) -> PathBuf;
    /// The root of `user`, which holds their Android disk.
    fn user_data(&self, user: &str) -> Result<PathBuf, String>;
    /// Each app's own window program, for `user`.
    fn apps(&self, user: &str) -> Result<PathBuf, String>;
    /// The logs of the processes in `user`'s session.
    fn user_logs(&self, user: &str) -> Result<PathBuf, String>;
    /// Temporary files of `user`'s processes.
    fn user_tmp(&self, user: &str) -> Result<PathBuf, String>;
}

/// [`HostPaths`] under two folders a test chooses, with the same layout as the hosts use:
/// the machine's places below `machine`, and each user's root at `users/<user>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeHostPaths {
    pub machine: PathBuf,
    pub users: PathBuf,
}

impl FakeHostPaths {
    pub fn new(machine: impl Into<PathBuf>, users: impl Into<PathBuf>) -> FakeHostPaths {
        FakeHostPaths {
            machine: machine.into(),
            users: users.into(),
        }
    }

    fn user(&self, user: &str) -> Result<PathBuf, String> {
        if user.is_empty() || user.contains(['/', '\\']) || user == "." || user == ".." {
            return Err(format!("{user:?} is not a user"));
        }
        Ok(self.users.join(user))
    }
}

fn below(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

impl HostPaths for FakeHostPaths {
    fn machine_data(&self) -> PathBuf {
        self.machine.clone()
    }
    fn images(&self) -> PathBuf {
        below(&self.machine, "images")
    }
    fn updates(&self) -> PathBuf {
        below(&self.machine, "updates")
    }
    fn settings(&self) -> PathBuf {
        below(&self.machine, "settings")
    }
    fn machine_logs(&self) -> PathBuf {
        below(&self.machine, "logs")
    }
    fn machine_tmp(&self) -> PathBuf {
        below(&self.machine, "tmp")
    }
    fn user_data(&self, user: &str) -> Result<PathBuf, String> {
        self.user(user)
    }
    fn apps(&self, user: &str) -> Result<PathBuf, String> {
        Ok(below(&self.user(user)?, "apps"))
    }
    fn user_logs(&self, user: &str) -> Result<PathBuf, String> {
        Ok(below(&self.user(user)?, "logs"))
    }
    fn user_tmp(&self, user: &str) -> Result<PathBuf, String> {
        Ok(below(&self.user(user)?, "tmp"))
    }
}

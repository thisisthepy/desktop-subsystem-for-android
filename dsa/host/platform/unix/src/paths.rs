//! Where a Unix host keeps its files: the [`HostPaths`] of the Linux and macOS hosts.
//!
//! The layout follows each system's own convention, with the same places as every host:
//!
//! ```text
//! Linux (the XDG base directories, the FHS for the machine)
//!   /var/lib/dsa                      the machine's: images, updates, settings, logs, tmp
//!   $XDG_DATA_HOME/dsa                each user's, ~/.local/share/dsa when that is unset
//!
//! macOS
//!   /Library/Application Support/DSA  the machine's
//!   ~/Library/Application Support/DSA each user's
//! ```
//!
//! Each user's root holds their Android disk and the `apps`, `logs` and `tmp` folders, as on
//! Windows. The Linux and macOS hosts that will use these paths are planned and not built
//! yet; this module only fixes where their files go.

use std::path::{Path, PathBuf};

use dsa_host_core::host_paths::HostPaths;

/// The folder below each root that is DSA's on Linux.
const LINUX_DSA: &str = "dsa";
/// The folder below each root that is DSA's on macOS, as applications name theirs there.
const MACOS_DSA: &str = "DSA";

/// Finds a user's home folder from their user ID.
pub type FindHome = Box<dyn Fn(&str) -> Result<PathBuf, String> + Send + Sync>;

/// Which system's convention the paths follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Linux,
    MacOs,
}

/// The Unix [`HostPaths`]: a machine root and each user's root below their home.
pub struct UnixHostPaths {
    layout: Layout,
    machine: PathBuf,
    find_home: FindHome,
    /// `$XDG_DATA_HOME` of the user this process runs as, which wins for that user on Linux.
    xdg_data_home: Option<(String, PathBuf)>,
}

impl UnixHostPaths {
    /// The paths of `layout` with the machine root at `machine`, finding each user's home
    /// with `find_home`.
    pub fn new(layout: Layout, machine: impl Into<PathBuf>, find_home: FindHome) -> UnixHostPaths {
        UnixHostPaths {
            layout,
            machine: machine.into(),
            find_home,
            xdg_data_home: None,
        }
    }

    /// The paths of `layout` at the system's own machine root.
    pub fn standard(layout: Layout, find_home: FindHome) -> UnixHostPaths {
        let machine = match layout {
            Layout::Linux => PathBuf::from("/var/lib").join(LINUX_DSA),
            Layout::MacOs => PathBuf::from("/Library/Application Support").join(MACOS_DSA),
        };
        UnixHostPaths::new(layout, machine, find_home)
    }

    /// Uses `dir` as the `$XDG_DATA_HOME` of the user `user`, on Linux.
    pub fn with_xdg_data_home(mut self, user: &str, dir: impl Into<PathBuf>) -> UnixHostPaths {
        self.xdg_data_home = Some((user.to_string(), dir.into()));
        self
    }

    fn user_root(&self, user: &str) -> Result<PathBuf, String> {
        if self.layout == Layout::Linux {
            if let Some((owner, dir)) = &self.xdg_data_home {
                if owner == user && dir.is_absolute() {
                    return Ok(dir.join(LINUX_DSA));
                }
            }
        }
        let home = (self.find_home)(user)?;
        Ok(match self.layout {
            Layout::Linux => home.join(".local").join("share").join(LINUX_DSA),
            Layout::MacOs => home
                .join("Library")
                .join("Application Support")
                .join(MACOS_DSA),
        })
    }
}

fn below(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

impl HostPaths for UnixHostPaths {
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
        self.user_root(user)
    }
    fn apps(&self, user: &str) -> Result<PathBuf, String> {
        Ok(below(&self.user_root(user)?, "apps"))
    }
    fn user_logs(&self, user: &str) -> Result<PathBuf, String> {
        Ok(below(&self.user_root(user)?, "logs"))
    }
    fn user_tmp(&self, user: &str) -> Result<PathBuf, String> {
        Ok(below(&self.user_root(user)?, "tmp"))
    }
}

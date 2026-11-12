//! Which apps see the computer's memory as the device's memory.
//!
//! Google Play picks the build of an app by the device's memory: a device under 8 GB gets a
//! build of Chrome whose browser is 32-bit, which this 64-bit only guest cannot start. The VM
//! starts small and grows, so its size says nothing about the computer. The host passes the
//! computer's physical memory at each start (`kernel.dsa_host_memory.total_kb` in the
//! bootconfig), and two places show it, only to the Play Store and Google Play services:
//!
//! - the guest kernel, as `MemTotal` in `/proc/meminfo` and the total of `sysinfo()`, for a
//!   process whose uid is in `/sys/module/dsa_host_memory/parameters/uids`;
//! - the framework, as `ActivityManager.MemoryInfo.totalMem`, which system_server computes in
//!   its own process for whichever app asks: it reads the same two kernel parameters and
//!   answers a binder caller whose uid is in `uids` with `total_kb`.
//!
//! The daemon finds the apps' uids in the package manager's list ([`APPS`]), which exists only
//! once Android has booted and changes when Google Play is installed or removed, and sets
//! [`PROPERTY`]. init writes the same list to the kernel (`dsa-memory.rc`): only init and the
//! daemon may set the property, and only init may write the kernel's list. Every other app, and
//! Android itself, sees the VM's real memory, so the guest's memory management is unchanged.

use std::collections::{BTreeSet, HashMap};

/// The apps that see the computer's memory: the Play Store and Google Play services (all of
/// whose processes run as its uid).
pub const APPS: [&str; 2] = ["com.android.vending", "com.google.android.gms"];

/// The property that names their uids, comma separated, or [`NONE`].
pub const PROPERTY: &str = "vendor.dsa.host_memory.uids";

/// The value that names no app: an empty property cannot be set.
pub const NONE: &str = "none";

/// How often the daemon reads the package list again, so an install or removal of Google Play
/// is followed while Android runs.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(30);

/// The property value for the uids `uids` (the package manager's list) gives [`APPS`]: their
/// app IDs (a uid modulo 100000, the same app for every Android user), in order, comma
/// separated, or [`NONE`] when neither is installed.
pub fn property_value(uids: &HashMap<String, u32>) -> String {
    let found: BTreeSet<u32> = APPS
        .iter()
        .filter_map(|package| uids.get(*package))
        .map(|uid| uid % 100_000)
        .collect();
    if found.is_empty() {
        NONE.to_string()
    } else {
        found
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Sets the property from the package list each time it changes, starting with the first
/// list read. `read` gives the package manager's list (`None` while it cannot be read); `set`
/// sets the property and says why it could not.
pub struct HostMemoryApps {
    last: Option<String>,
}

impl Default for HostMemoryApps {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMemoryApps {
    pub fn new() -> HostMemoryApps {
        HostMemoryApps { last: None }
    }

    /// The value to set now, if it differs from the one set last: from the package list
    /// `packages_list` (its text), or nothing while it cannot be read.
    pub fn look(&mut self, packages_list: Option<&str>) -> Option<String> {
        let text = packages_list?;
        let value = property_value(&crate::su_relay::uids_from_packages_list(text));
        if self.last.as_deref() == Some(value.as_str()) {
            return None;
        }
        Some(value)
    }

    /// Remembers that `value` was set, so it is not set again until it changes.
    pub fn set(&mut self, value: String) {
        self.last = Some(value);
    }
}

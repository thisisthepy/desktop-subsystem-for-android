//! Which apps see which memory as the device's memory.
//!
//! The VM starts small and grows, up to a maximum the host sets, so its size says nothing about
//! what an app may use, and apps decide by it: a game refuses to start under its requirement,
//! and Google Play gives a device under 8 GB a build of Chrome whose browser is 32-bit, which
//! this 64-bit only guest cannot start. The host passes two sizes at each start in the
//! bootconfig: the memory the VM may grow to (`kernel.dsa_host_memory.total_kb`) and the
//! computer's physical memory (`kernel.dsa_host_memory.store_total_kb`). Two places show them:
//!
//! - the guest kernel, as `MemTotal` in `/proc/meminfo` and the total of `sysinfo()`, with what
//!   the VM may still grow added to free and available memory, for every app process (an app
//!   ID of 10000 or more);
//! - the framework, as `ActivityManager.MemoryInfo.totalMem` and `availMem`, which
//!   system_server computes in its own process for whichever app asks: it reads the same kernel
//!   parameters and answers a binder caller by the same rules.
//!
//! The daemon names two kinds of app, by their uids from the package manager's list, which
//! exists only once Android has booted and changes when an app is installed or removed:
//!
//! - [`APPS`], the Play Store and Google Play services, which see the computer's memory as
//!   their total, in [`PROPERTY`] (the kernel's `uids`);
//! - [`REAL_SIZE_APPS`], DSA's agent, which reports the guest's memory to the host and so sees
//!   the VM's own size, as Android's system processes do, in [`REAL_SIZE_PROPERTY`] (the
//!   kernel's `real_uids`).
//!
//! init writes them to the kernel (`dsa-memory.rc`): only init and the daemon may set the
//! properties, and only init may write the kernel's lists.

use std::collections::{BTreeSet, HashMap};

/// The apps that see the computer's memory: the Play Store and Google Play services (all of
/// whose processes run as its uid).
pub const APPS: [&str; 2] = ["com.android.vending", "com.google.android.gms"];

/// The property that names [`APPS`]' uids, comma separated, or [`NONE`].
pub const PROPERTY: &str = "vendor.dsa.host_memory.uids";

/// The apps that see the VM's own memory, as Android's system processes do: DSA's agent, which
/// measures the guest's memory and reports it to the host.
pub const REAL_SIZE_APPS: [&str; 1] = ["io.github.thisisthepy.dsa.agent"];

/// The property that names [`REAL_SIZE_APPS`]' uids, comma separated, or [`NONE`].
pub const REAL_SIZE_PROPERTY: &str = "vendor.dsa.host_memory.real_uids";

/// The value that names no app: an empty property cannot be set.
pub const NONE: &str = "none";

/// How often the daemon reads the package list again, so an install or removal is followed
/// while Android runs.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(30);

/// The property value for the uids `uids` (the package manager's list) gives [`APPS`].
pub fn property_value(uids: &HashMap<String, u32>) -> String {
    property_value_for(&APPS, uids)
}

/// The property value for the uids `uids` (the package manager's list) gives `apps`: their app
/// IDs (a uid modulo 100000, the same app for every Android user), in order, comma separated,
/// or [`NONE`] when none is installed.
pub fn property_value_for(apps: &[&str], uids: &HashMap<String, u32>) -> String {
    let found: BTreeSet<u32> = apps
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

/// Sets a property from the package list each time it changes, starting with the first list
/// read.
pub struct HostMemoryApps {
    apps: &'static [&'static str],
    last: Option<String>,
}

impl Default for HostMemoryApps {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMemoryApps {
    /// For [`APPS`], which [`PROPERTY`] names.
    pub fn new() -> HostMemoryApps {
        HostMemoryApps::for_apps(&APPS)
    }

    /// For `apps`.
    pub fn for_apps(apps: &'static [&'static str]) -> HostMemoryApps {
        HostMemoryApps { apps, last: None }
    }

    /// The value to set now, if it differs from the one set last: from the package list
    /// `packages_list` (its text), or nothing while it cannot be read.
    pub fn look(&mut self, packages_list: Option<&str>) -> Option<String> {
        let text = packages_list?;
        let value = property_value_for(self.apps, &crate::su_relay::uids_from_packages_list(text));
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

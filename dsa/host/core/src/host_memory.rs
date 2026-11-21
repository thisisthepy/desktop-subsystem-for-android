//! The memory the host tells the guest at each start, which apps see as the device's memory.
//!
//! The guest's VM starts small and grows as Android needs it, up to a maximum (the user's
//! limit, or the computer's memory less what the host keeps), so the size its kernel reports
//! says nothing about what an app may use. Apps decide by that size: a game refuses to start
//! when the device has less memory than it requires, and Google Play picks the build of an app
//! by it (a device under 8 GB gets a build of Chrome whose browser is 32-bit, which the 64-bit
//! only guest cannot start).
//!
//! The host puts three sizes in the boot's bootconfig ([`bootconfig`]):
//!
//! - the memory the VM may grow to ([`BOOTCONFIG_KEY`]), which every app sees as `MemTotal` in
//!   `/proc/meminfo`, as the total of `sysinfo()` and as `ActivityManager.MemoryInfo.totalMem`.
//!   What the VM may still grow is added to the free and available memory those apps see.
//! - the computer's physical memory ([`STORE_BOOTCONFIG_KEY`]), which only the Play Store and
//!   Google Play services see as their total, so Play still picks the 64-bit build on a
//!   computer whose VM may grow to less than 8 GB.
//! - the memory the computer is installed with ([`INSTALLED_BOOTCONFIG_KEY`]), from which the
//!   framework answers `advertisedMem`.
//!
//! Android's own processes (init, lmkd, system_server and the other system uids) keep the VM's
//! real size, so memory management in the guest is unchanged.

/// The bootconfig key the guest kernel reads the memory the VM may grow to from, in KiB. Keys
/// under `kernel.` reach the kernel as command line parameters.
pub const BOOTCONFIG_KEY: &str = "kernel.dsa_host_memory.total_kb";

/// The bootconfig key the guest kernel reads the computer's physical memory from, in KiB,
/// which the app store sees.
pub const STORE_BOOTCONFIG_KEY: &str = "kernel.dsa_host_memory.store_total_kb";

/// The bootconfig key that carries the memory the computer is installed with, in KiB. Android
/// turns an `androidboot.` key into the read-only property `ro.boot.dsa.host_installed_kb`,
/// which the framework reads to answer `advertisedMem`, the figure a user knows the computer by
/// ("32 GB"). The total memory the operating system can use is a little less than that (the
/// firmware and the graphics hardware keep some), and rounding it up the way Android does for a
/// phone turned a 32 GB computer into 64 GB.
pub const INSTALLED_BOOTCONFIG_KEY: &str = "androidboot.dsa.host_installed_kb";

/// The bootconfig lines that tell the guest how much memory apps see: `vm_max_mb`, the most
/// the VM may grow to, never more than the computer's `host_total_kb`; the computer's memory
/// for the app store; and the memory the computer is installed with (`installed_kb`, which
/// `advertisedMem` is made from). A size the host could not read (`None`) or read as 0 is left
/// out: the apps it is for then see the VM's own size, as before, and without the installed
/// figure the framework uses the total.
pub fn bootconfig(
    host_total_kb: Option<u64>,
    vm_max_mb: Option<u64>,
    installed_kb: Option<u64>,
) -> String {
    let host_total_kb = host_total_kb.filter(|kb| *kb > 0);
    let apps_kb = vm_max_mb
        .map(|mb| mb.saturating_mul(1024))
        .map(|kb| host_total_kb.map_or(kb, |host| kb.min(host)));
    let line = |key: &str, kb: Option<u64>| match kb {
        Some(kb) if kb > 0 => format!("{key} = \"{kb}\"\n"),
        _ => String::new(),
    };
    line(BOOTCONFIG_KEY, apps_kb)
        + &line(STORE_BOOTCONFIG_KEY, host_total_kb)
        + &line(INSTALLED_BOOTCONFIG_KEY, installed_kb)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB_KB: u64 = 1024 * 1024;

    #[test]
    fn fr32_apps_are_told_the_memory_the_vm_may_grow_to() {
        // A 16 GiB computer whose VM may grow to 14 GiB (automatic: all but 2 GiB).
        assert_eq!(
            bootconfig(Some(16 * GIB_KB), Some(14 * 1024), None),
            "kernel.dsa_host_memory.total_kb = \"14680064\"\n\
             kernel.dsa_host_memory.store_total_kb = \"16777216\"\n"
        );
    }

    #[test]
    fn fr32_apps_see_the_users_limit_when_they_set_one() {
        // The user limited Android to 6 GiB on a 32 GiB computer.
        let lines = bootconfig(Some(32 * GIB_KB), Some(6 * 1024), None);
        assert!(lines.contains("kernel.dsa_host_memory.total_kb = \"6291456\"\n"));
    }

    #[test]
    fn fr32_the_bootconfig_tells_the_guest_the_memory_the_computer_is_installed_with() {
        // A 32 GiB computer that Windows reports 31.5 GiB of: Play is told 32 GiB installed.
        assert_eq!(
            bootconfig(Some(33_043_084), None, Some(32 * GIB_KB)),
            "kernel.dsa_host_memory.store_total_kb = \"33043084\"\n\
             androidboot.dsa.host_installed_kb = \"33554432\"\n"
        );
        assert_eq!(
            bootconfig(None, None, Some(1024)),
            "androidboot.dsa.host_installed_kb = \"1024\"\n"
        );
    }

    #[test]
    fn fr32_apps_never_see_more_than_the_computer_has() {
        let lines = bootconfig(Some(8 * GIB_KB), Some(64 * 1024), None);
        assert!(lines.contains("kernel.dsa_host_memory.total_kb = \"8388608\"\n"));
    }

    #[test]
    fn fr32_google_play_sees_the_computers_memory_where_the_vm_may_grow_to_less() {
        // An 8 GiB computer: the VM may grow to 6 GiB, under the 8 GB at which Google Play
        // picks the 64-bit Chrome, so the app store is told the computer's memory.
        let max_mb = crate::memory::automatic_vm_memory_mb(8 * 1024);
        assert!(max_mb < 8 * 1024);
        let lines = bootconfig(Some(8 * GIB_KB), Some(max_mb), None);
        assert!(lines.contains("kernel.dsa_host_memory.store_total_kb = \"8388608\"\n"));
    }

    #[test]
    fn fr32_a_size_the_host_cannot_read_is_left_out() {
        assert_eq!(bootconfig(None, None, None), "");
        assert_eq!(bootconfig(Some(0), Some(0), Some(0)), "");
        assert_eq!(
            bootconfig(None, Some(4096), None),
            "kernel.dsa_host_memory.total_kb = \"4194304\"\n"
        );
        assert_eq!(
            bootconfig(Some(GIB_KB), None, None),
            "kernel.dsa_host_memory.store_total_kb = \"1048576\"\n"
        );
    }

    #[test]
    fn fr32_the_lines_are_ones_the_guest_kernel_takes_as_parameters() {
        // The kernel turns the keys under "kernel." into command line parameters; anything
        // else is left to user space.
        let lines = bootconfig(Some(2), Some(1), None);
        let keys: Vec<_> = lines
            .lines()
            .map(|line| line.split_once(" = ").unwrap().0)
            .map(|key| key.strip_prefix("kernel.").unwrap())
            .collect();
        assert_eq!(
            keys,
            ["dsa_host_memory.total_kb", "dsa_host_memory.store_total_kb"]
        );
    }
}

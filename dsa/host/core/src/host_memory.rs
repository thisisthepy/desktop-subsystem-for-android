//! The computer's physical memory, which the host tells the guest at each start so that Google
//! Play sees it as the device's memory.
//!
//! Google Play picks the build of an app by the device's memory: a device under 8 GB gets a
//! build of Chrome whose browser is 32-bit, which the 64-bit only guest cannot start. The
//! guest's VM starts small and grows as Android needs it, so the size its kernel reports says
//! nothing about the computer. The host puts the computer's physical memory in the boot's
//! bootconfig ([`bootconfig`]). The guest kernel shows it, as `MemTotal` in `/proc/meminfo` and
//! as the total of `sysinfo()`, only to the apps the guest's control daemon names (the Play
//! Store and Google Play services); every other process, Android's own included, keeps the VM's
//! real size, so memory management in the guest is unchanged.

/// The bootconfig key the guest kernel reads the computer's memory from, in KiB. Keys under
/// `kernel.` reach the kernel as command line parameters.
pub const BOOTCONFIG_KEY: &str = "kernel.dsa_host_memory.total_kb";

/// The bootconfig key that carries the memory the computer is installed with, in KiB. Android
/// turns an `androidboot.` key into the read-only property `ro.boot.dsa.host_installed_kb`,
/// which the framework reads to answer `advertisedMem`, the figure a user knows the computer by
/// ("32 GB"). The total memory the operating system can use is a little less than that (the
/// firmware and the graphics hardware keep some), and rounding it up the way Android does for a
/// phone turned a 32 GB computer into 64 GB.
pub const INSTALLED_BOOTCONFIG_KEY: &str = "androidboot.dsa.host_installed_kb";

/// The bootconfig lines that tell the guest the computer's memory: its physical memory
/// (`total_kb` KiB, which the guest kernel shows) and the memory it is installed with
/// (`installed_kb` KiB, which `advertisedMem` is made from). Nothing for a figure the host could
/// not read (`None`) or read as nothing (0): without the total the guest shows every app its own
/// size, as before, and without the installed figure the framework uses the total.
pub fn bootconfig(total_kb: Option<u64>, installed_kb: Option<u64>) -> String {
    let line = |key: &str, kb: Option<u64>| match kb {
        Some(kb) if kb > 0 => format!("{key} = \"{kb}\"\n"),
        _ => String::new(),
    };
    line(BOOTCONFIG_KEY, total_kb) + &line(INSTALLED_BOOTCONFIG_KEY, installed_kb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr32_the_bootconfig_tells_the_guest_the_computers_memory() {
        // A 16 GiB computer.
        assert_eq!(
            bootconfig(Some(16 * 1024 * 1024), None),
            "kernel.dsa_host_memory.total_kb = \"16777216\"\n"
        );
    }

    #[test]
    fn fr32_the_bootconfig_tells_the_guest_the_memory_the_computer_is_installed_with() {
        // A 32 GiB computer that Windows reports 31.5 GiB of: Play is told 32 GiB installed.
        assert_eq!(
            bootconfig(Some(33_043_084), Some(32 * 1024 * 1024)),
            "kernel.dsa_host_memory.total_kb = \"33043084\"\n\
             androidboot.dsa.host_installed_kb = \"33554432\"\n"
        );
        assert_eq!(
            bootconfig(None, Some(1024)),
            "androidboot.dsa.host_installed_kb = \"1024\"\n"
        );
    }

    #[test]
    fn fr32_a_host_that_cannot_read_its_memory_tells_the_guest_nothing() {
        assert_eq!(bootconfig(None, None), "");
        assert_eq!(bootconfig(Some(0), Some(0)), "");
    }

    #[test]
    fn fr32_the_line_is_one_the_guest_kernel_takes_as_a_parameter() {
        // The kernel turns the keys under "kernel." into command line parameters; anything
        // else is left to user space.
        let line = bootconfig(Some(1), None);
        let (key, value) = line.trim_end().split_once(" = ").unwrap();
        assert_eq!(
            key.strip_prefix("kernel."),
            Some("dsa_host_memory.total_kb")
        );
        assert_eq!(value, "\"1\"");
    }
}

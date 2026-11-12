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

/// The bootconfig line that tells the guest the computer's physical memory, `total_kb` KiB.
/// Nothing when the host could not read it (`None`) or read nothing (0): the guest then shows
/// every app its own size, as before.
pub fn bootconfig(total_kb: Option<u64>) -> String {
    match total_kb {
        Some(kb) if kb > 0 => format!("{BOOTCONFIG_KEY} = \"{kb}\"\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr32_the_bootconfig_tells_the_guest_the_computers_memory() {
        // A 16 GiB computer.
        assert_eq!(
            bootconfig(Some(16 * 1024 * 1024)),
            "kernel.dsa_host_memory.total_kb = \"16777216\"\n"
        );
    }

    #[test]
    fn fr32_a_host_that_cannot_read_its_memory_tells_the_guest_nothing() {
        assert_eq!(bootconfig(None), "");
        assert_eq!(bootconfig(Some(0)), "");
    }

    #[test]
    fn fr32_the_line_is_one_the_guest_kernel_takes_as_a_parameter() {
        // The kernel turns the keys under "kernel." into command line parameters; anything
        // else is left to user space.
        let line = bootconfig(Some(1));
        let (key, value) = line.trim_end().split_once(" = ").unwrap();
        assert_eq!(
            key.strip_prefix("kernel."),
            Some("dsa_host_memory.total_kb")
        );
        assert_eq!(value, "\"1\"");
    }
}

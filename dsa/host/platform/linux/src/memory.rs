//! The host's share of each user's Android: its crosvm process, and the window manager, the
//! graphics renderer (`dsa-gfx`) and the window processes in the user's session, from
//! `/proc`.
//!
//! crosvm keeps the guest's memory in a memory file that its sandboxed device processes map
//! too, so the guest's pages count as shared in crosvm's own `smaps_rollup`. The VM's share is
//! therefore everything resident or swapped out in the crosvm process, which maps all of it.

use dsa_host_core::memory::{HostMemory, HostProcessMemory, HostRole};

/// `VmRSS` from a `/proc/<pid>/status`, in KiB.
pub fn resident_kb(status: &str) -> Option<u64> {
    kb_field(status, "VmRSS:")
}

/// The memory only this process has, from a `/proc/<pid>/smaps_rollup`, in KiB: its private
/// pages, resident or swapped out.
pub fn private_kb(smaps_rollup: &str) -> Option<u64> {
    let clean = kb_field(smaps_rollup, "Private_Clean:")?;
    let dirty = kb_field(smaps_rollup, "Private_Dirty:")?;
    let swap = kb_field(smaps_rollup, "Swap:").unwrap_or(0);
    Some(clean + dirty + swap)
}

/// The VM's memory from its crosvm process's `/proc/<pid>/status`, in KiB: resident and
/// swapped out, shared with its device processes or not.
pub fn vm_kb(status: &str) -> Option<u64> {
    Some(resident_kb(status)? + kb_field(status, "VmSwap:").unwrap_or(0))
}

fn kb_field(text: &str, name: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(name))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse().ok())
}

/// The owner's user ID, from a `/proc/<pid>/status`.
pub fn owner_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|uid| uid.parse().ok())
}

/// The role of a process named `name` in a user's session, if it is one of DSA's windows.
pub fn window_role(name: &str) -> Option<HostRole> {
    match name {
        "dsa-compositor" => Some(HostRole::WindowManager),
        "dsa-gfx" => Some(HostRole::Renderer),
        "dsa-app-window" => Some(HostRole::AppWindow),
        _ => None,
    }
}

/// One process's memory from `/proc`: resident, and only its own (for the VM, all of the
/// guest's).
pub fn process(pid: u32, role: HostRole) -> Option<HostProcessMemory> {
    let dir = format!("/proc/{pid}");
    let status = std::fs::read_to_string(format!("{dir}/status")).ok()?;
    let working_set_kb = resident_kb(&status)?;
    let private = if role == HostRole::Vm {
        vm_kb(&status).unwrap_or(working_set_kb)
    } else {
        std::fs::read_to_string(format!("{dir}/smaps_rollup"))
            .ok()
            .and_then(|text| private_kb(&text))
            .unwrap_or(working_set_kb)
    };
    // A crosvm started with `--pmem` maps the direct-mapped disks' files; the guest's memory
    // is a memory file, counted as shared memory, not as a file's.
    let with_disks = role == HostRole::Vm
        && std::fs::read(format!("{dir}/cmdline")).is_ok_and(|line| maps_disks(&line));
    let mapped_kb = if with_disks {
        mapped_file_kb(&status).min(working_set_kb)
    } else {
        0
    };
    Some(HostProcessMemory {
        role,
        pid,
        working_set_kb,
        private_kb: private,
        mapped_kb,
    })
}

/// The resident pages a process maps from files (`RssFile` of a `/proc/<pid>/status`), in
/// KiB: for a crosvm process that maps the direct-mapped disks, their pages, and its own
/// program files, a few MiB.
pub fn mapped_file_kb(status: &str) -> u64 {
    kb_field(status, "RssFile:").unwrap_or(0)
}

/// Whether a crosvm command line (`/proc/<pid>/cmdline`, its words NUL-separated) attaches a
/// disk as persistent memory.
pub fn maps_disks(cmdline: &[u8]) -> bool {
    cmdline
        .split(|&b| b == 0)
        .any(|word| word == b"--pmem" || word.starts_with(b"--pmem="))
}

/// [`HostMemory`] from `/proc`.
pub struct ProcHostMemory;

impl HostMemory for ProcHostMemory {
    fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory> {
        let mut found = Vec::new();
        if let Some(pid) = vm_id
            .and_then(|cid| cid.parse().ok())
            .and_then(crate::launcher::vm_process)
        {
            found.extend(process(pid, HostRole::Vm));
        }
        let Ok(uid) = crate::users::uid_of(user) else {
            return found;
        };
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(name) = std::fs::read_to_string(entry.path().join("comm")) else {
                continue;
            };
            let Some(role) = window_role(name.trim()) else {
                continue;
            };
            let owned = std::fs::read_to_string(entry.path().join("status"))
                .ok()
                .and_then(|status| owner_uid(&status));
            if owned == Some(uid) {
                found.extend(process(pid, role));
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr1_the_vms_resident_and_private_memory_are_read_from_proc() {
        let status =
            "Name:\tdsa-android-100\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t  812345 kB\nVmSwap:\t 5 kB\n";
        assert_eq!(resident_kb(status), Some(812_345));
        // The guest's memory is shared with crosvm's device processes, and is all the VM's.
        assert_eq!(vm_kb(status), Some(812_350));
        assert_eq!(owner_uid(status), Some(1000));
        let rollup = "Rss: 9 kB\nPrivate_Clean:  100 kB\nPrivate_Dirty:  700000 kB\nSwap: 20 kB\n";
        assert_eq!(private_kb(rollup), Some(700_120));
        assert_eq!(window_role("dsa-compositor"), Some(HostRole::WindowManager));
        assert_eq!(window_role("dsa-gfx"), Some(HostRole::Renderer));
        assert_eq!(window_role("bash"), None);
    }

    #[test]
    fn nfr1_the_pages_of_the_disks_crosvm_maps_are_told_apart() {
        let status = "VmRSS:\t  900000 kB\nRssAnon:\t 1000 kB\nRssFile:\t  450000 kB\n\
                      RssShmem:\t  449000 kB\n";
        assert_eq!(mapped_file_kb(status), 450_000);
        assert_eq!(mapped_file_kb("VmRSS:\t 5 kB\n"), 0);
        let mapped = b"crosvm\0run\0--pmem\0path=/var/lib/dsa/direct-mapped/a.img,ro=true\0";
        assert!(maps_disks(mapped));
        assert!(maps_disks(b"crosvm\0run\0--pmem=path=/x.img\0"));
        assert!(!maps_disks(b"crosvm\0run\0--block\0path=/x.img,ro=true\0"));
        // A path that merely names it is not the option.
        assert!(!maps_disks(b"crosvm\0run\0--block\0path=/--pmem.img\0"));
    }

    #[test]
    fn nfr1_this_process_is_measured() {
        let me = process(std::process::id(), HostRole::Vm).unwrap();
        assert!(me.working_set_kb > 0);
    }
}

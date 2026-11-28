//! Emptying the working set of a VM's crosvm process, for the core's idle VM trim
//! ([`dsa_host_core::idle_vm_trim`]).
//!
//! crosvm keeps the guest's memory in a memory file (a memfd) that it maps shared, so the guest's
//! pages are shared memory. `process_madvise` with `MADV_PAGEOUT` (Linux 5.10 and later) on those
//! mappings has the kernel reclaim their pages at once: each goes to swap, and the guest's next
//! touch brings it back. The kernel leaves alone a page another process maps too, which is what
//! crosvm's sandboxed device processes do with the buffers they use, so only what the guest alone
//! touches leaves RAM. Shared memory can leave RAM only for swap, so a host without swap keeps it,
//! and the trim says so. The service runs as root, which has what the call asks: the right to
//! change another process's scheduling (`CAP_SYS_NICE`) and ptrace access to crosvm.
//!
//! What the VM holds in RAM is crosvm's anonymous and shared resident memory (`RssAnon` and
//! `RssShmem`), without the pages of the disk files it maps. What the host wrote to swap is
//! `pswpout` in `/proc/vmstat`: every page swapped out since the host started, for the whole host.
//!
//! The rules that read `/proc` are plain functions, tested on every host; the calls are Linux
//! only.

#[cfg(target_os = "linux")]
pub use system::LinuxVmWorkingSet;

/// The address ranges of the guest's memory in a crosvm process, from its `/proc/<pid>/maps`:
/// every writable shared mapping of a memory file (`/memfd:`).
pub fn guest_memory_ranges(maps: &str) -> Vec<(u64, u64)> {
    maps.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next()?;
            let perms = fields.next()?;
            // Offset, device and inode come before the path.
            let path = fields.nth(3)?;
            let shared_writable = perms.starts_with("rw") && perms.as_bytes().get(3) == Some(&b's');
            if !shared_writable || !path.starts_with("/memfd:") {
                return None;
            }
            let (start, end) = range.split_once('-')?;
            let start = u64::from_str_radix(start, 16).ok()?;
            let end = u64::from_str_radix(end, 16).ok()?;
            (end > start).then_some((start, end))
        })
        .collect()
}

/// The pages swapped out since the host started, from `/proc/vmstat`.
pub fn swapped_out_pages(vmstat: &str) -> Option<u64> {
    vmstat
        .lines()
        .find_map(|line| line.strip_prefix("pswpout "))
        .and_then(|count| count.trim().parse().ok())
}

/// Whether the host has any swap space, from `/proc/swaps` (a header line, then one line for each
/// swap area).
pub fn has_swap(swaps: &str) -> bool {
    swaps.lines().skip(1).any(|line| !line.trim().is_empty())
}

/// The VM's memory in RAM from crosvm's `/proc/<pid>/status`, in KiB: its anonymous and shared
/// resident memory, without the disk files it maps; `VmRSS` where the kernel does not split it.
pub fn resident_kb(status: &str) -> Option<u64> {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
    };
    match (field("RssAnon:"), field("RssShmem:")) {
        (Some(anon), Some(shmem)) => Some(anon + shmem),
        _ => field("VmRSS:"),
    }
}

#[cfg(target_os = "linux")]
mod system {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    use dsa_host_core::idle_vm_trim::{EmptyRefused, VmMemoryLook, VmWorkingSet};

    use super::{guest_memory_ranges, has_swap, resident_kb, swapped_out_pages};

    /// The most ranges one `process_madvise` takes (`UIO_MAXIOV`).
    const MAX_RANGES: usize = 1024;

    /// The crosvm processes of the running VMs, found by their vsock address (the VM ID).
    pub struct LinuxVmWorkingSet;

    impl LinuxVmWorkingSet {
        fn pid(vm_id: &str) -> Option<u32> {
            vm_id.parse().ok().and_then(crate::launcher::vm_process)
        }
    }

    impl VmWorkingSet for LinuxVmWorkingSet {
        fn look(&self, vm_id: &str) -> VmMemoryLook {
            let resident = Self::pid(vm_id)
                .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/status")).ok())
                .and_then(|status| resident_kb(&status));
            let paged_out = std::fs::read_to_string("/proc/vmstat")
                .ok()
                .and_then(|vmstat| swapped_out_pages(&vmstat))
                .map(|pages| pages * page_kb());
            VmMemoryLook {
                resident_kb: resident,
                paged_out_kb: paged_out,
            }
        }

        fn empty(&self, vm_id: &str) -> Result<(), EmptyRefused> {
            let Some(pid) = Self::pid(vm_id) else {
                return Err(EmptyRefused::Failed(
                    "the VM's crosvm process is not running".into(),
                ));
            };
            let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).map_err(|error| {
                EmptyRefused::Failed(format!("crosvm's memory map cannot be read: {error}"))
            })?;
            let ranges = guest_memory_ranges(&maps);
            if ranges.is_empty() {
                return Err(EmptyRefused::Failed(
                    "crosvm maps no memory file for the guest's memory".into(),
                ));
            }
            let swap = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
            if !has_swap(&swap) {
                return Err(EmptyRefused::Failed(
                    "the host has no swap space, and the guest's memory, which crosvm keeps as \
                     shared memory, can leave RAM only for swap"
                        .into(),
                ));
            }
            page_out(pid, &ranges)
        }
    }

    /// The size of a page, in KiB.
    fn page_kb() -> u64 {
        // SAFETY: no preconditions.
        let bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        (u64::try_from(bytes).unwrap_or(4096) / 1024).max(1)
    }

    /// What a failed call says: a kernel without the call cannot do it at all, and anything else
    /// may work on the next try.
    fn refused(call: &str, error: io::Error) -> EmptyRefused {
        if error.raw_os_error() == Some(libc::ENOSYS) {
            EmptyRefused::Unsupported(format!(
                "the kernel has no {call}, which paging out another process's memory needs \
                 (Linux 5.10 or later)"
            ))
        } else {
            EmptyRefused::Failed(format!("{call}: {error}"))
        }
    }

    /// Has the kernel page out the ranges `ranges` of the process `pid` now.
    fn page_out(pid: u32, ranges: &[(u64, u64)]) -> Result<(), EmptyRefused> {
        // SAFETY: a plain system call; the descriptor it returns is owned below.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
        if fd < 0 {
            return Err(refused("pidfd_open", io::Error::last_os_error()));
        }
        // SAFETY: a descriptor the call just opened, closed once when this goes.
        let pidfd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        for chunk in ranges.chunks(MAX_RANGES) {
            let vectors: Vec<libc::iovec> = chunk
                .iter()
                .map(|(start, end)| libc::iovec {
                    iov_base: *start as usize as *mut libc::c_void,
                    iov_len: (end - start) as usize,
                })
                .collect();
            // SAFETY: the vectors name ranges of the other process's address space, which the
            // kernel checks; of this process's memory only the vectors themselves are read.
            let done = unsafe {
                libc::syscall(
                    libc::SYS_process_madvise,
                    pidfd.as_raw_fd(),
                    vectors.as_ptr(),
                    vectors.len(),
                    libc::MADV_PAGEOUT,
                    0u32,
                )
            };
            if done < 0 {
                return Err(refused("process_madvise", io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr1_the_guest_memory_of_crosvm_is_found_in_its_memory_map() {
        let maps = "\
55d0c0000000-55d0c0200000 r-xp 00000000 08:01 1234       /usr/bin/crosvm
7f0000000000-7f0080000000 rw-s 00000000 00:01 5678       /memfd:crosvm_guest (deleted)
7f0080000000-7f0090000000 rw-s 80000000 00:01 5678       /memfd:crosvm_guest (deleted)
7f00a0000000-7f00a1000000 r--s 00000000 00:01 9999       /memfd:readonly (deleted)
7f00b0000000-7f00b1000000 rw-p 00000000 00:00 0          [heap]
7f00c0000000-7f00c4000000 rw-s 00000000 08:01 4321       /var/lib/dsa/direct-mapped/system.img
7f00d0000000-7f00d1000000 rw-p 00000000 00:00 0
";
        assert_eq!(
            guest_memory_ranges(maps),
            [
                (0x7f00_0000_0000, 0x7f00_8000_0000),
                (0x7f00_8000_0000, 0x7f00_9000_0000)
            ]
        );
        assert!(guest_memory_ranges("").is_empty());
    }

    #[test]
    fn nfr1_the_hosts_swap_writes_and_the_vms_memory_are_read_from_proc() {
        let vmstat = "nr_free_pages 12345\npswpin 10\npswpout 2048\npgfault 1\n";
        assert_eq!(swapped_out_pages(vmstat), Some(2048));
        assert_eq!(swapped_out_pages("pswpin 1\n"), None);
        assert!(!has_swap(
            "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n"
        ));
        assert!(has_swap(
            "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0 partition 8388604 0 100\n"
        ));
        let status = "VmRSS:\t  900000 kB\nRssAnon:\t  20000 kB\nRssFile:\t 480000 kB\n\
                      RssShmem:\t 400000 kB\n";
        assert_eq!(resident_kb(status), Some(420_000));
        assert_eq!(resident_kb("VmRSS:\t 5 kB\n"), Some(5));
    }
}

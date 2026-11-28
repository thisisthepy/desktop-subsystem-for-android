//! A second zram device for memory the guest gains after boot.
//!
//! fs_mgr sizes zram0 once, at boot, as half of the memory Android has then ([`ZRAM_PERCENT`]),
//! and the kernel refuses to resize a device that is in use. A guest that grows later (the host
//! adds memory, or its balloon gives memory back) could then fill zram0 with memory still free.
//! So when MemTotal outgrows what zram0 was sized for, the daemon asks for a second device,
//! zram1, sized for the memory added, and asks for it to go again when the guest shrinks back
//! and the device holds nothing.
//!
//! The daemon only decides ([`extra_mib`]) and sets [`ZRAM_EXTRA_PROPERTY`]. init then starts
//! `dsa-channeld --zram-extra` as root in a domain of its own, which reads the property and
//! applies it ([`step`], [`swap_header`]): it adds the device through zram-control, sizes it,
//! writes the swap header and turns it on with a priority below zram0's, so zram0 fills first.
//! A device in use is never cut back while it holds pages: taking it off would read them all
//! back into memory the guest no longer has. The daemon looks again every minute and asks once
//! more when such a device has emptied ([`ask_again`]).

/// The property the daemon sets to the MiB the second zram device should have, 0 for none.
pub const ZRAM_EXTRA_PROPERTY: &str = "vendor.dsa.memory.zram_extra";

/// How large a zram device is for the memory it swaps for, in percent: fstab.dsa sizes zram0 as
/// half of the memory at boot (`zramsize=50%`), and zram1 is half of the memory added.
///
/// A device's size is only a cap on what it holds before compression, but its slot table costs
/// memory for every 4 KiB of that cap whether used or not (16 to 24 B, 8 to 12 MiB for 2 GiB).
/// On the owner's PC an idle guest held 277 MiB in zram of the 1977 MiB zram0 could take. At 100%, as it was, the cap was kept for
/// lmkd: a full zram0 in a guest that had grown made lmkd see swap low and kill cached apps with
/// memory free. lmkd no longer looks at how full the swap is (`ro.lmk.swap_free_low_percentage`
/// is 0 in make-image.py), and memory added gets zram1.
pub const ZRAM_PERCENT: u64 = 50;

/// Growth smaller than this gets no device: a device's own cost and an extra swap area are not
/// worth less memory than this, and small changes in MemTotal (memory the kernel reserves or
/// frees for itself) must not add and remove a device over and over.
pub const MIN_EXTRA_MIB: u64 = 256;

/// The device's size is rounded down to this, so a few MiB of change ask for nothing new.
pub const STEP_MIB: u64 = 64;

/// The MiB the second zram device should have: [`ZRAM_PERCENT`] of the memory the guest has
/// beyond what zram0 was sized for, rounded down to [`STEP_MIB`], or 0 when the memory added is
/// less than [`MIN_EXTRA_MIB`].
///
/// `zram0_bytes` is zram0's `disksize`: fstab.dsa sizes it as [`ZRAM_PERCENT`] of the memory at
/// boot, so it was sized for that memory.
pub fn extra_mib(mem_total_kb: u64, zram0_bytes: u64) -> u64 {
    let covered_mib = zram0_bytes / (1024 * 1024) * 100 / ZRAM_PERCENT;
    let added_mib = (mem_total_kb / 1024).saturating_sub(covered_mib);
    if added_mib < MIN_EXTRA_MIB {
        0
    } else {
        added_mib * ZRAM_PERCENT / 100 / STEP_MIB * STEP_MIB
    }
}

/// What the helper does to the second device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Leave it as it is.
    Leave,
    /// Make it (taking an old one off first) with this many MiB.
    Make(u64),
    /// Take it off and free it.
    Remove,
}

/// A device holding less than this, before compression, counts as empty: taking it off reads
/// that little back into memory, and keeping it costs its slot table (some 3 MiB for 512 MiB)
/// for as long as the guest runs. On the owner's PC a zram1 kept for "0 MiB of swapped pages",
/// under a MiB, stayed for the rest of the boot.
pub const EMPTY_BELOW_BYTES: u64 = 1024 * 1024;

/// How often the daemon looks whether a second device it was told to cut back is empty now.
pub const REASK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The step for a target of `target_mib`, with the device now `current_mib` large (0 when it is
/// not in use) and holding `stored_bytes` of swapped pages.
///
/// A larger target remakes the device: taking it off reads what it holds back into memory, which
/// the guest has just gained. A smaller one, or none, takes it off only once it is empty (it holds
/// less than [`EMPTY_BELOW_BYTES`]).
pub fn step(target_mib: u64, current_mib: u64, stored_bytes: u64) -> Step {
    if target_mib == current_mib {
        Step::Leave
    } else if target_mib > current_mib {
        Step::Make(target_mib)
    } else if stored_bytes >= EMPTY_BELOW_BYTES {
        Step::Leave
    } else if target_mib == 0 {
        Step::Remove
    } else {
        Step::Make(target_mib)
    }
}

/// Whether the daemon asks again for a target of `target_mib` it already asked for, with the
/// device `current_mib` large (its `disksize`, 0 when it is gone) and holding `stored_bytes`.
///
/// The helper leaves a device larger than the target while it holds pages, and the daemon asks
/// only when the target changes, so such a device stayed until the guest stopped. It is asked for
/// again once the step would now cut it back, at most every [`REASK_INTERVAL`].
pub fn ask_again(target_mib: u64, current_mib: u64, stored_bytes: u64) -> bool {
    current_mib > target_mib && step(target_mib, current_mib, stored_bytes) != Step::Leave
}

/// The first page of a swap area of `size_bytes` with pages of `page_size` bytes: what mkswap
/// writes. Version 1, no bad pages, `SWAPSPACE2` in the page's last ten bytes.
pub fn swap_header(page_size: usize, size_bytes: u64) -> Vec<u8> {
    let mut page = vec![0u8; page_size];
    let last_page = (size_bytes / page_size as u64).saturating_sub(1) as u32;
    // union swap_header: 1024 bytes of boot block, then version, last_page, nr_badpages.
    page[1024..1028].copy_from_slice(&1u32.to_ne_bytes());
    page[1028..1032].copy_from_slice(&last_page.to_ne_bytes());
    page[page_size - 10..].copy_from_slice(b"SWAPSPACE2");
    page
}

/// The first number of zram's `mm_stat`: the bytes of pages it holds, before compression.
pub fn stored_bytes(mm_stat: &str) -> u64 {
    mm_stat
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Whether `/proc/swaps` lists `device` as in use.
pub fn swapped_on(proc_swaps: &str, device: &str) -> bool {
    proc_swaps
        .lines()
        .skip(1)
        .any(|line| line.split_whitespace().next() == Some(device))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod apply {
    use super::*;
    use std::ffi::CString;
    use std::fs;
    use std::io::Write;

    const DEVICE: &str = "/dev/block/zram1";
    const SYSFS: &str = "/sys/devices/virtual/block/zram1";
    const HOT_ADD: &str = "/sys/class/zram-control/hot_add";

    fn swapoff() -> Result<(), String> {
        let path = CString::new(DEVICE).unwrap();
        // SAFETY: a NUL-terminated path that outlives the call.
        if unsafe { libc::swapoff(path.as_ptr()) } != 0 {
            return Err(format!(
                "swapoff {DEVICE}: {}",
                std::io::Error::last_os_error()
            ));
        }
        fs::write(format!("{SYSFS}/reset"), "1").map_err(|e| format!("{SYSFS}/reset: {e}"))
    }

    fn make(mib: u64) -> Result<(), String> {
        // zram-control adds the next free device; only zram1 has a policy label, so it adds
        // only while zram1 is missing.
        if fs::metadata(SYSFS).is_err() {
            let id = fs::read_to_string(HOT_ADD).map_err(|e| format!("{HOT_ADD}: {e}"))?;
            if id.trim() != "1" {
                return Err(format!(
                    "zram-control added zram{} instead of zram1",
                    id.trim()
                ));
            }
        }
        let bytes = mib * 1024 * 1024;
        fs::write(format!("{SYSFS}/disksize"), bytes.to_string())
            .map_err(|e| format!("{SYSFS}/disksize: {e}"))?;
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let mut device = fs::OpenOptions::new()
            .write(true)
            .open(DEVICE)
            .map_err(|e| format!("{DEVICE}: {e}"))?;
        device
            .write_all(&swap_header(page, bytes))
            .and_then(|_| device.sync_all())
            .map_err(|e| format!("writing the swap header to {DEVICE}: {e}"))?;
        drop(device);
        let path = CString::new(DEVICE).unwrap();
        // No priority flag: the kernel gives the area one below every area already on, so
        // zram0 fills first.
        // SAFETY: a NUL-terminated path that outlives the call.
        if unsafe { libc::swapon(path.as_ptr(), 0) } != 0 {
            return Err(format!(
                "swapon {DEVICE}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Applies `target_mib`, writing what it did through `say`, and returns the exit status.
    pub fn run(target_mib: u64, say: &dyn Fn(&str)) -> i32 {
        let on = fs::read_to_string("/proc/swaps")
            .map(|text| swapped_on(&text, DEVICE))
            .unwrap_or(false);
        let current = if on {
            fs::read_to_string(format!("{SYSFS}/disksize"))
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0)
                / (1024 * 1024)
        } else {
            0
        };
        let stored = fs::read_to_string(format!("{SYSFS}/mm_stat"))
            .map(|text| stored_bytes(&text))
            .unwrap_or(0);
        let result = match step(target_mib, current, stored) {
            Step::Leave => {
                if target_mib < current {
                    say(&format!(
                        "zram1 keeps {current} MiB: it still holds {} MiB of swapped pages",
                        stored / (1024 * 1024)
                    ));
                }
                Ok(())
            }
            Step::Remove => swapoff().map(|_| say("zram1 is off: the memory it was for is gone")),
            Step::Make(mib) => (if on { swapoff() } else { Ok(()) })
                .and_then(|_| make(mib))
                .map(|_| {
                    say(&format!(
                        "zram1 swaps {mib} MiB for the memory added since boot"
                    ))
                }),
        };
        match result {
            Ok(()) => 0,
            Err(why) => {
                say(&format!("zram1 could not change: {why}"));
                1
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub use apply::run;

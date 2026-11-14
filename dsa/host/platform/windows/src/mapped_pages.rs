//! Giving back the pages of the disks a VM maps directly (the host's direct-mapped mode) that
//! its memory process keeps in its working set.
//!
//! Hyper-V maps a virtual persistent memory device's file into the VM's memory process
//! (`vmmem`) as a read-only view of the file: a page the guest reads is a page of Windows'
//! file cache, and it joins the process's working set as a shared page. Nothing takes it out
//! again but Windows' working set trimming, which runs only when Windows is short of memory,
//! and the guest cannot hand it back: its own reclaim and free page reporting cover its RAM,
//! not the device. So every page Android read once (at boot it reads most of the framework,
//! its apps and their libraries) stayed in `vmmem`'s working set: 700 to 850 MiB more, in one
//! measurement, than with the disks on SCSI.
//!
//! The service takes them out of the working set itself, with `VirtualUnlockEx` on the views
//! of those disk files only. On a range that is not locked it removes the pages from the
//! working set (and answers `ERROR_NOT_LOCKED`, as `VirtualUnlock` does). The pages are clean,
//! so they go to the standby list: they stay cached, nothing is written, and the guest's next
//! touch brings a page back from there without reading the disk (a soft fault). The guest's
//! own memory is the process's private memory, which is never touched here.

use std::path::Path;

/// The name Windows gives a file in the form `GetMappedFileNameW` reports it
/// (`\Device\HarddiskVolume3\ProgramData\DSA\x.vhd`), from its path (`C:\ProgramData\DSA\x.vhd`
/// or `\\?\C:\...`) and the device the path's drive letter stands for (`QueryDosDeviceW` of
/// `C:`). `None` for a path without a drive letter.
pub fn device_path(path: &str, drive_device: &str) -> Option<String> {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let rest = path[2..].replace('/', "\\");
    let rest = rest.trim_end_matches('\\');
    Some(format!("{}{rest}", drive_device.trim_end_matches('\\')))
}

/// Whether the file `mapped` (as `GetMappedFileNameW` names it) is `target` or lies inside
/// it, both in device form. Case does not matter, as on NTFS, and a folder matches only at a
/// separator, so `...\direct-mapped` does not take in `...\direct-mapped-old\x.vhd`.
pub fn is_within(mapped: &str, target: &str) -> bool {
    let (mapped, target) = (mapped.to_ascii_lowercase(), target.to_ascii_lowercase());
    match mapped.strip_prefix(&target) {
        Some(rest) => rest.is_empty() || rest.starts_with('\\'),
        None => false,
    }
}

/// The drive letter part of `path` (`C:`), for `QueryDosDeviceW`.
pub fn drive_of(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let bytes = text.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| text[..2].to_ascii_uppercase())
}

/// What one give-back did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GaveBack {
    /// Views of the disk files found in the process.
    pub views: u32,
    /// Their size, in KiB.
    pub view_kb: u64,
}

#[cfg(windows)]
pub use system::give_back;

#[cfg(windows)]
mod system {
    use std::io;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_LOCKED, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
    use windows_sys::Win32::System::Memory::{
        VirtualQueryEx, VirtualUnlockEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_MAPPED,
    };
    use windows_sys::Win32::System::ProcessStatus::K32GetMappedFileNameW;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
    };

    use super::{device_path, drive_of, is_within, GaveBack};
    use crate::pipe::OwnedHandle;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `target` in the form `GetMappedFileNameW` names files.
    fn device_form(target: &Path) -> io::Result<String> {
        let drive = drive_of(target).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not on a drive letter", target.display()),
            )
        })?;
        let mut device = [0u16; 512];
        // SAFETY: a NUL-terminated name and a buffer of the size given.
        let len = unsafe {
            QueryDosDeviceW(
                wide(&drive).as_ptr(),
                device.as_mut_ptr(),
                device.len() as u32,
            )
        };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        // The answer is a list of NUL-terminated names; the first is the drive's device.
        let end = device.iter().position(|&c| c == 0).unwrap_or(device.len());
        let device = String::from_utf16_lossy(&device[..end]);
        device_path(&target.to_string_lossy(), &device).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} cannot be named as a device path", target.display()),
            )
        })
    }

    /// Takes the pages of every view of the files `targets` names (a file, or every file in a
    /// folder) out of the working set of the process `pid`, a VM's memory process. Nothing
    /// else of the process is touched.
    pub fn give_back(pid: u32, targets: &[&Path]) -> io::Result<GaveBack> {
        let targets: Vec<String> = targets
            .iter()
            .map(|t| device_form(t))
            .collect::<io::Result<_>>()?;
        // SAFETY: opening a process by ID to read its address space's layout and change its
        // working set; closed by OwnedHandle.
        let handle: HANDLE =
            unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle(handle);
        let mut done = GaveBack::default();
        let mut address = 0usize;
        loop {
            // SAFETY: plain data, filled by the query.
            let mut region: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: the buffer's size is passed with it.
            let got = unsafe {
                VirtualQueryEx(
                    process.0,
                    address as *const _,
                    &mut region,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if got == 0 || region.RegionSize == 0 {
                break;
            }
            let base = region.BaseAddress as usize;
            if region.State == MEM_COMMIT && region.Type == MEM_MAPPED {
                let mut name = [0u16; 1024];
                // SAFETY: a buffer of the size given.
                let len = unsafe {
                    K32GetMappedFileNameW(
                        process.0,
                        region.BaseAddress,
                        name.as_mut_ptr(),
                        name.len() as u32,
                    )
                };
                let name = String::from_utf16_lossy(&name[..len as usize]);
                if len > 0 && targets.iter().any(|t| is_within(&name, t)) {
                    // SAFETY: a range of the process's own address space, as the query gave it.
                    let unlocked = unsafe {
                        VirtualUnlockEx(process.0, region.BaseAddress, region.RegionSize)
                    };
                    // A range nobody locked answers ERROR_NOT_LOCKED, after its pages left the
                    // working set: the case this is for.
                    // SAFETY: reads the calling thread's last error.
                    let error = unsafe { GetLastError() };
                    if unlocked == 0 && error != ERROR_NOT_LOCKED {
                        return Err(io::Error::from_raw_os_error(error as i32));
                    }
                    done.views += 1;
                    done.view_kb += region.RegionSize as u64 / 1024;
                }
            }
            match base.checked_add(region.RegionSize) {
                Some(next) => address = next,
                None => break,
            }
        }
        Ok(done)
    }
}

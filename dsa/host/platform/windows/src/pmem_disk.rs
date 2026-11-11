//! The disks of the host's direct-mapped mode: the read-only system disk and the add-on disk
//! attached as Hyper-V virtual persistent memory (see `dsa_host_core::direct_mapped`).
//!
//! Virtual persistent memory maps its file into the VM as it is, so it takes a flat image: a
//! fixed VHD (HCS's `Vhd1`), the disk's bytes followed by a 512-byte footer. The add-on disk is
//! one already. The image's system disk is a dynamic VHDX, which holds only what was written,
//! so the service makes a fixed VHD of it once, beside it in the image's folder
//! (`system.pmem.vhd`), with Windows' own virtual disk service, and uses that copy while the
//! image's VHDX stays as it was published, with its checksum. The copy is made again when the
//! VHDX changes, and goes with the image's folder.
//!
//! Integrity is the host's in this mode: the guest cannot run dm-verity over a mapped
//! partition. The copy is made by the service, which runs as SYSTEM, from the VHDX it checked
//! against the signed release list, into the image store only SYSTEM and administrators may
//! write, and the VM attaches it read only.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::vm::PERSISTENT_MEMORY_SECTION;

/// The fixed VHD made from the system disk `system_image`, in the same folder.
pub fn system_disk_beside(system_image: &Path) -> PathBuf {
    system_image.with_file_name("system.pmem.vhd")
}

/// Where the copy records which VHDX it was made from (its size and modification time).
fn source_record(pmem: &Path) -> PathBuf {
    pmem.with_file_name("system.pmem.source")
}

const FOOTER_BYTES: u64 = 512;

/// The size of the disk in the fixed VHD at `path`, without its footer: the footer's cookie
/// (`conectix`) and disk type (fixed) are checked, and the size it gives must be the file's.
pub fn fixed_vhd_disk_bytes(path: &Path) -> io::Result<u64> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length < FOOTER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too short to be a VHD", path.display()),
        ));
    }
    file.seek(SeekFrom::Start(length - FOOTER_BYTES))?;
    let mut footer = [0u8; FOOTER_BYTES as usize];
    file.read_exact(&mut footer)?;
    let size = u64::from_be_bytes(footer[48..56].try_into().expect("eight bytes"));
    let kind = u32::from_be_bytes(footer[60..64].try_into().expect("four bytes"));
    if &footer[0..8] != b"conectix" || kind != 2 || size != length - FOOTER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a fixed VHD", path.display()),
        ));
    }
    Ok(size)
}

/// What the copy's record says of `source`: its size and modification time, in one line.
fn describe(source: &Path) -> io::Result<String> {
    let meta = fs::metadata(source)?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("{} {modified}\n", meta.len()))
}

/// The copy of `system_image` made earlier, with its disk size, when it is still the copy of
/// that VHDX as it is now and a whole number of persistent memory sections.
pub fn current_copy(system_image: &Path) -> Option<(PathBuf, u64)> {
    let pmem = system_disk_beside(system_image);
    let recorded = fs::read_to_string(source_record(&pmem)).ok()?;
    if recorded != describe(system_image).ok()? {
        return None;
    }
    let bytes = fixed_vhd_disk_bytes(&pmem).ok()?;
    (bytes > 0 && bytes % PERSISTENT_MEMORY_SECTION == 0).then_some((pmem, bytes))
}

/// Records that `pmem` was made from `system_image` as it is now.
#[cfg_attr(not(windows), allow(dead_code))]
fn record_source(system_image: &Path, pmem: &Path) -> io::Result<()> {
    fs::write(source_record(pmem), describe(system_image)?)
}

/// The fixed VHD of `system_image` for the direct-mapped mode, made now unless the one made
/// earlier is still current; with its disk size.
#[cfg(windows)]
pub fn prepare_system_disk(system_image: &Path) -> Result<(PathBuf, u64), String> {
    if let Some(current) = current_copy(system_image) {
        return Ok(current);
    }
    let pmem = system_disk_beside(system_image);
    // Windows' virtual disk service names the format by the extension, so the partial copy
    // keeps it.
    let partial = pmem.with_file_name("system.pmem.part.vhd");
    let _ = fs::remove_file(&partial);
    let _ = fs::remove_file(source_record(&pmem));
    let started = std::time::Instant::now();
    vhd::copy_to_fixed_vhd(system_image, &partial)?;
    let bytes = fixed_vhd_disk_bytes(&partial).map_err(|e| e.to_string())?;
    if bytes == 0 || bytes % PERSISTENT_MEMORY_SECTION != 0 {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "the system disk {} is {bytes} bytes, not a whole number of 2 MiB, so the guest \
             could not map it; an image made for the direct-mapped mode is",
            system_image.display()
        ));
    }
    let _ = fs::remove_file(&pmem);
    fs::rename(&partial, &pmem).map_err(|e| format!("placing {}: {e}", pmem.display()))?;
    record_source(system_image, &pmem).map_err(|e| format!("recording {}: {e}", pmem.display()))?;
    eprintln!(
        "direct-mapped: made {} ({} MiB) from {} in {:.1} s",
        pmem.display(),
        bytes >> 20,
        system_image.display(),
        started.elapsed().as_secs_f64()
    );
    Ok((pmem, bytes))
}

#[cfg(windows)]
mod vhd {
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Storage::Vhd::{
        CreateVirtualDisk, CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION,
        CREATE_VIRTUAL_DISK_PARAMETERS, CREATE_VIRTUAL_DISK_VERSION_2, VIRTUAL_DISK_ACCESS_NONE,
        VIRTUAL_STORAGE_TYPE, VIRTUAL_STORAGE_TYPE_DEVICE_VHD, VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
        VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Makes `target`, a fixed VHD holding the disk of the VHDX `source`, at its size.
    pub fn copy_to_fixed_vhd(source: &Path, target: &Path) -> Result<(), String> {
        let storage = VIRTUAL_STORAGE_TYPE {
            DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHD,
            VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
        };
        let source_path = wide(source);
        let target_path = wide(target);
        let mut parameters = CREATE_VIRTUAL_DISK_PARAMETERS {
            Version: CREATE_VIRTUAL_DISK_VERSION_2,
            ..Default::default()
        };
        // Writing union fields is safe; they are the version 2 members, matching `Version`. A
        // size of 0 takes the source's.
        parameters.Anonymous.Version2.MaximumSize = 0;
        parameters.Anonymous.Version2.SourcePath = source_path.as_ptr();
        parameters.Anonymous.Version2.SourceVirtualStorageType = VIRTUAL_STORAGE_TYPE {
            DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
            VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: valid parameters whose paths outlive the call, and an out pointer; the handle
        // is closed below. A null security descriptor makes the file inherit the folder's
        // access list. Without an OVERLAPPED the call returns once the copy is whole.
        let code = unsafe {
            CreateVirtualDisk(
                &storage,
                target_path.as_ptr(),
                VIRTUAL_DISK_ACCESS_NONE,
                std::ptr::null_mut(),
                CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION,
                0,
                &parameters,
                std::ptr::null(),
                &mut handle,
            )
        };
        if code != ERROR_SUCCESS {
            let _ = std::fs::remove_file(target);
            return Err(format!(
                "making the fixed VHD {} from {}: {}",
                target.display(),
                source.display(),
                std::io::Error::from_raw_os_error(code as i32)
            ));
        }
        // SAFETY: the handle CreateVirtualDisk returned, closed once.
        unsafe { CloseHandle(handle) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-pmem-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixed_vhd(bytes: u64) -> Vec<u8> {
        let mut data = vec![0u8; bytes as usize + 512];
        let footer = &mut data[bytes as usize..];
        footer[0..8].copy_from_slice(b"conectix");
        footer[48..56].copy_from_slice(&bytes.to_be_bytes());
        footer[60..64].copy_from_slice(&2u32.to_be_bytes());
        data
    }

    #[test]
    fn nfr1_the_persistent_memory_copy_is_used_while_it_is_the_copy_of_the_system_disk() {
        let dir = scratch("copy");
        let system = dir.join("system.vhdx");
        fs::write(&system, b"published system disk").unwrap();
        let pmem = system_disk_beside(&system);
        assert_eq!(pmem, dir.join("system.pmem.vhd"));
        assert_eq!(current_copy(&system), None, "no copy yet");
        fs::write(&pmem, fixed_vhd(4 << 20)).unwrap();
        assert_eq!(
            current_copy(&system),
            None,
            "a copy with no record of its source"
        );
        record_source(&system, &pmem).unwrap();
        assert_eq!(current_copy(&system), Some((pmem.clone(), 4 << 20)));
        // A changed VHDX needs a new copy.
        fs::write(&system, b"a newer published system disk").unwrap();
        assert_eq!(current_copy(&system), None);
        // So does one the guest could not map.
        fs::write(&pmem, fixed_vhd((4 << 20) + 4096)).unwrap();
        record_source(&system, &pmem).unwrap();
        assert_eq!(current_copy(&system), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr1_only_a_fixed_vhd_can_be_mapped() {
        let dir = scratch("vhd");
        let path = dir.join("addons.vhd");
        fs::write(&path, fixed_vhd(2 << 20)).unwrap();
        assert_eq!(fixed_vhd_disk_bytes(&path).unwrap(), 2 << 20);
        let mut dynamic = fixed_vhd(2 << 20);
        let at = dynamic.len() - 512 + 60;
        dynamic[at..at + 4].copy_from_slice(&3u32.to_be_bytes());
        fs::write(&path, dynamic).unwrap();
        assert!(fixed_vhd_disk_bytes(&path).is_err());
        fs::write(&path, vec![0u8; 100]).unwrap();
        assert!(fixed_vhd_disk_bytes(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}

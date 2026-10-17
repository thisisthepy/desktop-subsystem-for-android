//! Each user's Android disk, and the folder it is in.
//!
//! The service creates both the first time a user asks for Android. A disk in the user's
//! own profile is protected by the profile, which admits only them, SYSTEM and
//! administrators, and the VM is given access to the disk file alone when it starts. A
//! disk in a shared `data_dir` gets a folder whose access list is protected (it inherits
//! nothing), so other users cannot read someone's Android data, and which lets Hyper-V's
//! virtual machine processes in.

use std::io;
use std::path::Path;

use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::Storage::Vhd::{
    CreateVirtualDisk, CREATE_VIRTUAL_DISK_FLAG_NONE, CREATE_VIRTUAL_DISK_PARAMETERS,
    CREATE_VIRTUAL_DISK_VERSION_2, VIRTUAL_DISK_ACCESS_NONE, VIRTUAL_STORAGE_TYPE,
    VIRTUAL_STORAGE_TYPE_DEVICE_VHDX, VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
};

/// SYSTEM, Administrators and Hyper-V's virtual machines (`S-1-5-83-0`), inherited by
/// everything in the folder, and nobody else.
pub const USER_FOLDER_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-83-0)";

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn win32(code: u32, what: &str) -> Result<(), String> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            io::Error::from_raw_os_error(code as i32)
        ))
    }
}

/// Makes sure `disk` exists. A new disk is a dynamic VHDX that may grow to `size_gb`; an
/// existing one is left as it is.
pub fn prepare_user_data_disk(disk: &Path, size_gb: u32) -> Result<(), String> {
    prepare_user_disk(disk, size_gb, None)
}

/// The partitions of a user's Android disk that hold Android itself. They come from the
/// image; every other partition holds the user's data.
pub const ANDROID_PARTITIONS: [&str; 5] = [
    "super",
    "vbmeta_a",
    "vbmeta_system_a",
    "vbmeta_system_dlkm_a",
    "vbmeta_vendor_dlkm_a",
];

/// Makes sure the user's disk exists. A new disk is a copy of `template` (an image's `disk.vhdx`, holding Android and empty data
/// partitions) or, without one, an empty dynamic VHDX that may grow to `size_gb`.
pub fn prepare_user_disk(disk: &Path, size_gb: u32, template: Option<&Path>) -> Result<(), String> {
    let folder = disk
        .parent()
        .ok_or_else(|| format!("{} has no folder", disk.display()))?;
    std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    if disk.exists() {
        return Ok(());
    }
    match template {
        Some(template) => {
            let partial = disk.with_extension("vhdx.new");
            std::fs::copy(template, &partial).map_err(|e| {
                format!(
                    "copying {} to {}: {e}",
                    template.display(),
                    partial.display()
                )
            })?;
            std::fs::rename(&partial, disk).map_err(|e| format!("placing {}: {e}", disk.display()))
        }
        None => create_vhdx(disk, u64::from(size_gb) << 30),
    }
}

/// Creates the folder of a user's disk, without the disk. With `protect`, for a folder in a
/// shared `data_dir`, its access list becomes [`USER_FOLDER_SDDL`]; a folder in the user's
/// profile keeps what it inherits from the profile.
pub fn prepare_user_disk_folder(disk: &Path, protect: bool) -> Result<(), String> {
    let folder = disk
        .parent()
        .ok_or_else(|| format!("{} has no folder", disk.display()))?;
    std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    if protect {
        protect_folder(folder)?;
    }
    Ok(())
}

/// The image version a user's disk holds Android from, recorded beside it.
fn version_file(disk: &Path) -> std::path::PathBuf {
    disk.with_extension("image-version")
}

/// The image version the user's disk holds Android from, if known.
pub fn disk_image_version(disk: &Path) -> Option<String> {
    std::fs::read_to_string(version_file(disk))
        .ok()
        .map(|v| v.trim().to_string())
}

pub fn record_disk_image_version(disk: &Path, version: &str) -> Result<(), String> {
    std::fs::write(version_file(disk), version)
        .map_err(|e| format!("recording the image version of {}: {e}", disk.display()))
}

/// Brings the user's disk to `version`: a new disk is copied from `template`; a disk that
/// holds Android from another version gets the template's Android partitions, and keeps
/// its misc, metadata and userdata partitions, which hold the user's data.
pub fn bring_disk_to(disk: &Path, template: &Path, version: &str) -> Result<(), String> {
    if !disk.exists() {
        prepare_user_disk(disk, 0, Some(template))?;
        return record_disk_image_version(disk, version);
    }
    if disk_image_version(disk).as_deref() == Some(version) {
        return Ok(());
    }
    let from = AttachedDisk::attach(template, true)?;
    let to = AttachedDisk::attach(disk, false)?;
    let mut source = from.open(false)?;
    let mut target = to.open(true)?;
    let names: Vec<&str> = {
        let have = crate::gpt::partitions(&mut source)
            .map_err(|e| format!("reading {}: {e}", template.display()))?;
        ANDROID_PARTITIONS
            .iter()
            .copied()
            .filter(|n| have.iter().any(|p| p.name == *n))
            .collect()
    };
    crate::gpt::copy_partitions(&mut source, &mut target, &names)
        .map_err(|e| format!("writing Android {version} into {}: {e}", disk.display()))?;
    drop(target);
    drop(source);
    drop(to);
    drop(from);
    record_disk_image_version(disk, version)
}

/// A VHDX attached to the host as a disk without a drive letter, detached on drop.
pub struct AttachedDisk {
    handle: HANDLE,
    physical: String,
}

impl AttachedDisk {
    pub fn attach(path: &Path, read_only: bool) -> Result<AttachedDisk, String> {
        use windows_sys::Win32::Storage::Vhd::{
            AttachVirtualDisk, GetVirtualDiskPhysicalPath, OpenVirtualDisk,
            ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER, ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY,
            ATTACH_VIRTUAL_DISK_PARAMETERS, ATTACH_VIRTUAL_DISK_VERSION_1,
            OPEN_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_PARAMETERS, OPEN_VIRTUAL_DISK_VERSION_1,
            VIRTUAL_DISK_ACCESS_ALL,
        };
        let storage = VIRTUAL_STORAGE_TYPE {
            DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
            VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
        };
        let mut open = OPEN_VIRTUAL_DISK_PARAMETERS {
            Version: OPEN_VIRTUAL_DISK_VERSION_1,
            ..Default::default()
        };
        open.Anonymous.Version1.RWDepth = 1;
        let wide_path = wide(path);
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: valid parameters and out pointer; the handle is closed on drop.
        let code = unsafe {
            OpenVirtualDisk(
                &storage,
                wide_path.as_ptr(),
                VIRTUAL_DISK_ACCESS_ALL,
                OPEN_VIRTUAL_DISK_FLAG_NONE,
                &open,
                &mut handle,
            )
        };
        win32(code, &format!("opening {}", path.display()))?;
        let mut disk = AttachedDisk {
            handle,
            physical: String::new(),
        };
        let attach = ATTACH_VIRTUAL_DISK_PARAMETERS {
            Version: ATTACH_VIRTUAL_DISK_VERSION_1,
            ..Default::default()
        };
        let mut flags = ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER;
        if read_only {
            flags |= ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY;
        }
        // SAFETY: an open virtual disk and valid parameters.
        let code = unsafe {
            AttachVirtualDisk(
                handle,
                std::ptr::null_mut(),
                flags,
                0,
                &attach,
                std::ptr::null(),
            )
        };
        win32(code, &format!("attaching {}", path.display()))?;
        let mut buffer = vec![0u16; 260];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: a buffer of `size` bytes.
        let code = unsafe { GetVirtualDiskPhysicalPath(handle, &mut size, buffer.as_mut_ptr()) };
        win32(code, &format!("finding the disk of {}", path.display()))?;
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        disk.physical = String::from_utf16_lossy(&buffer[..len]);
        Ok(disk)
    }

    /// Opens the attached disk's raw device.
    pub fn open(&self, write: bool) -> Result<std::fs::File, String> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(write)
            .open(&self.physical)
            .map_err(|e| format!("opening {}: {e}", self.physical))
    }
}

impl Drop for AttachedDisk {
    fn drop(&mut self) {
        use windows_sys::Win32::Storage::Vhd::{DetachVirtualDisk, DETACH_VIRTUAL_DISK_FLAG_NONE};
        // SAFETY: an open virtual disk handle, detached (if attached) and closed once.
        unsafe {
            if !self.physical.is_empty() {
                DetachVirtualDisk(self.handle, DETACH_VIRTUAL_DISK_FLAG_NONE, 0);
            }
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

// SAFETY: a virtual disk handle may be used and closed from any thread.
unsafe impl Send for AttachedDisk {}

/// Replaces the folder's access list with [`USER_FOLDER_SDDL`], applied to what it holds.
fn protect_folder(folder: &Path) -> Result<(), String> {
    let sddl: Vec<u16> = USER_FOLDER_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated SDDL string; the descriptor is freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!(
            "parsing the folder's access list: {}",
            io::Error::last_os_error()
        ));
    }
    let result = (|| {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: a valid descriptor; the DACL points into it.
        if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
            == 0
        {
            return Err(format!(
                "reading the folder's access list: {}",
                io::Error::last_os_error()
            ));
        }
        let path = wide(folder);
        // SAFETY: a NUL-terminated path and a DACL that outlives the call.
        let code = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        win32(code, &format!("restricting access to {}", folder.display()))
    })();
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(descriptor as HLOCAL) };
    result
}

fn create_vhdx(disk: &Path, max_bytes: u64) -> Result<(), String> {
    let storage = VIRTUAL_STORAGE_TYPE {
        DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
        VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    };
    let mut parameters = CREATE_VIRTUAL_DISK_PARAMETERS {
        Version: CREATE_VIRTUAL_DISK_VERSION_2,
        ..Default::default()
    };
    // Writing a union field is safe; it is the version 2 member, matching `Version`.
    parameters.Anonymous.Version2.MaximumSize = max_bytes;
    let path = wide(disk);
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: valid parameters and out pointer; the handle is closed below. A null
    // security descriptor makes the file inherit the folder's access list.
    let code = unsafe {
        CreateVirtualDisk(
            &storage,
            path.as_ptr(),
            VIRTUAL_DISK_ACCESS_NONE,
            std::ptr::null_mut(),
            CREATE_VIRTUAL_DISK_FLAG_NONE,
            0,
            &parameters,
            std::ptr::null(),
            &mut handle,
        )
    };
    win32(code, &format!("creating the data disk {}", disk.display()))?;
    // SAFETY: the handle CreateVirtualDisk returned, closed once.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    Ok(())
}

/// The access list of `path` as SDDL, for checks and diagnostics.
pub fn access_list(path: &Path) -> Result<String, String> {
    let wide_path = wide(path);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated path; the descriptor is freed below.
    let code = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    win32(
        code,
        &format!("reading the access list of {}", path.display()),
    )?;
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: a valid descriptor; the string is freed below.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            std::ptr::null_mut(),
        )
    };
    let result = if ok == 0 {
        Err(format!(
            "formatting an access list: {}",
            io::Error::last_os_error()
        ))
    } else {
        // SAFETY: a NUL-terminated string from LocalAlloc, freed once.
        Ok(unsafe {
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as HLOCAL);
            value
        })
    };
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe { LocalFree(descriptor as HLOCAL) };
    result
}

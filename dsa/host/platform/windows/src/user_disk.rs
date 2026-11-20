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

use dsa_host_core::disk::DiskBackend;

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

/// The access list of a user's backup folder: the user, SYSTEM and Administrators, inherited
/// by everything in it. The service writes the backup as SYSTEM, and the user still owns what
/// is there: they can open, copy and remove it.
pub fn backup_folder_sddl(sid: &str) -> String {
    format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
}

/// Makes sure the backup folder of the user with SID `sid` exists. A folder made here gets
/// [`backup_folder_sddl`], so it is the user's whatever their profile folder passes down; a
/// folder that is there already (one the user made or set up) keeps its own. A link is
/// refused, so the service never changes the access list of another folder.
pub fn prepare_backup_folder(folder: &Path, sid: &str) -> Result<(), String> {
    if std::fs::symlink_metadata(folder).is_ok() {
        crate::safe_fs::GuardedDir::open(folder)?;
        return Ok(());
    }
    if let Some(parent) = folder
        .parent()
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
    {
        crate::safe_fs::GuardedDir::open(parent)?;
    }
    std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    let _guard = crate::safe_fs::GuardedDir::open(folder)?;
    protect_folder_as(folder, &backup_folder_sddl(sid))
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

/// Brings the user's disk to `version`, from the image's disk `template`: the rules are the
/// host core's ([`dsa_host_core::disk::bring_disk_to`]), run on attached VHDX disks.
pub fn bring_disk_to(disk: &Path, template: &Path, version: &str) -> Result<(), String> {
    dsa_host_core::disk::bring_disk_to(&VhdxBackend, disk, template, version)
}

/// Whether `path` is a VHDX file, which starts with its signature.
fn is_vhdx(path: &Path) -> Result<bool, String> {
    dsa_host_core::vhdx::is_vhdx(path).map_err(|e| format!("opening {}: {e}", path.display()))
}

/// The user's disks as attached VHDX files: the disk core's backend on Windows.
pub struct VhdxBackend;

/// An attached disk's raw device, which detaches the disk when it is dropped.
pub struct AttachedDevice {
    // Dropped first: the device is closed before the disk is detached.
    file: std::fs::File,
    _attached: AttachedDisk,
}

impl io::Read for AttachedDevice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl io::Write for AttachedDevice {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl io::Seek for AttachedDevice {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

impl DiskBackend for VhdxBackend {
    type Device = AttachedDevice;

    fn open(&self, disk: &Path, write: bool) -> Result<AttachedDevice, String> {
        let attached = AttachedDisk::attach(disk, !write)?;
        let file = attached.open(write)?;
        Ok(AttachedDevice {
            file,
            _attached: attached,
        })
    }

    fn create(&self, disk: &Path, template: Option<&Path>, size_gb: u32) -> Result<(), String> {
        prepare_user_disk(disk, size_gb, template)
    }

    fn write_beside(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }

    fn has_partitions(&self, path: &Path) -> Result<bool, String> {
        is_vhdx(path)
    }
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
    protect_folder_as(folder, USER_FOLDER_SDDL)
}

/// The machine's `tmp` folder: SYSTEM and Administrators write; Hyper-V's virtual machines may
/// only look into the folder itself, and each VM is let into the one file it boots from.
pub const MACHINE_TMP_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200a9;;;S-1-5-83-0)";

/// Replaces the folder's access list with `sddl`, protected from what it would inherit.
pub fn protect_folder_as(folder: &Path, sddl: &str) -> Result<(), String> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
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

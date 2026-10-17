//! Each user's data disk: a folder only the system and VMs may open, and a growing VHDX.
//!
//! The service creates both the first time a user asks for Android. The folder's access
//! list is protected (it inherits nothing), so other users cannot read someone's Android
//! data, and it lets Hyper-V's virtual machine processes in, which is what a VM needs to
//! open its disk.

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

/// Makes sure `disk` exists and its folder carries [`USER_FOLDER_SDDL`]. A new disk is a
/// dynamic VHDX that may grow to `size_gb`; an existing one is left as it is.
pub fn prepare_user_data_disk(disk: &Path, size_gb: u32) -> Result<(), String> {
    let folder = disk
        .parent()
        .ok_or_else(|| format!("{} has no folder", disk.display()))?;
    std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    protect_folder(folder)?;
    if disk.exists() {
        return Ok(());
    }
    create_vhdx(disk, u64::from(size_gb) << 30)
}

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

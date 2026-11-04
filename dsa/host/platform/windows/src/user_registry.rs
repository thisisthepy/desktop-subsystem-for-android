//! The registry keys the window manager writes in each user's own classes: the protocol a
//! click on a notification launches, and the AppUserModelID of each app that posted one.
//! Uninstalling removes them for every user, signed in or not.
//!
//! What a program writes to `HKCU\Software\Classes` lands in the user's `UsrClass.dat`, which
//! Windows loads as `HKEY_USERS\<SID>_Classes` while the user is signed in. For a user who is
//! not, the uninstaller (running as SYSTEM) loads the file itself for as long as it needs it.

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteTreeW, RegEnumKeyExW, RegLoadKeyW, RegOpenKeyExW, RegUnLoadKeyW, HKEY,
    HKEY_USERS, KEY_ALL_ACCESS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::compositor::notifications::PROTOCOL;

/// The prefix of every AppUserModelID DSA gives an app.
const APP_ID_PREFIX: &str = "DSA.";

/// Where a hive is loaded while its keys are removed.
const MOUNT: &str = "DSA-Uninstall-Classes";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(root: HKEY, path: &str) -> Option<HKEY> {
    let path = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated path; the key is closed by the caller.
    let code = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_ALL_ACCESS, &mut key) };
    (code == ERROR_SUCCESS).then_some(key)
}

fn subkeys(key: HKEY) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0.. {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        // SAFETY: a buffer of the length given, and an open key.
        let code = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut length,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if code != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..length as usize]));
    }
    names
}

fn delete_tree(key: HKEY, name: &str) -> Result<(), String> {
    let name_w = wide(name);
    // SAFETY: an open key and a NUL-terminated name.
    let code = unsafe { RegDeleteTreeW(key, name_w.as_ptr()) };
    match code {
        ERROR_SUCCESS | windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND => Ok(()),
        code => Err(format!(
            "removing {name}: {}",
            std::io::Error::from_raw_os_error(code as i32)
        )),
    }
}

/// Removes DSA's keys from one user's classes, opened as `classes`.
fn remove_from(classes: HKEY) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(why) = delete_tree(classes, PROTOCOL) {
        problems.push(why);
    }
    if let Some(ids) = open(classes, "AppUserModelId") {
        for name in subkeys(ids)
            .into_iter()
            .filter(|name| name.starts_with(APP_ID_PREFIX))
        {
            if let Err(why) = delete_tree(ids, &name) {
                problems.push(why);
            }
        }
        // SAFETY: opened above, closed once.
        unsafe { RegCloseKey(ids) };
    }
    problems
}

/// Turns on a privilege this process holds but has off, as SYSTEM holds the two that loading a
/// hive needs.
fn enable_privilege(name: &str) -> Result<(), String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: this process's own token, closed below.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return Err(format!(
            "opening this process's token: {}",
            std::io::Error::last_os_error()
        ));
    }
    let name_w = wide(name);
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: a NUL-terminated name and a LUID to write.
    let found = unsafe { LookupPrivilegeValueW(std::ptr::null(), name_w.as_ptr(), &mut luid) };
    let result = if found == 0 {
        Err(format!("{name}: {}", std::io::Error::last_os_error()))
    } else {
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: an open token and one privilege.
        if unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                &privileges,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            Err(format!("{name}: {}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    };
    // SAFETY: opened above, closed once.
    unsafe { CloseHandle(token) };
    result
}

/// Removes DSA's keys from the classes of the user `sid`, whose local application data folder
/// is `local_app_data`. Returns what could not be removed, to report.
pub fn remove_user_keys(sid: &str, local_app_data: &Path) -> Vec<String> {
    if let Some(classes) = open(HKEY_USERS, &format!("{sid}_Classes")) {
        let problems = remove_from(classes);
        // SAFETY: opened above, closed once.
        unsafe { RegCloseKey(classes) };
        return problems;
    }
    let hive = local_app_data
        .join("Microsoft")
        .join("Windows")
        .join("UsrClass.dat");
    if !hive.exists() {
        return Vec::new();
    }
    for privilege in ["SeBackupPrivilege", "SeRestorePrivilege"] {
        if let Err(why) = enable_privilege(privilege) {
            return vec![format!("{sid}: {why}")];
        }
    }
    let (mount, file) = (wide(MOUNT), wide(&hive.to_string_lossy()));
    // SAFETY: NUL-terminated names; the hive is unloaded below.
    let code = unsafe { RegLoadKeyW(HKEY_USERS, mount.as_ptr(), file.as_ptr()) };
    if code != ERROR_SUCCESS {
        return vec![format!(
            "{sid}: loading {}: {}",
            hive.display(),
            std::io::Error::from_raw_os_error(code as i32)
        )];
    }
    let problems = match open(HKEY_USERS, MOUNT) {
        Some(classes) => {
            let problems = remove_from(classes);
            // SAFETY: opened above, closed once, before the hive is unloaded.
            unsafe { RegCloseKey(classes) };
            problems
        }
        None => vec![format!("{sid}: opening the loaded {}", hive.display())],
    };
    // SAFETY: the hive loaded above.
    unsafe { RegUnLoadKeyW(HKEY_USERS, mount.as_ptr()) };
    problems
}

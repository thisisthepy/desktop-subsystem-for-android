//! Where a user's local application data lives, found from their SID.
//!
//! The service runs as SYSTEM, so it cannot ask for "my" `%LOCALAPPDATA%`; it looks up the
//! caller's. Windows records each profile's folder under `ProfileList` in the machine's
//! registry. A user who moved their local application data has the new place in their own
//! registry hive, which is loaded while they are signed in, as they are when they ask for
//! Android. Without that entry the folder is the profile's `AppData\Local`, which is where
//! WSL keeps each user's distribution disks too.

use std::io;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a string value; `None` when the key or value is absent.
fn read_string(root: HKEY, key: &str, value: &str, expand: bool) -> Result<Option<String>, String> {
    let (key_w, value_w) = (wide(key), wide(value));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | if expand { 0 } else { RRF_NOEXPAND };
    let mut size = 0u32;
    // SAFETY: NUL-terminated names; a null buffer asks for the size only.
    let code = unsafe {
        RegGetValueW(
            root,
            key_w.as_ptr(),
            value_w.as_ptr(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if code != ERROR_SUCCESS {
        return Err(format!(
            "reading {key}\\{value}: {}",
            io::Error::from_raw_os_error(code as i32)
        ));
    }
    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer holds `size` bytes.
    let code = unsafe {
        RegGetValueW(
            root,
            key_w.as_ptr(),
            value_w.as_ptr(),
            flags,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(format!(
            "reading {key}\\{value}: {}",
            io::Error::from_raw_os_error(code as i32)
        ));
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..len])))
}

/// The local application data folder of the user with SID `sid`.
pub fn local_app_data(sid: &str) -> Result<PathBuf, String> {
    let profile = read_string(
        HKEY_LOCAL_MACHINE,
        &format!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{sid}"),
        "ProfileImagePath",
        true,
    )?
    .ok_or_else(|| format!("Windows has no profile folder for {sid}; sign in once first"))?;
    let moved = read_string(
        HKEY_USERS,
        &format!(r"{sid}\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders"),
        "Local AppData",
        false,
    )
    .unwrap_or(None);
    Ok(resolve(&profile, moved.as_deref()))
}

/// The folder from the profile folder and the user's own "Local AppData" setting, which
/// names the profile as `%USERPROFILE%`. A setting that names other variables cannot be
/// expanded for another user, so the default place is used for it.
fn resolve(profile: &str, moved: Option<&str>) -> PathBuf {
    if let Some(moved) = moved {
        let marker = "%USERPROFILE%";
        let expanded = match moved.get(..marker.len()) {
            Some(head) if head.eq_ignore_ascii_case(marker) => {
                format!("{profile}{}", &moved[marker.len()..])
            }
            _ => moved.to_string(),
        };
        if !expanded.contains('%') && !expanded.is_empty() {
            return PathBuf::from(expanded);
        }
    }
    PathBuf::from(profile).join("AppData").join("Local")
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use std::path::PathBuf;

    #[test]
    fn pr5_a_moved_local_app_data_is_followed_and_an_unexpandable_one_is_not() {
        let profile = r"C:\Users\alice";
        assert_eq!(
            resolve(profile, None),
            PathBuf::from(r"C:\Users\alice\AppData\Local")
        );
        assert_eq!(
            resolve(profile, Some(r"%USERPROFILE%\AppData\Local")),
            PathBuf::from(r"C:\Users\alice\AppData\Local")
        );
        assert_eq!(
            resolve(profile, Some(r"D:\Profiles\alice\Local")),
            PathBuf::from(r"D:\Profiles\alice\Local")
        );
        assert_eq!(
            resolve(profile, Some(r"%HOMESHARE%\Local")),
            PathBuf::from(r"C:\Users\alice\AppData\Local")
        );
    }
}

//! Showing the shared storage of the user's Android in Explorer.
//!
//! The service bridges a loopback port to the guest, which serves its shared storage as
//! WebDAV, and answers with the address (`http://127.0.0.1:<port>/<token>/`). Explorer opens
//! WebDAV through Windows' WebDAV redirector, which takes the address as a UNC path,
//! `\\127.0.0.1@<port>\<token>`. The token is the first part of the path and the service
//! makes a new one for each session, so only someone the service told can open the files.
//!
//! The settings app's button runs `dsa files open`, which asks the service for the address,
//! starting Android when it is not running, and opens it. The installed window host does the
//! same with [`ANDROID_FILES_ARGUMENT`], which a "DSA Files" shortcut an earlier version made
//! and the user pinned still runs.
//!
//! That address changes with every session, so nothing can keep it. Explorer's navigation
//! pane keeps "Android" instead ([`register_in_navigation_pane`]): a folder shortcut, as WSL's
//! "Linux" is, whose target is the service's files door (`\\127.0.0.1@<door port>\<key>`),
//! which does not change and starts Android when it is opened. The key is the user's own.
//!
//! Explorer opens the user's Documents when it is given a path that does not answer, so every
//! path is checked first ([`reachable`]), and a path that does not answer is an error the user
//! sees.

/// The argument that has the window host open Android's files in Explorer.
pub const ANDROID_FILES_ARGUMENT: &str = "--android-files";

/// The UNC path the WebDAV redirector takes for `url`, an `http://host:port/path/` address on
/// the loopback address. `None` for any other address.
pub fn unc_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = authority.rsplit_once(':')?;
    if host != "127.0.0.1" || port.parse::<u16>().is_err() {
        return None;
    }
    let path = path.trim_end_matches('/');
    let mut unc = format!(r"\\{host}@{port}");
    if path.is_empty() {
        unc.push_str(r"\DavWWWRoot");
    } else {
        unc.push('\\');
        unc.push_str(&path.replace('/', "\\"));
    }
    Some(unc)
}

/// Opens `path` in a new Explorer window.
#[cfg(windows)]
pub fn open_in_explorer(path: &str) -> Result<(), String> {
    let windows = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    std::process::Command::new(windows.join("explorer.exe"))
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot open {path} in Explorer: {e}"))
}

/// Asks the service at `api` where the caller's Android files are, as a path Explorer opens.
#[cfg(windows)]
pub fn locate(api: &mut dyn dsa_transport::Connector) -> Result<String, String> {
    let url = dsa_host_core::host_api::HostApiClient::connect(
        api,
        "dsa-app-window files",
        std::time::Duration::from_secs(5),
    )
    .map_err(|e| format!("the DSA service cannot be reached: {e}"))?
    .android_files()
    .map_err(|e| e.to_string())?;
    unc_path(&url).ok_or_else(|| format!("Explorer cannot open {url}"))
}

/// The class ID of the "Android" entry in Explorer's navigation pane.
pub const NAVIGATION_CLSID: &str = "{6F1D4E2A-3B7C-4D85-9A1E-7C2B5D8F0A43}";

/// The name of the entry, as Explorer shows it.
pub const NAVIGATION_NAME: &str = "Android";

/// Where Explorer sorts the entry in its navigation pane: directly below WSL's "Linux", whose
/// installer gives it 119 (0x77). Entries with the same number sort by name, so one more keeps
/// this entry below it.
pub const NAVIGATION_SORT_ORDER: u32 = 0x78;

/// The value, beside the entry in the user's own classes, that holds the user's key.
pub const KEY_VALUE: &str = "DsaFilesKey";

/// The shell's folder shortcut object, which shows a folder (here a UNC path) as its own entry.
const FOLDER_SHORTCUT_CLSID: &str = "{0E5AAE11-A475-4c5b-AB00-C66DE400274E}";

/// The key of the entry in a user's classes.
pub fn navigation_key() -> String {
    format!("CLSID\\{NAVIGATION_CLSID}")
}

/// The UNC path of the files door for `key`: the same on every start of Android and of the
/// service, so the navigation pane can keep it.
pub fn door_path(key: &str) -> String {
    format!(
        r"\\127.0.0.1@{}\{key}",
        dsa_host_core::files_door::DOOR_PORT
    )
}

/// The registry values of the entry, as (subkey under the entry, value name, value), with
/// `icon` the program whose icon it shows and `target` the folder it opens. A `None` value
/// name is the key's default value. Numbers are DWORDs.
pub fn navigation_values(
    icon: &str,
    target: &str,
) -> Vec<(&'static str, Option<&'static str>, RegValue)> {
    use RegValue::{Dword, Text};
    vec![
        ("", None, Text(NAVIGATION_NAME.into())),
        // The name and tooltip in the user's display language, from the window host's
        // string table.
        (
            "",
            Some("LocalizedString"),
            Text(format!(
                "@{icon},-{}",
                dsa_exe_resources::shell::NAVIGATION_NAME
            )),
        ),
        (
            "",
            Some("InfoTip"),
            Text(format!(
                "@{icon},-{}",
                dsa_exe_resources::shell::NAVIGATION_TIP
            )),
        ),
        // Shown in the navigation pane, directly below WSL's "Linux".
        ("", Some("System.IsPinnedToNameSpaceTree"), Dword(1)),
        ("", Some("SortOrderIndex"), Dword(NAVIGATION_SORT_ORDER)),
        (
            "DefaultIcon",
            None,
            Text(format!(
                "{icon},-{}",
                dsa_exe_resources::FILES_ICON_GROUP_ID
            )),
        ),
        (
            "InProcServer32",
            None,
            Text(r"%SystemRoot%\system32\shell32.dll".into()),
        ),
        (
            "Instance",
            Some("CLSID"),
            Text(FOLDER_SHORTCUT_CLSID.into()),
        ),
        // A folder (0x10) that is read only to the shell's own checks (0x01): Explorer takes
        // the real rights from the server.
        ("Instance\\InitPropertyBag", Some("Attributes"), Dword(0x11)),
        (
            "Instance\\InitPropertyBag",
            Some("TargetFolderPath"),
            Text(target.into()),
        ),
        // A folder that can be browsed, has subfolders, and holds the file system's items.
        ("ShellFolder", Some("FolderValueFlags"), Dword(0x28)),
        ("ShellFolder", Some("Attributes"), Dword(0xF080_004D)),
    ]
}

/// A registry value of the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    Text(String),
    Dword(u32),
}

/// Whether the entry a user already has is what [`navigation_values`] would write, by the
/// values that change between versions and between paths. `stored` reads a value by subkey and
/// name. An entry an earlier version wrote fails this, so it is written again: the user's entry
/// takes the place of the machine's, so the installer cannot correct it.
pub fn entry_is_current(
    icon: &str,
    target: &str,
    stored: impl Fn(&str, Option<&str>) -> Option<RegValue>,
) -> bool {
    navigation_values(icon, target)
        .into_iter()
        .filter(|(subkey, name, _)| {
            matches!(
                (*subkey, *name),
                ("Instance\\InitPropertyBag", Some("TargetFolderPath"))
                    | ("DefaultIcon", None)
                    | ("", Some("InfoTip"))
                    | ("", Some("SortOrderIndex"))
            )
        })
        .all(|(subkey, name, value)| stored(subkey, name).as_ref() == Some(&value))
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, KEY_READ, KEY_WRITE,
        REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// An open key, closed when dropped.
    pub struct Key(pub HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: opened by create, closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    pub fn create(root: HKEY, path: &str) -> Result<Key, String> {
        let path_w = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path and an out key.
        let code = unsafe {
            RegCreateKeyExW(
                root,
                path_w.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if code != ERROR_SUCCESS {
            return Err(format!(
                "creating {path}: {}",
                std::io::Error::from_raw_os_error(code as i32)
            ));
        }
        Ok(Key(key))
    }

    pub fn text(root: HKEY, path: &str, name: &str) -> Option<String> {
        let (path_w, name_w) = (wide(path), wide(name));
        let mut buffer = [0u16; 512];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: NUL-terminated names and a buffer of the size given.
        let code = unsafe {
            RegGetValueW(
                root,
                path_w.as_ptr(),
                name_w.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if code != ERROR_SUCCESS {
            return None;
        }
        let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]))
    }

    pub fn dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
        let (path_w, name_w) = (wide(path), wide(name));
        let mut number = 0u32;
        let mut size = 4u32;
        // SAFETY: NUL-terminated names and four bytes of space.
        let code = unsafe {
            RegGetValueW(
                root,
                path_w.as_ptr(),
                name_w.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut number as *mut u32).cast(),
                &mut size,
            )
        };
        (code == ERROR_SUCCESS).then_some(number)
    }

    pub fn set(key: &Key, name: Option<&str>, value: &super::RegValue) -> Result<(), String> {
        let name_w = name.map(wide);
        let name_ptr = name_w.as_ref().map_or(std::ptr::null(), |n| n.as_ptr());
        let code = match value {
            super::RegValue::Text(text) => {
                let data = wide(text);
                let kind = if text.contains('%') {
                    REG_EXPAND_SZ
                } else {
                    REG_SZ
                };
                // SAFETY: an open key and data of the length given.
                unsafe {
                    RegSetValueExW(
                        key.0,
                        name_ptr,
                        0,
                        kind,
                        data.as_ptr().cast(),
                        (data.len() * 2) as u32,
                    )
                }
            }
            super::RegValue::Dword(number) => {
                // SAFETY: an open key and four bytes of data.
                unsafe {
                    RegSetValueExW(
                        key.0,
                        name_ptr,
                        0,
                        REG_DWORD,
                        (number as *const u32).cast(),
                        4,
                    )
                }
            }
        };
        if code != ERROR_SUCCESS {
            return Err(format!(
                "setting {}: {}",
                name.unwrap_or("(default)"),
                std::io::Error::from_raw_os_error(code as i32)
            ));
        }
        Ok(())
    }
}

/// Puts "Android" in the current user's Explorer navigation pane, pointing at the files door
/// under the user's key, and returns the door's path. The key is made the first time and kept
/// beside the entry in the user's own classes, which only the user and the service can read.
/// The window manager does this each time it starts, which also corrects an entry an earlier
/// version wrote; [`ANDROID_FILES_ARGUMENT`] does it too.
#[cfg(windows)]
pub fn register_in_navigation_pane() -> Result<String, String> {
    use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
    let base = format!("Software\\Classes\\{}", navigation_key());
    let key = match registry::text(HKEY_CURRENT_USER, &base, KEY_VALUE)
        .filter(|key| key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()))
    {
        Some(key) => key,
        None => dsa_protocol::auth::to_hex(&dsa_protocol::auth::nonce()?),
    };
    let target = door_path(&key);
    let icon = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("dsa-app-window.exe")))
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|| "dsa-app-window.exe".into());
    let unchanged = entry_is_current(&icon, &target, |subkey, name| {
        let path = if subkey.is_empty() {
            base.clone()
        } else {
            format!("{base}\\{subkey}")
        };
        let name = name.unwrap_or("");
        if name == "SortOrderIndex" {
            registry::dword(HKEY_CURRENT_USER, &path, name).map(RegValue::Dword)
        } else {
            registry::text(HKEY_CURRENT_USER, &path, name).map(RegValue::Text)
        }
    });
    if unchanged {
        return Ok(target);
    }
    let entry = registry::create(HKEY_CURRENT_USER, &base)?;
    registry::set(&entry, Some(KEY_VALUE), &RegValue::Text(key.clone()))?;
    for (subkey, name, value) in navigation_values(&icon, &target) {
        if subkey.is_empty() {
            registry::set(&entry, name, &value)?;
        } else {
            let sub = registry::create(HKEY_CURRENT_USER, &format!("{base}\\{subkey}"))?;
            registry::set(&sub, name, &value)?;
        }
    }
    // Explorer reads its namespace again.
    // SAFETY: an event with no items.
    unsafe {
        windows_sys::Win32::UI::Shell::SHChangeNotify(
            windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
            windows_sys::Win32::UI::Shell::SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    Ok(target)
}

/// The user (by SID) whose key is `key`, among the users signed in: the service answers the
/// files door with it. Each user's classes are loaded under `HKEY_USERS\<SID>_Classes` while
/// they are signed in.
#[cfg(windows)]
pub fn owner_of_key(key: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegCloseKey, HKEY_USERS};
    if key.len() != 64 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let users = crate::user_registry::open(HKEY_USERS, "")?;
    let names = crate::user_registry::subkeys(users);
    // SAFETY: opened above, closed once.
    unsafe { RegCloseKey(users) };
    names.into_iter().find_map(|name| {
        let sid = name.strip_suffix("_Classes")?;
        let theirs = registry::text(
            HKEY_USERS,
            &format!("{name}\\{}", navigation_key()),
            KEY_VALUE,
        )?;
        theirs.eq_ignore_ascii_case(key).then(|| sid.to_string())
    })
}

/// Whether `path` answers in time: the WebDAV redirector can take long to give up, and an
/// Explorer window opened on a path that does not answer shows the user's Documents instead.
pub fn reachable(path: &str, wait: std::time::Duration) -> Result<(), String> {
    let (done, answer) = std::sync::mpsc::channel();
    let owned = path.to_string();
    std::thread::spawn(move || {
        let _ = done.send(std::fs::read_dir(&owned).map(|_| ()));
    });
    match answer.recv_timeout(wait) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("{path} cannot be opened: {error}")),
        Err(_) => Err(format!(
            "{path} did not answer within {} seconds",
            wait.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr29_the_webdav_address_becomes_the_redirectors_unc_path() {
        assert_eq!(
            unc_path("http://127.0.0.1:52714/3f9a/").as_deref(),
            Some(r"\\127.0.0.1@52714\3f9a")
        );
        assert_eq!(
            unc_path("http://127.0.0.1:52714/").as_deref(),
            Some(r"\\127.0.0.1@52714\DavWWWRoot")
        );
        assert_eq!(unc_path("http://example.com:80/x/"), None);
        assert_eq!(unc_path("file:///C:/"), None);
    }

    #[test]
    fn fr29_android_shows_in_the_navigation_pane_at_the_files_door() {
        let door = door_path("ab12");
        assert_eq!(
            door,
            format!(r"\\127.0.0.1@{}\ab12", dsa_host_core::files_door::DOOR_PORT)
        );
        let values = navigation_values(r"C:\Program Files\DSA\dsa-app-window.exe", &door);
        let value = |subkey: &str, name: Option<&str>| {
            values
                .iter()
                .find(|(s, n, _)| *s == subkey && *n == name)
                .map(|(_, _, v)| v.clone())
        };
        assert_eq!(value("", None), Some(RegValue::Text("Android".into())));
        assert_eq!(
            value("", Some("InfoTip")),
            Some(RegValue::Text(
                r"@C:\Program Files\DSA\dsa-app-window.exe,-108".into()
            ))
        );
        assert_eq!(
            value("", Some("LocalizedString")),
            Some(RegValue::Text(
                r"@C:\Program Files\DSA\dsa-app-window.exe,-107".into()
            ))
        );
        assert_eq!(
            value("", Some("System.IsPinnedToNameSpaceTree")),
            Some(RegValue::Dword(1))
        );
        assert_eq!(
            value("Instance\\InitPropertyBag", Some("TargetFolderPath")),
            Some(RegValue::Text(door))
        );
        assert_eq!(
            value("Instance", Some("CLSID")),
            Some(RegValue::Text(FOLDER_SHORTCUT_CLSID.into()))
        );
        assert_eq!(
            value("DefaultIcon", None),
            Some(RegValue::Text(
                r"C:\Program Files\DSA\dsa-app-window.exe,-3".into()
            ))
        );
    }

    #[test]
    fn fr29_android_sorts_directly_below_wsls_linux() {
        // The value WSL's installer gives "Linux", and the number the pane sorts by.
        const LINUX_SORT_ORDER: u32 = 0x77;
        assert_eq!(NAVIGATION_SORT_ORDER, LINUX_SORT_ORDER + 1);
        let values = navigation_values("icon.exe", "target");
        let order = values
            .iter()
            .find(|(s, n, _)| s.is_empty() && *n == Some("SortOrderIndex"))
            .map(|(_, _, v)| v.clone());
        assert_eq!(order, Some(RegValue::Dword(NAVIGATION_SORT_ORDER)));
    }

    #[test]
    fn fr29_an_entry_an_earlier_version_wrote_is_written_again() {
        let values = navigation_values("icon.exe", "target");
        let current = |order: Option<u32>| {
            entry_is_current("icon.exe", "target", |subkey, name| {
                if name == Some("SortOrderIndex") {
                    return order.map(RegValue::Dword);
                }
                values
                    .iter()
                    .find(|(s, n, _)| *s == subkey && *n == name)
                    .map(|(_, _, v)| v.clone())
            })
        };
        assert!(current(Some(NAVIGATION_SORT_ORDER)));
        assert!(!current(Some(0x42)));
        assert!(!current(None));
    }

    #[test]
    fn fr29_a_path_that_does_not_answer_is_an_error_not_another_folder() {
        let missing = std::env::temp_dir().join("dsa-no-such-folder-for-android-files");
        let error = reachable(
            &missing.display().to_string(),
            std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.contains("cannot be opened"), "{error}");
    }
}

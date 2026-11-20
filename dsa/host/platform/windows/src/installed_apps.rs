//! Each app installed in the user's Android, in Windows' list of installed apps.
//!
//! Windows Settings (Apps > Installed apps) and Control Panel's Programs and Features list the
//! programs that have a key under `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`
//! (for the user's own) with a display name. The window manager gives each Android app such a
//! key, named `Thisisthepy.DSA.<package>` like the app's AppUserModelID, so the app shows there
//! with its name, its icon (from its window program), its version, its size and the date it was
//! installed. Uninstalling it there runs the settings app with `--uninstall-app <package>`: a
//! small window shows the app and, without asking again (Windows has), runs
//! `dsa apps uninstall <package>` hidden, which
//! uninstalls the app in Android. The settings app is a windowed program, so no console window
//! flashes up as it would for `dsa` itself.
//!
//! The keys follow the apps whenever Android says which it has, together with the Start menu.
//! The window manager only ever writes and removes keys whose names start with
//! `Thisisthepy.DSA.`, which no other program uses, so another program's entry is never
//! touched. Uninstalling DSA removes them for every user.

use std::collections::BTreeSet;
use std::path::Path;

use dsa_host_core::compositor::window_hosts::is_valid_package;
use dsa_host_core::tasks::AppInfo;

use crate::start_menu::APP_ID_PREFIX;

/// What the entries say published each app: Android does not say who made an app, and the
/// entry is there because of DSA.
pub const PUBLISHER: &str = "Desktop Subsystem for Android";

/// The longest registry key name, in characters.
const MAX_KEY_NAME: usize = 255;

/// The name of `package`'s key under the list of installed apps.
pub fn key_name(package: &str) -> String {
    format!("{APP_ID_PREFIX}{package}")
        .chars()
        .take(MAX_KEY_NAME)
        .collect()
}

/// Whether `name` is the name of a key the window manager made.
pub fn is_ours(name: &str) -> bool {
    name.starts_with(APP_ID_PREFIX) && name.len() > APP_ID_PREFIX.len()
}

/// One registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `REG_SZ`.
    Text(String),
    /// `REG_DWORD`.
    Number(u32),
}

/// One app's key: its name and its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub values: Vec<(&'static str, Value)>,
}

/// The date `ms` (milliseconds since the Unix epoch) as `YYYYMMDD` in UTC, the form Windows
/// reads `InstallDate` in. `None` for 0, which means Android did not say.
pub fn install_date(ms: u64) -> Option<String> {
    if ms == 0 {
        return None;
    }
    // Howard Hinnant's days-to-civil algorithm.
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!("{year:04}{month:02}{day:02}"))
}

/// The command that uninstalls `package`: the settings app (at `uninstaller`) with
/// `--uninstall-app <package>`. Windows runs it as it is; the settings app is a windowed
/// program, so no console window shows, and it runs `dsa apps uninstall` hidden at once.
pub fn uninstall_command(uninstaller: &Path, package: &str) -> String {
    format!("\"{}\" --uninstall-app {package}", uninstaller.display())
}

/// The key of `app`, whose window program (and so its icon) is at `program`, uninstalled with
/// the settings app at `uninstaller`.
pub fn entry(app: &AppInfo, program: &Path, uninstaller: &Path) -> Entry {
    let name = app.display_name();
    let mut values = vec![
        ("DisplayName", Value::Text(name.to_string())),
        (
            "DisplayIcon",
            Value::Text(format!("\"{}\",0", program.display())),
        ),
        ("Publisher", Value::Text(PUBLISHER.to_string())),
        (
            "UninstallString",
            Value::Text(uninstall_command(uninstaller, &app.package_name)),
        ),
        ("NoModify", Value::Number(1)),
        ("NoRepair", Value::Number(1)),
        (
            "EstimatedSize",
            Value::Number(u32::try_from(app.total_bytes() / 1024).unwrap_or(u32::MAX)),
        ),
    ];
    if !app.version_name.trim().is_empty() {
        values.push((
            "DisplayVersion",
            Value::Text(app.version_name.trim().to_string()),
        ));
    }
    if let Some(date) = install_date(app.first_install_time_ms) {
        values.push(("InstallDate", Value::Text(date)));
    }
    Entry {
        key: key_name(&app.package_name),
        values,
    }
}

/// The keys under the list of installed apps, as the window manager reads and writes them.
pub trait InstalledAppKeys {
    /// The names of the keys there now.
    fn names(&self) -> Result<Vec<String>, String>;
    /// Makes the key `name` hold exactly `values`.
    fn write(&self, name: &str, values: &[(&'static str, Value)]) -> Result<(), String>;
    /// Removes the key `name` and everything under it; no key is no error.
    fn remove(&self, name: &str) -> Result<(), String>;
}

/// Makes the keys match `entries`: one for each, and none of ours for an app that is gone. A
/// key that is not ours is never touched. Returns what went wrong, one sentence each; the rest
/// is done anyway.
pub fn sync(entries: &[Entry], keys: &dyn InstalledAppKeys) -> Vec<String> {
    let mut problems = Vec::new();
    let wanted: BTreeSet<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
    match keys.names() {
        Ok(names) => {
            for name in names {
                if is_ours(&name) && !wanted.contains(name.as_str()) {
                    if let Err(why) = keys.remove(&name) {
                        problems.push(format!("could not remove {name}: {why}"));
                    }
                }
            }
        }
        Err(why) => problems.push(format!("could not read the installed apps: {why}")),
    }
    for entry in entries {
        if let Err(why) = keys.write(&entry.key, &entry.values) {
            problems.push(format!("could not write {}: {why}", entry.key));
        }
    }
    problems
}

/// The entries of `apps`, each with the window program the Start menu made for it in
/// `apps_dir`, uninstalled with the settings app at `uninstaller`.
pub fn entries(apps: &[AppInfo], apps_dir: &Path, uninstaller: &Path) -> Vec<Entry> {
    apps.iter()
        .filter(|app| is_valid_package(&app.package_name))
        .map(|app| {
            let label = app.label.trim();
            let label = if label.is_empty() {
                app.package_name.as_str()
            } else {
                label
            };
            let program = crate::app_exe::program_path(apps_dir, &app.package_name, label);
            entry(app, &program, uninstaller)
        })
        .collect()
}

/// Removes every key of ours. Returns what could not be removed.
pub fn remove_all(keys: &dyn InstalledAppKeys) -> Vec<String> {
    sync(&[], keys)
}

/// The keys of the registry, under `root\path`.
pub struct RegistryKeys {
    root: registry::Root,
    path: String,
}

impl RegistryKeys {
    /// The current user's list of installed apps.
    pub fn current_user() -> RegistryKeys {
        RegistryKeys {
            root: registry::Root::CurrentUser,
            path: crate::paths::INSTALLED_APPS_KEY.to_string(),
        }
    }

    /// The keys under `HKCU\<path>`, for tests.
    pub fn current_user_at(path: &str) -> RegistryKeys {
        RegistryKeys {
            root: registry::Root::CurrentUser,
            path: path.to_string(),
        }
    }

    /// The keys under `HKEY_USERS\<path>`, for a hive the uninstaller found or loaded.
    pub fn users_at(path: &str) -> RegistryKeys {
        RegistryKeys {
            root: registry::Root::Users,
            path: path.to_string(),
        }
    }
}

impl InstalledAppKeys for RegistryKeys {
    fn names(&self) -> Result<Vec<String>, String> {
        registry::subkeys(self.root, &self.path)
    }

    fn write(&self, name: &str, values: &[(&'static str, Value)]) -> Result<(), String> {
        registry::write(self.root, &format!("{}\\{name}", self.path), values)
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        registry::remove(self.root, &self.path, name)
    }
}

/// Removes the keys of ours from the list of installed apps of the user `sid`, whose profile is
/// at `profile`: in the user's hive as Windows has it loaded while they are signed in, else in
/// their `NTUSER.DAT`, loaded for as long as it takes. Returns what could not be removed.
pub fn remove_for_user(sid: &str, profile: &Path) -> Vec<String> {
    let loaded = format!("{sid}\\{}", crate::paths::INSTALLED_APPS_KEY);
    if registry::exists(registry::Root::Users, sid) {
        return remove_all(&RegistryKeys::users_at(&loaded));
    }
    let hive = crate::paths::user_hive_in_profile(profile);
    if !hive.exists() {
        return Vec::new();
    }
    match registry::with_loaded_hive(&hive, |mount| {
        remove_all(&RegistryKeys::users_at(&format!(
            "{mount}\\{}",
            crate::paths::INSTALLED_APPS_KEY
        )))
    }) {
        Ok(problems) => problems
            .into_iter()
            .map(|why| format!("{sid}: {why}"))
            .collect(),
        Err(why) => vec![format!("{sid}: {why}")],
    }
}

/// The calls into the registry.
mod registry {
    use std::path::Path;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW,
        RegEnumValueW, RegLoadKeyW, RegOpenKeyExW, RegSetValueExW, RegUnLoadKeyW, HKEY,
        HKEY_CURRENT_USER, HKEY_USERS, KEY_ALL_ACCESS, KEY_READ, REG_DWORD,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    use super::Value;

    /// Where a hive is loaded while its keys are removed.
    const MOUNT: &str = "DSA-Uninstall-User";

    #[derive(Debug, Clone, Copy)]
    pub enum Root {
        CurrentUser,
        Users,
    }

    fn handle(root: Root) -> HKEY {
        match root {
            Root::CurrentUser => HKEY_CURRENT_USER,
            Root::Users => HKEY_USERS,
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn os_error(code: u32) -> String {
        std::io::Error::from_raw_os_error(code as i32).to_string()
    }

    /// An open key, closed when dropped.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a key this process opened, closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn open(root: Root, path: &str, access: u32) -> Result<Option<Key>, String> {
        let path_w = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path; the key is closed by `Key`.
        let code = unsafe { RegOpenKeyExW(handle(root), path_w.as_ptr(), 0, access, &mut key) };
        match code {
            ERROR_SUCCESS => Ok(Some(Key(key))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            code => Err(format!("opening {path}: {}", os_error(code))),
        }
    }

    pub fn exists(root: Root, path: &str) -> bool {
        matches!(open(root, path, KEY_READ), Ok(Some(_)))
    }

    fn enumerate(key: &Key, values: bool) -> Vec<String> {
        let mut names = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 16_384];
            let mut length = name.len() as u32;
            // SAFETY: a buffer of the length given, and an open key.
            let code = unsafe {
                if values {
                    RegEnumValueW(
                        key.0,
                        index,
                        name.as_mut_ptr(),
                        &mut length,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                } else {
                    RegEnumKeyExW(
                        key.0,
                        index,
                        name.as_mut_ptr(),
                        &mut length,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                }
            };
            if code != ERROR_SUCCESS {
                break;
            }
            names.push(String::from_utf16_lossy(&name[..length as usize]));
        }
        names
    }

    pub fn subkeys(root: Root, path: &str) -> Result<Vec<String>, String> {
        Ok(match open(root, path, KEY_READ)? {
            Some(key) => enumerate(&key, false),
            None => Vec::new(),
        })
    }

    pub fn write(root: Root, path: &str, values: &[(&'static str, Value)]) -> Result<(), String> {
        let path_w = wide(path);
        let mut raw: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path; the key is closed by `Key`.
        let code = unsafe {
            RegCreateKeyExW(
                handle(root),
                path_w.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut raw,
                std::ptr::null_mut(),
            )
        };
        if code != ERROR_SUCCESS {
            return Err(format!("creating {path}: {}", os_error(code)));
        }
        let key = Key(raw);
        // A value an earlier version of the entry had and this one does not goes.
        for old in enumerate(&key, true) {
            if !values
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&old))
            {
                let old_w = wide(&old);
                // SAFETY: an open key and a NUL-terminated name.
                unsafe { RegDeleteValueW(key.0, old_w.as_ptr()) };
            }
        }
        for (name, value) in values {
            let name_w = wide(name);
            let code = match value {
                Value::Text(text) => {
                    let data = wide(text);
                    // SAFETY: the data is `data.len() * 2` bytes, NUL included, as REG_SZ wants.
                    unsafe {
                        RegSetValueExW(
                            key.0,
                            name_w.as_ptr(),
                            0,
                            REG_SZ,
                            data.as_ptr().cast(),
                            (data.len() * 2) as u32,
                        )
                    }
                }
                Value::Number(number) => {
                    let data = number.to_le_bytes();
                    // SAFETY: four bytes, as REG_DWORD wants.
                    unsafe {
                        RegSetValueExW(
                            key.0,
                            name_w.as_ptr(),
                            0,
                            REG_DWORD,
                            data.as_ptr(),
                            data.len() as u32,
                        )
                    }
                }
            };
            if code != ERROR_SUCCESS {
                return Err(format!("setting {name}: {}", os_error(code)));
            }
        }
        Ok(())
    }

    pub fn remove(root: Root, path: &str, name: &str) -> Result<(), String> {
        let Some(parent) = open(root, path, KEY_ALL_ACCESS)? else {
            return Ok(());
        };
        let name_w = wide(name);
        // SAFETY: an open key and a NUL-terminated name.
        match unsafe { RegDeleteTreeW(parent.0, name_w.as_ptr()) } {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            code => Err(os_error(code)),
        }
    }

    /// Loads the hive at `file` under `HKEY_USERS`, runs `work` with the name it is mounted
    /// as, and unloads it. The privileges loading needs are turned on first, as SYSTEM holds
    /// them.
    pub fn with_loaded_hive<T>(file: &Path, work: impl FnOnce(&str) -> T) -> Result<T, String> {
        for privilege in ["SeBackupPrivilege", "SeRestorePrivilege"] {
            crate::user_registry::enable_privilege(privilege)?;
        }
        let (mount, file_w) = (wide(MOUNT), wide(&file.to_string_lossy()));
        // SAFETY: NUL-terminated names; the hive is unloaded below.
        let code = unsafe { RegLoadKeyW(HKEY_USERS, mount.as_ptr(), file_w.as_ptr()) };
        if code != ERROR_SUCCESS {
            return Err(format!("loading {}: {}", file.display(), os_error(code)));
        }
        let result = work(MOUNT);
        // SAFETY: the hive loaded above; every key in it this module opened is closed by now.
        unsafe { RegUnLoadKeyW(HKEY_USERS, mount.as_ptr()) };
        Ok(result)
    }
}

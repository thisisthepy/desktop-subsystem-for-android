//! Each app's own window program: a copy of the app window host carrying the app's name and
//! icon.
//!
//! Task Manager and the taskbar group windows by program, and take the name and icon from
//! the program file. So each Android app gets its own copy of `dsa-app-window.exe`, at
//! `%LOCALAPPDATA%\DSA\apps\<package>\<Label>.exe`, named after the app, whose version
//! information names the app and whose icon is the app's launcher icon. Settings then shows
//! as `Settings.exe` and Settings, with its own icon and its own entry, apart from every other
//! app. Each package has its own folder, so two apps of the same name never share a file.
//!
//! A copy is made again when the installed window host changes (DSA was updated) or the app's
//! name or icon does. A note beside it (`made.json`) records what it was made from. A copy
//! under the app's old name is removed once it no longer runs. A copy that is running cannot
//! be replaced; when its name stays the same, it stays as it is until the app's windows close.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use dsa_exe_resources::{
    icon_group, names, png_icon, version_info, VersionStrings, ICON_GROUP_ID, LANG_EN_US,
    RT_GROUP_ICON, RT_ICON, RT_VERSION, VERSION_ID,
};
use dsa_host_core::compositor::window_hosts::is_valid_package;
use dsa_host_core::tasks::AppInfo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, FindResourceExW, LoadLibraryExW, UpdateResourceW,
    LOAD_LIBRARY_AS_DATAFILE,
};

/// The installed app window host, beside the window manager.
pub const TEMPLATE_NAME: &str = "dsa-app-window.exe";

/// The most icon images a program is looked through for.
const MAX_ICON_IMAGES: u16 = 64;

/// The note in each app's folder that records what its program was made from.
pub const NOTE_NAME: &str = "made.json";

/// The longest file name an app's program gets, in characters, before `.exe`. Paths in the
/// user's profile stay well under the classic 260-character limit with it.
const MAX_NAME: usize = 64;

/// The file name, without `.exe`, of the program named `label`: the label with what Windows
/// does not allow in a file name taken out. `package` stands in when nothing usable is left.
pub fn program_name(package: &str, label: &str) -> String {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let cleaned: String = label
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c))
        .collect();
    let mut name: String = cleaned.trim().chars().take(MAX_NAME).collect();
    // Windows drops trailing dots and spaces from a file name.
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    let name = name.trim_start().to_string();
    if name.is_empty() {
        return package.to_string();
    }
    // A name whose part before the first dot is a device's (`CON`, `COM1`) opens the device.
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    let numbered = stem.chars().count() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem
            .chars()
            .nth(3)
            .is_some_and(|c| c.is_ascii_digit() || "\u{b9}\u{b2}\u{b3}".contains(c));
    if DEVICES.contains(&stem.as_str()) || numbered {
        return format!("{name}_");
    }
    name
}

/// Where the program of `package` named `label` goes: its own folder, in `apps_dir`.
pub fn program_path(apps_dir: &Path, package: &str, label: &str) -> PathBuf {
    apps_dir
        .join(package)
        .join(format!("{}.exe", program_name(package, label)))
}

/// Removes what is left of the app's older programs: the ones under another name in its
/// folder, unfinished copies, and the program and note of the earlier layout, where every
/// app's program was `<package>.exe` directly in `apps_dir`. A program that still runs
/// cannot be removed; it is tried again the next time.
fn remove_old(apps_dir: &Path, package: &str, keep: &Path) {
    let _ = fs::remove_file(apps_dir.join(format!("{package}.exe")));
    let _ = fs::remove_file(apps_dir.join(format!("{package}.exe.new")));
    let _ = fs::remove_file(apps_dir.join(format!("{package}.json")));
    let Some(dir) = keep.parent() else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if path != keep && (name.ends_with(".exe") || name.ends_with(".exe.new")) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Where each app's window program goes: `%LOCALAPPDATA%\DSA\apps`.
pub fn default_apps_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("DSA").join("apps"))
}

/// The installed app window host: beside this program.
pub fn default_template() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join(TEMPLATE_NAME))
}

/// What a copy was made from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Made {
    /// The installed window host's size and modification time.
    template: String,
    label: String,
    /// SHA-256 of the icon, in hexadecimal; empty for none.
    icon: String,
}

fn template_stamp(template: &Path) -> io::Result<String> {
    let meta = fs::metadata(template)?;
    let modified = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("{}-{modified}", meta.len()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The name a copy carries: the app's own, else its package.
fn label_of(package: &str, app: Option<&AppInfo>) -> String {
    app.map(|app| app.label.trim())
        .filter(|label| !label.is_empty())
        .unwrap_or(package)
        .to_string()
}

/// Makes sure `apps_dir` holds the window program of `package`, made from `template` with the
/// app's name and icon and named after the app, and returns its path. An existing copy that
/// is up to date is used as it is; an outdated one of the same name that is running is used
/// too, and the reason is returned with it.
pub fn ensure(
    template: &Path,
    apps_dir: &Path,
    package: &str,
    app: Option<&AppInfo>,
) -> Result<(PathBuf, Option<String>), String> {
    if !is_valid_package(package) {
        return Err(format!("{package:?} is not an Android package name"));
    }
    let label = label_of(package, app);
    let target = program_path(apps_dir, package, &label);
    let Some(folder) = target.parent().map(Path::to_path_buf) else {
        return Err(format!("{} has no folder", target.display()));
    };
    let note = folder.join(NOTE_NAME);
    let icon = app
        .map(|app| app.icon_png.as_slice())
        .filter(|png| png_icon(png).is_some());
    let made = Made {
        template: template_stamp(template)
            .map_err(|e| format!("the app window host {}: {e}", template.display()))?,
        label,
        icon: icon
            .map(|png| hex(&Sha256::digest(png)))
            .unwrap_or_default(),
    };
    let current = fs::read_to_string(&note)
        .ok()
        .and_then(|text| serde_json::from_str::<Made>(&text).ok());
    if target.exists() && current.as_ref() == Some(&made) {
        remove_old(apps_dir, package, &target);
        return Ok((target, None));
    }
    fs::create_dir_all(&folder)
        .map_err(|e| format!("could not create {}: {e}", folder.display()))?;
    let mut fresh = target.clone().into_os_string();
    fresh.push(".new");
    let fresh = PathBuf::from(fresh);
    fs::copy(template, &fresh).map_err(|e| {
        format!(
            "could not copy {} to {}: {e}",
            template.display(),
            fresh.display()
        )
    })?;
    if let Err(why) = write_resources(&fresh, package, &made.label, icon) {
        let _ = fs::remove_file(&fresh);
        return Err(why);
    }
    match fs::rename(&fresh, &target) {
        Ok(()) => {
            let text = serde_json::to_string(&made).unwrap_or_default();
            let _ = fs::write(&note, text);
            remove_old(apps_dir, package, &target);
            Ok((target, None))
        }
        Err(e) if target.exists() => {
            let _ = fs::remove_file(&fresh);
            Ok((
                target,
                Some(format!(
                    "{package}'s window program is in use, so it keeps its old name and icon \
                     until its windows close ({e})"
                )),
            ))
        }
        Err(e) => {
            let _ = fs::remove_file(&fresh);
            Err(format!(
                "could not put {package}'s window program in place: {e}"
            ))
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// How many icon images the program at `path` has, numbered from 1.
fn icon_images(path: &Path) -> u16 {
    let path_w = wide(&path.to_string_lossy());
    // SAFETY: loaded as data only, never run; freed below.
    let module = unsafe {
        LoadLibraryExW(
            path_w.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_AS_DATAFILE,
        )
    };
    if module.is_null() {
        return 0;
    }
    let mut count = 0;
    while count < MAX_ICON_IMAGES {
        // SAFETY: integer resource IDs, in a module loaded above.
        let found = unsafe {
            FindResourceExW(
                module,
                RT_ICON as usize as *const u16,
                (count + 1) as usize as *const u16,
                0,
            )
        };
        if found.is_null() {
            break;
        }
        count += 1;
    }
    // SAFETY: the module loaded above.
    unsafe { FreeLibrary(module) };
    count
}

/// Rewrites the version information and icon of the program at `path`.
fn write_resources(
    path: &Path,
    package: &str,
    label: &str,
    icon: Option<&[u8]>,
) -> Result<(), String> {
    let version = crate::VERSION.to_string();
    let strings = VersionStrings {
        description: label.to_string(),
        product: names::PRODUCT.to_string(),
        company: names::COMPANY.to_string(),
        internal_name: package.to_string(),
        version,
    };
    let images = icon_images(path);
    let path_w = wide(&path.to_string_lossy());
    // SAFETY: a NUL-terminated path; the handle is ended below on every path.
    let update = unsafe { BeginUpdateResourceW(path_w.as_ptr(), 0) };
    if update.is_null() {
        return Err(format!(
            "could not open {} to name it: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    let put = |kind: u16, id: u16, language: u16, data: Option<&[u8]>| -> bool {
        let (pointer, length) = match data {
            Some(data) => (data.as_ptr() as *const std::ffi::c_void, data.len() as u32),
            None => (std::ptr::null(), 0),
        };
        // SAFETY: the type and name are integer resource IDs; the data outlives the call.
        unsafe {
            UpdateResourceW(
                update,
                kind as usize as *const u16,
                id as usize as *const u16,
                language,
                pointer,
                length,
            ) != 0
        }
    };
    let mut ok = put(
        RT_VERSION,
        VERSION_ID,
        LANG_EN_US,
        Some(&version_info(&strings, LANG_EN_US)),
    );
    // The installed window host has only an English name, which this replaces; the app's
    // own name already comes in the language Android shows.
    if let Some(image) = icon.and_then(png_icon) {
        ok &= put(RT_ICON, 1, 0, Some(&image.data));
        // Only the app's image stays, so no size of the window host's own icon shows.
        for id in 2..=images {
            ok &= put(RT_ICON, id, 0, None);
        }
        ok &= put(
            RT_GROUP_ICON,
            ICON_GROUP_ID,
            0,
            Some(&icon_group(&[image], 1)),
        );
    }
    let error = io::Error::last_os_error();
    // SAFETY: ends the update begun above; discards it when something failed.
    let written = unsafe { EndUpdateResourceW(update, if ok { 0 } else { 1 }) } != 0;
    if !ok {
        return Err(format!(
            "could not write {package}'s name and icon into {}: {error}",
            path.display()
        ));
    }
    if !written {
        return Err(format!(
            "could not save {package}'s name and icon into {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Starts each app's window program: makes the app's copy of the window host when it is
/// missing or outdated, then runs it.
pub struct ProgramLauncher {
    pub template: PathBuf,
    pub apps_dir: PathBuf,
    pub logger: Option<std::sync::Arc<crate::compositor::log::CompositorLogger>>,
}

impl crate::compositor::hosts::HostLauncher for ProgramLauncher {
    fn start(&mut self, package: &str, app: Option<&AppInfo>) -> Result<(), String> {
        let (program, note) = ensure(&self.template, &self.apps_dir, package, app)?;
        if let Some(note) = note {
            crate::compositor::log::note(self.logger.as_deref(), &note);
        }
        spawn(&program, package)
    }
}

/// Runs `program` as the window host of `package` the window manager started.
fn spawn(program: &Path, package: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let application = wide(&program.to_string_lossy());
    // The package is named too, though the program's folder names it, so the window host
    // never has to tell it from the file's place.
    let mut command_line = wide(&format!(
        "\"{}\" {} {} {package}",
        program.display(),
        crate::app_window::STARTED_BY_WINDOW_MANAGER,
        crate::app_window::PACKAGE_ARGUMENT
    ));
    // SAFETY: plain structures, zero is their documented initial state.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated strings; the command line buffer is writable, as the call needs.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(format!(
            "could not run {}: {}",
            program.display(),
            io::Error::last_os_error()
        ));
    }
    // The window host connects by itself; nothing waits on the process.
    // SAFETY: handles CreateProcessW returned, closed once.
    unsafe {
        CloseHandle(process.hProcess);
        CloseHandle(process.hThread);
    }
    Ok(())
}

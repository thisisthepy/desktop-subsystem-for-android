//! Each app's own window program: a copy of the app window host carrying the app's name and
//! icon.
//!
//! Task Manager and the taskbar group windows by program, and take the name and icon from
//! the program file. So each Android app gets its own copy of `dsa-app-window.exe`, at
//! `%LOCALAPPDATA%\DSA\apps\<package>.exe`, whose version information names the app and
//! whose icon is the app's launcher icon. Settings then shows as Settings, with its own icon
//! and its own entry, apart from every other app.
//!
//! A copy is made again when the installed window host changes (DSA was updated) or the app's
//! name or icon does. A note beside it (`<package>.json`) records what it was made from. A
//! copy that is running cannot be replaced; it stays as it is until the app's windows close.

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
/// app's name and icon, and returns its path. An existing copy that is up to date is used as
/// it is; an outdated one that is running is used too, and the reason is returned with it.
pub fn ensure(
    template: &Path,
    apps_dir: &Path,
    package: &str,
    app: Option<&AppInfo>,
) -> Result<(PathBuf, Option<String>), String> {
    if !is_valid_package(package) {
        return Err(format!("{package:?} is not an Android package name"));
    }
    let target = apps_dir.join(format!("{package}.exe"));
    let note = apps_dir.join(format!("{package}.json"));
    let icon = app
        .map(|app| app.icon_png.as_slice())
        .filter(|png| png_icon(png).is_some());
    let made = Made {
        template: template_stamp(template)
            .map_err(|e| format!("the app window host {}: {e}", template.display()))?,
        label: label_of(package, app),
        icon: icon
            .map(|png| hex(&Sha256::digest(png)))
            .unwrap_or_default(),
    };
    let current = fs::read_to_string(&note)
        .ok()
        .and_then(|text| serde_json::from_str::<Made>(&text).ok());
    if target.exists() && current.as_ref() == Some(&made) {
        return Ok((target, None));
    }
    fs::create_dir_all(apps_dir)
        .map_err(|e| format!("could not create {}: {e}", apps_dir.display()))?;
    let fresh = apps_dir.join(format!("{package}.exe.new"));
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
        spawn(&program)
    }
}

/// Runs `program` as a window host the window manager started.
fn spawn(program: &Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let application = wide(&program.to_string_lossy());
    let mut command_line = wide(&format!(
        "\"{}\" {}",
        program.display(),
        crate::app_window::STARTED_BY_WINDOW_MANAGER
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

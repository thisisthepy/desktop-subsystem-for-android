//! Where DSA keeps its files on Windows. Every path the host makes at run time comes from here.
//!
//! There are two roots and nothing else:
//!
//! ```text
//! %ProgramData%\DSA                  the machine's (SYSTEM and Administrators write)
//!   dsa-hostd.json                   the service's configuration, from the installer
//!   images\                          the Android image store
//!   updates\                         downloaded setup files and the update state
//!   settings\                        machine.json, users\<SID>.json, widget placements
//!   logs\                            dsa-hostd.log, android-<SID>.log (the guest console)
//!   tmp\                             files that cannot be avoided, such as each boot's initrd
//!
//! %LOCALAPPDATA%\DSA                 each user's (the user, SYSTEM and Administrators)
//!   android.vhdx                     the user's Android disk, which uninstalling keeps
//!   android.image-version            the image version that disk holds, kept with it
//!   apps\<package>\                  each app's own window program
//!   notifications\                   the icons notifications show
//!   logs\                            dsa-wm.log, dsa-app-window.log
//!   compositor.json                  the render GPU chosen before the settings app existed
//!   monitors.txt                     the user's monitors, for sizing the guest display
//!   start-menu.json                  the Start menu shortcuts the window manager made
//!   tmp\                             files that cannot be avoided, in user processes
//! ```
//!
//! The machine root is the folder the installer chose for data (the configuration file's
//! folder), `%ProgramData%\DSA` unless the user chose another.
//!
//! Temporary files are avoided; data goes through memory or pipes where it can. When one cannot
//! be avoided it goes in its root's `tmp` folder under a name no other process uses
//! ([`unique_name`]), and the writer removes it. The service clears what earlier runs left in
//! the machine's `tmp` when it starts ([`clear_tmp`]).
//!
//! Some files used to be elsewhere; [`migrate`] moves them, and never removes or replaces a
//! user's Android disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The folder below each root that is DSA's.
pub const DSA: &str = "DSA";

/// `base` with `parts` added, written as text with the separator `base` uses, so a Windows path
/// stays one on every platform the tests run on and a test's own folder stays usable.
fn join(base: &Path, parts: &[&str]) -> PathBuf {
    let mut text = base
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string();
    let separator = if text.contains('\\') {
        '\\'
    } else {
        std::path::MAIN_SEPARATOR
    };
    for part in parts {
        text.push(separator);
        text.push_str(part);
    }
    PathBuf::from(text)
}

/// The machine's DSA folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRoot(PathBuf);

impl MachineRoot {
    /// The machine root at `dir`, the folder the installer chose for data.
    pub fn at(dir: impl Into<PathBuf>) -> MachineRoot {
        MachineRoot(dir.into())
    }

    /// `%ProgramData%\DSA`, for when no configuration file says where the data folder is.
    pub fn default_location() -> MachineRoot {
        let program_data = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        MachineRoot(join(&program_data, &[DSA]))
    }

    /// The machine root of a configuration file: the folder it is in.
    pub fn of_config_file(config: &Path) -> MachineRoot {
        let text = config.to_string_lossy();
        match text.rfind(['\\', '/']) {
            Some(end) => MachineRoot(PathBuf::from(&text[..end])),
            None => MachineRoot::default_location(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }
    pub fn config_file(&self) -> PathBuf {
        join(&self.0, &["dsa-hostd.json"])
    }
    pub fn images(&self) -> PathBuf {
        join(&self.0, &["images"])
    }
    pub fn updates(&self) -> PathBuf {
        join(&self.0, &["updates"])
    }
    pub fn settings(&self) -> PathBuf {
        join(&self.0, &["settings"])
    }
    pub fn logs(&self) -> PathBuf {
        join(&self.0, &["logs"])
    }
    pub fn service_log(&self) -> PathBuf {
        join(&self.0, &["logs", "dsa-hostd.log"])
    }
    pub fn tmp(&self) -> PathBuf {
        join(&self.0, &["tmp"])
    }
    /// Where each user's disk was kept before it moved into their profile:
    /// `users\<SID>\data.vhdx`.
    pub fn old_user_disk(&self, sid: &str) -> PathBuf {
        join(&self.0, &["users", sid, "data.vhdx"])
    }
    /// The empty files that turned traces on before they were settings.
    pub fn old_trace_markers(&self) -> [PathBuf; 2] {
        [
            join(&self.0, &["input-trace"]),
            join(&self.0, &["resize-trace"]),
        ]
    }
}

/// One user's DSA folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRoot(PathBuf);

impl UserRoot {
    /// The user root below a user's local application data folder.
    pub fn in_local_app_data(local_app_data: &Path) -> UserRoot {
        UserRoot(join(local_app_data, &[DSA]))
    }

    /// The user root that is the folder `dir` itself, as the folder of a user's disk is.
    pub fn from_dir(dir: &Path) -> UserRoot {
        UserRoot(dir.to_path_buf())
    }

    /// The user root of a profile folder (`C:\Users\<name>`), for an account whose local
    /// application data was not moved.
    pub fn in_profile(profile: &Path) -> UserRoot {
        UserRoot::in_local_app_data(&default_local_app_data(profile))
    }

    /// The user root of the user this process runs as: `%LOCALAPPDATA%\DSA`, or
    /// `%USERPROFILE%\AppData\Local\DSA` when that is not set. `None` with neither.
    pub fn current() -> Option<UserRoot> {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return Some(UserRoot::in_local_app_data(Path::new(&local)));
        }
        std::env::var_os("USERPROFILE").map(|profile| UserRoot::in_profile(Path::new(&profile)))
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }
    pub fn android_disk(&self) -> PathBuf {
        join(&self.0, &["android.vhdx"])
    }
    /// The image version the Android disk holds, kept beside it and with it.
    pub fn android_disk_version(&self) -> PathBuf {
        join(&self.0, &["android.image-version"])
    }
    pub fn apps(&self) -> PathBuf {
        join(&self.0, &["apps"])
    }
    pub fn notifications(&self) -> PathBuf {
        join(&self.0, &["notifications"])
    }
    pub fn logs(&self) -> PathBuf {
        join(&self.0, &["logs"])
    }
    pub fn window_manager_log(&self) -> PathBuf {
        join(&self.0, &["logs", "dsa-wm.log"])
    }
    pub fn app_window_log(&self) -> PathBuf {
        join(&self.0, &["logs", "dsa-app-window.log"])
    }
    pub fn compositor_settings(&self) -> PathBuf {
        join(&self.0, &["compositor.json"])
    }
    pub fn monitors(&self) -> PathBuf {
        join(&self.0, &["monitors.txt"])
    }
    pub fn start_menu_note(&self) -> PathBuf {
        join(&self.0, &["start-menu.json"])
    }
    pub fn tmp(&self) -> PathBuf {
        join(&self.0, &["tmp"])
    }
    /// Where the window manager keeps the document it is printing, for as long as it prints.
    pub fn print_spool(&self) -> PathBuf {
        join(&self.0, &["tmp", "print"])
    }
    /// The empty files that turned traces on before they were settings.
    pub fn old_trace_markers(&self) -> [PathBuf; 2] {
        [
            join(&self.0, &["input-trace"]),
            join(&self.0, &["resize-trace"]),
        ]
    }

    /// Whether uninstalling keeps the entry named `name` in this root: the Android disk and the
    /// image version recorded with it, which is the user's data.
    pub fn kept_on_uninstall(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name == "android.vhdx" || name == "android.image-version"
    }
}

/// The one place outside the DSA folders where files are made at run time: the user's Start
/// menu `Programs` folder, for each app's shortcut. `%APPDATA%\Microsoft\Windows\Start
/// Menu\Programs` for this process's user.
pub fn start_menu_programs() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| {
        join(
            Path::new(&dir),
            &["Microsoft", "Windows", "Start Menu", "Programs"],
        )
    })
}

/// The local application data folder of the profile at `profile`, where Windows puts it unless
/// the user moved it: `<profile>\AppData\Local`.
pub fn default_local_app_data(profile: &Path) -> PathBuf {
    join(profile, &["AppData", "Local"])
}

/// The Start menu `Programs` folder of the profile at `profile`, as the uninstaller finds it.
pub fn start_menu_programs_in_profile(profile: &Path) -> PathBuf {
    join(
        profile,
        &[
            "AppData",
            "Roaming",
            "Microsoft",
            "Windows",
            "Start Menu",
            "Programs",
        ],
    )
}

/// A file name no other process and no other call in this one uses: `<prefix>-<pid>-<n><suffix>`.
pub fn unique_name(prefix: &str, suffix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}{suffix}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Removes everything in the `tmp` folder `dir` that earlier runs left: all but this process's
/// own files ([`unique_name`] puts its ID in their names). Returns what could not be removed,
/// with why, to log.
pub fn clear_tmp(dir: &Path) -> Vec<String> {
    let own = format!("-{}-", std::process::id());
    let mut problems = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return problems,
        Err(error) => return vec![format!("reading {}: {error}", dir.display())],
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name().to_string_lossy().contains(&own) {
            continue;
        }
        let path = entry.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed {
            problems.push(format!("removing {}: {error}", path.display()));
        }
    }
    problems
}

/// What moving one file from an old location did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// There was nothing at the old location.
    Nothing,
    /// The file is at its new location now.
    Moved,
    /// The file stays at the old location, for the reason given.
    Left(String),
}

/// Moves the file at `old` to `new`, where it belongs now. It never replaces anything: when
/// `new` exists the old file stays, and so it does when the move fails (a different volume, a
/// file in use). Nothing is copied and then removed, so a failure cannot lose either file.
pub fn migrate(old: &Path, new: &Path) -> Moved {
    if !old.exists() {
        return Moved::Nothing;
    }
    if new.exists() {
        return Moved::Left(format!(
            "{} stays where it is: {} already exists",
            old.display(),
            new.display()
        ));
    }
    if let Some(parent) = new.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return Moved::Left(format!(
                "{} stays where it is: creating {}: {error}",
                old.display(),
                parent.display()
            ));
        }
    }
    match std::fs::rename(old, new) {
        Ok(()) => Moved::Moved,
        Err(error) => Moved::Left(format!(
            "{} stays where it is: moving it to {}: {error}",
            old.display(),
            new.display()
        )),
    }
}

/// Moves a user's Android disk from where the first releases kept it
/// (`<machine root>\users\<SID>\data.vhdx`) into their own root as `android.vhdx`, with the
/// image version recorded beside it. A disk already in the user's root is never replaced: the
/// old one then stays where it is, and so it does when the move fails. Returns what to log.
pub fn migrate_user_disk(machine: &MachineRoot, user: &UserRoot, sid: &str) -> Vec<String> {
    let old = machine.old_user_disk(sid);
    let mut notes = Vec::new();
    match migrate(&old, &user.android_disk()) {
        Moved::Nothing => return notes,
        Moved::Left(why) => {
            notes.push(why);
            return notes;
        }
        Moved::Moved => notes.push(format!(
            "moved {sid}'s Android disk from {} to {}",
            old.display(),
            user.android_disk().display()
        )),
    }
    if let Moved::Left(why) = migrate(
        &old.with_extension("image-version"),
        &user.android_disk_version(),
    ) {
        notes.push(why);
    }
    // The user's old folder, when nothing else is left in it.
    if let Some(folder) = old.parent() {
        let _ = std::fs::remove_dir(folder);
    }
    notes
}

/// The trace switches the empty files of earlier releases (`input-trace`, `resize-trace` in
/// either root) turn on, and the files that are there. The settings take them over; the files
/// go once the settings are saved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OldTraceMarkers {
    pub input_trace: bool,
    pub resize_trace: bool,
    pub found: Vec<PathBuf>,
}

/// Looks for the trace switch files among `markers`.
pub fn old_trace_markers(markers: &[PathBuf]) -> OldTraceMarkers {
    let mut old = OldTraceMarkers::default();
    for marker in markers.iter().filter(|marker| marker.is_file()) {
        let name = marker
            .to_string_lossy()
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match name.as_str() {
            "input-trace" => old.input_trace = true,
            "resize-trace" => old.resize_trace = true,
            _ => continue,
        }
        old.found.push(marker.clone());
    }
    old
}

/// Where Windows put each boot's initrd before it was kept in the machine's `tmp` folder: the
/// temporary folder of the account the service runs as.
pub fn old_boot_initrd(vm_name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{vm_name}.initrd"))
}

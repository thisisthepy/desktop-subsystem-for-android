//! What installing and upgrading need from dsa-hostd besides copying files.
//!
//! Before the package replaces its files, every DSA program still running from them has to
//! go, in every user's session: the window manager (`dsa-wm.exe`), the app window host and the
//! copies of it made for each Android app (in each user's `%LOCALAPPDATA%\DSA\apps`), and the
//! settings app. A program left running keeps its file in use, so Windows Installer cannot
//! replace it, and an old window manager then holds its single-instance lock against the new
//! one and speaks an older local API to the new service.
//!
//! After the files are in place, the window manager starts again for every signed-in user,
//! as it would at their next sign-in.

use std::path::{Path, PathBuf};

/// The programs in the program folder that run in a user's session and are ended. The
/// service is stopped by Windows Installer itself, and `dsa.exe` is left alone: it may be
/// the very command (`dsa update`) that is waiting for this installation to finish.
const SESSION_PROGRAMS: [&str; 4] = [
    "dsa-wm.exe",
    "dsa-compositor.exe",
    "dsa-app-window.exe",
    "dsasettings.exe",
];

/// Whether the program at `image` is one of DSA's that runs in a user's session: the window
/// manager, the app window host or the settings app in the program folder (or below it), or
/// any program in one of the users' app window folders. Paths are compared without regard to
/// case and to the separator, as Windows compares them.
pub fn is_dsa_program(image: &Path, program_folder: &Path, app_folders: &[PathBuf]) -> bool {
    let image = normalised(image);
    let below = |folder: &str| {
        !folder.is_empty()
            && image.len() > folder.len()
            && image.starts_with(folder)
            && image[folder.len()..].starts_with('\\')
    };
    let name = image.rsplit('\\').next().unwrap_or_default();
    let in_program = below(&normalised(program_folder)) && SESSION_PROGRAMS.contains(&name);
    in_program || app_folders.iter().any(|folder| below(&normalised(folder)))
}

fn normalised(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\').to_string()
}

/// The folders each user's app windows are made in, for every user with a profile.
#[cfg(windows)]
pub fn app_folders_of_every_user() -> Vec<PathBuf> {
    use crate::paths::{default_local_app_data, UserRoot};
    let mut folders = Vec::new();
    for (sid, profile) in crate::profile::profiles() {
        let default_local = default_local_app_data(&profile);
        let local = crate::profile::local_app_data(&sid).unwrap_or_else(|_| default_local.clone());
        folders.push(UserRoot::in_local_app_data(&local).apps());
        if local != default_local {
            folders.push(UserRoot::in_local_app_data(&default_local).apps());
        }
    }
    folders
}

/// Ends every DSA program running from `program_folder` or from a user's app window folder,
/// in every session, and waits for each to be gone. Returns what could not be ended, to
/// report; the installer goes on either way, and asks for a restart if a file stays in use.
#[cfg(windows)]
pub fn stop_dsa_programs(program_folder: &Path) -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    let apps = app_folders_of_every_user();
    let own = std::process::id();
    let mut problems = Vec::new();
    // SAFETY: a snapshot of every process, closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return vec![format!(
            "listing the running programs: {}",
            std::io::Error::last_os_error()
        )];
    }
    // SAFETY: a plain structure; zero is its documented initial state with its size set.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: the snapshot above and a structure whose size is set.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let pid = entry.th32ProcessID;
        if pid != own && pid != 0 {
            // SAFETY: opening a process by id; a null handle means it could not be opened.
            let process = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if !process.is_null() {
                let mut name = vec![0u16; 32768];
                let mut length = name.len() as u32;
                // SAFETY: a buffer of `length` characters, which the call updates.
                let named = unsafe {
                    QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut length)
                };
                if named != 0 {
                    let image = PathBuf::from(String::from_utf16_lossy(&name[..length as usize]));
                    if is_dsa_program(&image, program_folder, &apps) {
                        // SAFETY: a process handle opened with the right to end it.
                        let ended = unsafe { TerminateProcess(process, 1) } != 0;
                        // SAFETY: the same handle, opened with the right to wait on it.
                        let gone = ended
                            && unsafe { WaitForSingleObject(process, 10_000) } == WAIT_OBJECT_0;
                        if !gone {
                            problems.push(format!(
                                "{} (process {pid}) could not be ended: {}",
                                image.display(),
                                std::io::Error::last_os_error()
                            ));
                        }
                    }
                }
                // SAFETY: the handle OpenProcess returned, closed once.
                unsafe { CloseHandle(process) };
            }
        }
        // SAFETY: as for Process32FirstW.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot, closed once.
    unsafe { CloseHandle(snapshot) };
    problems
}

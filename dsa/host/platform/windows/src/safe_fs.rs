//! File operations the service makes as SYSTEM in a folder its user can change: the user's
//! `%LOCALAPPDATA%\DSA` and profile.
//!
//! The service keeps what it writes itself in the machine's folder, which only SYSTEM and
//! Administrators write. What has to stay in the user's folder (the Android disk, which is the
//! user's data, its image version and the sealed keys) is used through these:
//!
//! - every folder from the user's root down to the file is opened as itself, never through a
//!   link ([`guard_below`]): one that is a reparse point (a junction, a symbolic link, a mount
//!   point) is refused, and the folder's real path must be the one the service expected. The
//!   folders stay open, without the right to move or remove them, for as long as the service
//!   works in them, so they stay the folders that were checked;
//! - a file is opened the same way ([`check_plain_file`], [`open_existing`]): one that is a
//!   reparse point, or has more than one name (a hard link), is refused;
//! - a file the service moves out of the user's folder is read and removed through the one
//!   handle it checked ([`open_existing`], [`delete_open`]), never by its path again;
//! - work that belongs to the user (their backup, their app programs, the logs their own
//!   processes wrote) is done with the user's own rights ([`as_user`]).

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_TOKEN, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{ImpersonateLoggedOnUser, TOKEN_IMPERSONATE, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileDispositionInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, OPEN_EXISTING,
    SYNCHRONIZE, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken, SetThreadToken};

pub use crate::safe_paths::{is_below, same_path};

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// Opens `path` itself, never what a reparse point at it leads to.
fn open_itself(path: &Path, access: u32, share: u32) -> io::Result<File> {
    let name = wide(path);
    // SAFETY: a NUL-terminated path; the handle is owned by the File returned.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle CreateFileW opened, owned by nothing else.
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: an open handle and a structure the call fills.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// The path Windows finds `file` at, all links followed.
pub fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 1024];
    loop {
        // SAFETY: a buffer of the length given.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle() as HANDLE,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            return Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..length])));
        }
        buffer.resize(length + 1, 0);
    }
}

/// Refuses `file`, opened at `path`, unless it is a plain folder (`folder`) or a plain file
/// with one name, at the path the service asked for.
fn check(file: &File, path: &Path, folder: bool) -> Result<(), String> {
    let info = information(file).map_err(|e| format!("reading {}: {e}", path.display()))?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "{} is a link to somewhere else, which the service does not follow",
            path.display()
        ));
    }
    let is_folder = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_folder != folder {
        return Err(format!(
            "{} is a {}, where the service expected a {}",
            path.display(),
            if is_folder { "folder" } else { "file" },
            if folder { "folder" } else { "file" }
        ));
    }
    if !folder && info.nNumberOfLinks > 1 {
        return Err(format!(
            "{} has other names on the disk (a hard link), so the service leaves it alone",
            path.display()
        ));
    }
    let real = final_path(file).map_err(|e| format!("reading {}: {e}", path.display()))?;
    if !same_path(&real, path) {
        return Err(format!(
            "{} is really {}, so the service leaves it alone",
            path.display(),
            real.display()
        ));
    }
    Ok(())
}

/// A folder the service checked and holds open, so that it cannot be moved, removed or
/// replaced while the service works in it.
#[derive(Debug)]
pub struct GuardedDir {
    _folder: File,
}

impl GuardedDir {
    /// Opens the folder at `path` as itself and checks it; see the module's description.
    pub fn open(path: &Path) -> Result<GuardedDir, String> {
        // Reading the folder's list takes part in sharing, so leaving out the right to delete
        // keeps anyone from moving or removing it while it is open.
        let folder = open_itself(
            path,
            FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
        check(&folder, path, true)?;
        Ok(GuardedDir { _folder: folder })
    }
}

/// Guards `root` and every folder below it down to `inner`, which must be `root` or inside it.
/// Each must exist.
pub fn guard_below(root: &Path, inner: &Path) -> Result<Vec<GuardedDir>, String> {
    if !is_below(inner, root) {
        return Err(format!(
            "{} is not inside {}",
            inner.display(),
            root.display()
        ));
    }
    let mut chain = vec![inner.to_path_buf()];
    let mut at = inner;
    while !same_path(at, root) {
        at = at
            .parent()
            .ok_or_else(|| format!("{} is not inside {}", inner.display(), root.display()))?;
        chain.push(at.to_path_buf());
    }
    chain.reverse();
    chain.iter().map(|dir| GuardedDir::open(dir)).collect()
}

/// Guards the folders from `root` down to the folder of each of `files` (see [`guard_below`]),
/// and checks each file there is a plain file with one name. A file that is not there passes.
pub fn guard_files(root: &Path, files: &[&Path]) -> Result<Vec<GuardedDir>, String> {
    let mut guards = Vec::new();
    for file in files {
        let folder = file
            .parent()
            .ok_or_else(|| format!("{} has no folder", file.display()))?;
        guards.extend(guard_below(root, folder)?);
        check_plain_file(file)?;
    }
    Ok(guards)
}

/// Checks the file at `path` is a plain file with one name at that path. Says whether it is
/// there.
pub fn check_plain_file(path: &Path) -> Result<bool, String> {
    match open_itself(
        path,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    ) {
        Ok(file) => check(&file, path, false).map(|()| true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("opening {}: {e}", path.display())),
    }
}

/// Opens the plain file at `path` to read it and then remove it, checked as
/// [`check_plain_file`] checks, and so that nobody else writes or removes it meanwhile. `None`
/// when it is not there.
pub fn open_existing(path: &Path) -> Result<Option<File>, String> {
    match open_itself(path, GENERIC_READ | DELETE | SYNCHRONIZE, FILE_SHARE_READ) {
        Ok(file) => {
            check(&file, path, false)?;
            Ok(Some(file))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("opening {}: {e}", path.display())),
    }
}

/// Removes the file `file` was opened on by [`open_existing`], through that handle: it goes
/// when the handle is closed.
pub fn delete_open(file: &File) -> Result<(), String> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: a handle opened with the right to delete, and a structure of the size given.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(format!("removing it: {}", io::Error::last_os_error()));
    }
    Ok(())
}

/// Removes the plain file at `path` the way [`open_existing`] and [`delete_open`] do. Says
/// whether there was one.
pub fn remove_plain_file(path: &Path) -> Result<bool, String> {
    match open_existing(path)? {
        Some(file) => {
            delete_open(&file).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Runs `work` on this thread with the rights of the user with SID `sid`, from the token of a
/// session they are signed in to, or as this thread already acts when it is already theirs.
/// The thread acts as it did before afterwards, also when it was acting as someone already,
/// and also when `work` panics.
pub fn as_user<T>(sid: &str, work: impl FnOnce() -> T) -> Result<T, String> {
    let restore = Restore::capture()?;
    let already_user = if restore.0.is_null() {
        crate::pipe::current_user_sid().is_ok_and(|me| me == sid)
    } else {
        crate::pipe::sid_of_token(restore.0).is_ok_and(|me| me == sid)
    };
    if already_user {
        return Ok(work());
    }
    if !restore.0.is_null() {
        // Querying a session token takes the process's own rights; `restore` puts the
        // previous thread token back on every path out.
        // SAFETY: ends the current thread's impersonation while `restore` holds the token.
        unsafe { SetThreadToken(std::ptr::null(), std::ptr::null_mut()) };
    }
    let token = crate::window_manager::user_token(sid)
        .map_err(|e| format!("{sid} is not signed in, so this is not done as them ({e})"))?;
    // SAFETY: the user's token; this thread acts as the user until `restore` is dropped.
    if unsafe { ImpersonateLoggedOnUser(token.0) } == 0 {
        return Err(format!(
            "cannot act as {sid}: {}",
            io::Error::last_os_error()
        ));
    }
    let result = work();
    drop(restore);
    Ok(result)
}

/// What this thread acted as, put back on drop.
struct Restore(HANDLE);

impl Restore {
    fn capture() -> Result<Restore, String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: this thread's own token, opened as the process so it can always be read.
        let ok = unsafe {
            OpenThreadToken(
                GetCurrentThread(),
                TOKEN_IMPERSONATE | TOKEN_QUERY,
                1,
                &mut token,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(format!("reading this thread's token: {error}"));
            }
            token = std::ptr::null_mut();
        }
        Ok(Restore(token))
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: a null token ends the impersonation; any other is the one this thread had.
        unsafe {
            SetThreadToken(std::ptr::null(), self.0);
            if !self.0.is_null() {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-safe-fs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nfr5_guarded_dir_holds_plain_folders_and_refuses_junctions_and_files() {
        let dir = scratch("dirs");
        let root = dir.join("DSA");
        let sub = root.join("keys");
        fs::create_dir_all(&sub).unwrap();

        let guards = guard_below(&root, &sub).unwrap();
        assert_eq!(guards.len(), 2);
        // While held, neither folder can be moved or removed.
        assert!(fs::rename(&sub, root.join("keys-moved")).is_err());
        assert!(fs::remove_dir(&sub).is_err());
        drop(guards);
        assert!(fs::rename(&sub, root.join("keys-moved")).is_ok());
        fs::rename(root.join("keys-moved"), &sub).unwrap();

        let outside = dir.join("other");
        fs::create_dir_all(&outside).unwrap();
        let why = guard_below(&root, &outside).unwrap_err();
        assert!(why.contains("is not inside"), "{why}");

        let plain_file = root.join("file.txt");
        fs::write(&plain_file, b"hello").unwrap();
        let why = GuardedDir::open(&plain_file).unwrap_err();
        assert!(why.contains("expected a folder"), "{why}");

        let junction = root.join("linked");
        let status = Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &junction.to_string_lossy(),
                &outside.to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        let why = GuardedDir::open(&junction).unwrap_err();
        assert!(why.contains("link to somewhere else"), "{why}");
        let why = guard_below(&root, &junction).unwrap_err();
        assert!(why.contains("link to somewhere else"), "{why}");
        let _ = fs::remove_dir(&junction);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr5_plain_file_checks_refuse_hard_links_and_remove_through_the_checked_handle() {
        let dir = scratch("files");
        let file = dir.join("android.vhdx");
        assert!(!check_plain_file(&file).unwrap());
        fs::write(&file, b"disk").unwrap();
        assert!(check_plain_file(&file).unwrap());

        let second = dir.join("second-name.vhdx");
        fs::hard_link(&file, &second).unwrap();
        let why = check_plain_file(&file).unwrap_err();
        assert!(why.contains("hard link"), "{why}");
        assert!(open_existing(&file).is_err());
        assert!(remove_plain_file(&file).is_err());
        assert!(file.exists() && second.exists());

        fs::remove_file(&second).unwrap();
        assert!(check_plain_file(&file).unwrap());
        assert!(remove_plain_file(&file).unwrap());
        assert!(!file.exists());
        assert!(!remove_plain_file(&file).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr5_as_user_runs_for_this_user_and_refuses_an_unsigned_in_sid() {
        let me = crate::pipe::current_user_sid().unwrap();
        let out = as_user(&me, || as_user(&me, || 42)).unwrap().unwrap();
        assert_eq!(out, 42);
        let why = as_user("S-1-5-21-1-2-3-999999", || ()).unwrap_err();
        assert!(why.contains("is not signed in"), "{why}");
    }
}

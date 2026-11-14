//! Running a downloaded `setup.exe` to update DSA.
//!
//! Setup stops the DsaHost service, replaces it and starts the new one, so it must not
//! be a part of the service that it stops. It runs as a process of its own: detached from
//! the service's console, in a new process group, and out of the service's job when the
//! job lets it leave (a service is usually in no job at all). It inherits no handles,
//! so it holds nothing of the service open.
//!
//! The command line is `"<setup>" /quiet /norestart /log "<log>"`: no screens, no
//! restart of the machine even when Windows asks for one, and a log the service names in
//! what it tells the user.

use std::path::Path;

use dsa_host_core::update::program_service::SetupOutcome;

/// Windows Installer's "the installation worked; restart to finish it" (setup ends with it
/// when a file was in use and is replaced at the next start).
pub const ERROR_SUCCESS_REBOOT_REQUIRED: u32 = 3010;
/// "The installation worked, and setup has started the restart" (not with `/norestart`, but
/// it means installed all the same).
pub const ERROR_SUCCESS_REBOOT_INITIATED: u32 = 1641;
/// "The installation worked; restart to use it", the bundle's own form of 3010.
pub const ERROR_SUCCESS_RESTART_REQUIRED: u32 = 3011;

/// What setup's exit code says. Windows Installer and its setup programs end with 0, or with
/// one of the codes that say the installation worked and a restart would tidy up after it;
/// every other code is a failure.
pub fn setup_outcome(code: u32) -> SetupOutcome {
    match code {
        0 => SetupOutcome::Installed,
        ERROR_SUCCESS_REBOOT_REQUIRED
        | ERROR_SUCCESS_REBOOT_INITIATED
        | ERROR_SUCCESS_RESTART_REQUIRED => SetupOutcome::InstalledRestartPending,
        _ => SetupOutcome::Failed,
    }
}

/// Names a running process so it can be found again after this one ends: its id, which
/// Windows hands out again once it ends, and the moment it was created, which tells two
/// processes of one id apart.
pub fn process_key(pid: u32, created: u64) -> String {
    format!("{pid}:{created}")
}

/// Reads back what [`process_key`] wrote.
pub fn parse_process_key(key: &str) -> Option<(u32, u64)> {
    let (pid, created) = key.split_once(':')?;
    Some((pid.parse().ok()?, created.parse().ok()?))
}

/// The command line that restarts the service from `program` (the dsa-hostd installed now):
/// it waits for the service to stop, ends DSA's programs left running from the old files in
/// every session, starts the service and every signed-in user's window manager.
pub fn restart_command_line(program: &Path) -> String {
    format!("\"{}\" restart-service", program.display())
}

/// Quotes `argument` so that a program parsing its command line the usual Windows way
/// (`CommandLineToArgvW`, the C runtime) reads it back unchanged.
pub fn quote_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\x0b', '"']) {
        return argument.to_string();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The command line that runs `setup` quietly, logging to `log`. A program's own name is
/// read up to its closing quote, and a path holds no quotes, so it is only wrapped.
pub fn setup_command_line(setup: &Path, log: &Path) -> String {
    format!(
        "\"{}\" /quiet /norestart /log {}",
        setup.display(),
        quote_argument(&log.to_string_lossy())
    )
}

#[cfg(windows)]
pub use windows_impl::{
    file_version, reattach, spawn_detached, windows_installer_running, DetachedProcess,
    SetupInstaller,
};

#[cfg(windows)]
mod windows_impl {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use dsa_host_core::update::program_service::{ProgramInstaller, RunningSetup, SetupOutcome};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME, HANDLE, STILL_ACTIVE,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, GetProcessId, GetProcessTimes, OpenMutexW, OpenProcess,
        WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
        INFINITE, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        STARTUPINFOW,
    };

    use super::{
        parse_process_key, process_key, restart_command_line, setup_command_line, setup_outcome,
    };

    /// `SYNCHRONIZE`, the right to wait on an object, here a mutex.
    const SYNCHRONIZE: u32 = 0x0010_0000;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A process started by [`spawn_detached`]. Dropping it closes the handle and leaves
    /// the process running.
    pub struct DetachedProcess {
        process: HANDLE,
    }

    // SAFETY: a process handle may be used and closed from any thread.
    unsafe impl Send for DetachedProcess {}

    impl DetachedProcess {
        /// Waits for the process to end and returns its exit code.
        pub fn wait(&self) -> Result<u32, String> {
            // SAFETY: a process handle this value owns.
            if unsafe { WaitForSingleObject(self.process, INFINITE) } != WAIT_OBJECT_0 {
                return Err(format!(
                    "waiting for setup: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut code = 0u32;
            // SAFETY: as above, and a valid out pointer.
            if unsafe { GetExitCodeProcess(self.process, &mut code) } == 0 {
                return Err(format!(
                    "reading setup's exit code: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(code)
        }
    }

    impl Drop for DetachedProcess {
        fn drop(&mut self) {
            // SAFETY: closed once; the process goes on.
            unsafe { CloseHandle(self.process) };
        }
    }

    impl RunningSetup for DetachedProcess {
        fn wait(self: Box<Self>) -> Result<u32, String> {
            DetachedProcess::wait(&self)
        }

        fn id(&self) -> Option<String> {
            // SAFETY: a process handle this value owns.
            let pid = unsafe { GetProcessId(self.process) };
            if pid == 0 {
                return None;
            }
            Some(process_key(pid, created(self.process)?))
        }
    }

    /// When `process` was created, in the units Windows keeps it in.
    fn created(process: HANDLE) -> Option<u64> {
        let mut creation = FILETIME::default();
        let (mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: a process handle with query rights, and four out structures.
        let read =
            unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
        (read != 0)
            .then(|| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }

    /// The process [`process_key`] named, while it still runs.
    pub fn reattach(key: &str) -> Option<DetachedProcess> {
        let (pid, wanted) = parse_process_key(key)?;
        // SAFETY: opening a process by id; a null handle means it is gone or out of reach.
        let process = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if process.is_null() {
            return None;
        }
        let found = DetachedProcess { process };
        let mut code = 0u32;
        // SAFETY: the handle above, and a valid out pointer.
        let running = unsafe { GetExitCodeProcess(found.process, &mut code) } != 0
            && code == STILL_ACTIVE as u32;
        (running && created(found.process) == Some(wanted)).then_some(found)
    }

    /// The version of the file at `path`: its version information's `ProductVersion` text
    /// when that is a version (such as `0.2.3`, or `0.2.4-dev.7` for a development build),
    /// otherwise its fixed version numbers, as `a.b.c.d`.
    pub fn file_version(path: &Path) -> Option<String> {
        let name: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: a NUL-terminated name; the handle argument is unused.
        let size = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), std::ptr::null_mut()) };
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        // SAFETY: a buffer of the size Windows asked for.
        if unsafe { GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
            return None;
        }
        // The release's own version as text, which a development build's fixed numbers
        // cannot hold (0.2.4-dev.7 would read as 0.2.4.0, newer than itself). DSA's programs
        // carry it in English (United States), Unicode.
        let text_key = wide("\\StringFileInfo\\040904B0\\ProductVersion");
        let mut text: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut chars = 0u32;
        // SAFETY: the block read above, and out pointers into it.
        let has_text = unsafe {
            VerQueryValueW(
                data.as_ptr().cast(),
                text_key.as_ptr(),
                &mut text,
                &mut chars,
            )
        };
        if has_text != 0 && !text.is_null() && chars > 0 {
            // SAFETY: VerQueryValueW points at `chars` UTF-16 units inside `data`, which lives on.
            let units = unsafe { std::slice::from_raw_parts(text as *const u16, chars as usize) };
            let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
            let version = String::from_utf16_lossy(&units[..end]).trim().to_string();
            if dsa_host_core::update::program::Version::parse(&version).is_ok() {
                return Some(version);
            }
        }
        let root = wide("\\");
        let mut value: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut length = 0u32;
        // SAFETY: the block read above, and out pointers into it.
        let found =
            unsafe { VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut value, &mut length) };
        if found == 0
            || value.is_null()
            || (length as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        // SAFETY: VerQueryValueW points at a VS_FIXEDFILEINFO inside `data`, which lives on.
        let fixed = unsafe { std::ptr::read_unaligned(value as *const VS_FIXEDFILEINFO) };
        let (ms, ls) = (fixed.dwFileVersionMS, fixed.dwFileVersionLS);
        Some(format!(
            "{}.{}.{}.{}",
            ms >> 16,
            ms & 0xFFFF,
            ls >> 16,
            ls & 0xFFFF
        ))
    }

    /// Whether Windows Installer is installing something now: it holds this mutex for as
    /// long as an installation runs.
    pub fn windows_installer_running() -> bool {
        let name = wide("Global\\_MSIExecute");
        // SAFETY: a NUL-terminated name; a null handle means there is no such mutex.
        let mutex = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
        if mutex.is_null() {
            return false;
        }
        // SAFETY: the handle just opened, closed once.
        unsafe { CloseHandle(mutex) };
        true
    }

    /// Starts `program` with the whole `command_line` (which names the program first) in
    /// the folder `program` is in, apart from this process. Leaves this process's job when
    /// the job allows it and starts inside it otherwise.
    pub fn spawn_detached(program: &Path, command_line: &str) -> Result<DetachedProcess, String> {
        let application = wide(&program.to_string_lossy());
        let folder = program
            .parent()
            .map(|folder| wide(&folder.to_string_lossy()));
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        let mut last = None;
        for flags in [base | CREATE_BREAKAWAY_FROM_JOB, base] {
            // CreateProcessW may write to the command line, so each try gets a fresh copy.
            let mut line = wide(command_line);
            let startup = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            // SAFETY: NUL-terminated strings and structures that outlive the call; no
            // handles are inherited.
            let created = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    flags,
                    std::ptr::null(),
                    folder.as_ref().map_or(std::ptr::null(), |f| f.as_ptr()),
                    &startup,
                    &mut info,
                )
            };
            if created != 0 {
                // SAFETY: the thread handle is not needed.
                unsafe { CloseHandle(info.hThread) };
                return Ok(DetachedProcess {
                    process: info.hProcess,
                });
            }
            // SAFETY: reads this thread's last error.
            let code = unsafe { GetLastError() };
            last = Some(std::io::Error::from_raw_os_error(code as i32));
            // Only a job that forbids leaving it is worth a second try without breakaway.
            if code != ERROR_ACCESS_DENIED {
                break;
            }
        }
        Err(format!(
            "starting {}: {}",
            program.display(),
            last.expect("CreateProcessW was tried")
        ))
    }

    /// Runs a setup file as [`ProgramInstaller`] asks: quietly, without restarting the
    /// machine, logging to the given file, apart from the service.
    ///
    /// With [`for_service`](Self::for_service) it also restarts the service it is part of
    /// into the program files setup installed, which a service still running after setup
    /// needs: the files are new, the process is the old one.
    #[derive(Default)]
    pub struct SetupInstaller {
        service: Option<ServiceRestart>,
    }

    struct ServiceRestart {
        /// The installed dsa-hostd, which setup replaces.
        program: PathBuf,
        /// The service's stop flag.
        stop: Arc<AtomicBool>,
    }

    impl SetupInstaller {
        /// For the service that runs from `program` and stops when `stop` is set.
        pub fn for_service(program: PathBuf, stop: Arc<AtomicBool>) -> SetupInstaller {
            SetupInstaller {
                service: Some(ServiceRestart { program, stop }),
            }
        }
    }

    impl ProgramInstaller for SetupInstaller {
        fn install(&self, setup: &Path, log: &Path) -> Result<Box<dyn RunningSetup>, String> {
            let process = spawn_detached(setup, &setup_command_line(setup, log))?;
            Ok(Box::new(process))
        }

        fn outcome(&self, code: u32) -> SetupOutcome {
            setup_outcome(code)
        }

        fn reattach(&self, id: &str) -> Option<Box<dyn RunningSetup>> {
            reattach(id).map(|process| Box::new(process) as Box<dyn RunningSetup>)
        }

        fn installed_version(&self) -> Option<String> {
            file_version(&self.service.as_ref()?.program)
        }

        fn installation_running(&self) -> bool {
            windows_installer_running()
        }

        /// Starts the installed dsa-hostd to wait for this service to stop and start it
        /// again, then stops this service. The helper runs apart from the service, as setup
        /// does, so it outlives it.
        fn restart_into_installed(&self) -> Result<(), String> {
            let service = self
                .service
                .as_ref()
                .ok_or("this is not the service, so it cannot restart it")?;
            spawn_detached(&service.program, &restart_command_line(&service.program))?;
            service.stop.store(true, Ordering::Relaxed);
            Ok(())
        }
    }
}

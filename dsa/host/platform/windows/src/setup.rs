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
pub use windows_impl::{spawn_detached, DetachedProcess, SetupInstaller};

#[cfg(windows)]
mod windows_impl {
    use std::path::Path;

    use dsa_host_core::update::program_service::{ProgramInstaller, RunningSetup};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB,
        CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
    };

    use super::setup_command_line;

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
    pub struct SetupInstaller;

    impl ProgramInstaller for SetupInstaller {
        fn install(&self, setup: &Path, log: &Path) -> Result<Box<dyn RunningSetup>, String> {
            let process = spawn_detached(setup, &setup_command_line(setup, log))?;
            Ok(Box::new(process))
        }
    }
}

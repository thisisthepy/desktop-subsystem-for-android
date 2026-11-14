//! The administrator approval of an Android app's su call on Windows: the UAC prompt, shown in
//! the user's session under the app's own window program, so it names the app and shows its
//! icon.
//!
//! The service runs as SYSTEM in session 0, where no prompt can be seen. For each request it
//! [`UacPrompt`]:
//! 1. creates a one-off pipe, `\\.\pipe\dsa-root-approval-<random>`, that only SYSTEM and an
//!    elevated administrator may open (a token UAC filtered has the Administrators group for
//!    deny only, so it is refused);
//! 2. starts the app's window program (`%LOCALAPPDATA%\DSA\apps\<package>\<Label>.exe`, whose
//!    version information names the app and whose icon is the app's) in the user's session, as
//!    the user, with [`APPROVAL_ARGUMENT`] and the pipe's name;
//! 3. that program asks Windows to run itself elevated (`ShellExecuteEx` with `runas`), which
//!    brings up the UAC prompt with the program's name and icon. An administrator clicks Yes; a
//!    standard user has to type an administrator's password. Cancelling ends the program with
//!    [`exit::REFUSED`];
//! 4. the elevated copy ([`APPROVED_ARGUMENT`]) opens the pipe and sends its name back. The
//!    service approves only an answer that came through the pipe from a client Windows says is
//!    an administrator acting with its rights. Nothing the unelevated program says can approve:
//!    its exit code can only refuse.
//!
//! With UAC turned off, Windows shows no prompt. The program then asks in its own dialog, with
//! the app's name and icon, and only when the user is one of the computer's administrators;
//! otherwise it refuses ([`exit::NOT_ADMINISTRATOR`]).
//!
//! A prompt nobody answers in time is ended, and the request times out. The UAC dialog itself
//! belongs to Windows and may stay open; answering it after that runs nothing, as the pipe is
//! gone.
//!
//! [`wait_for_answer`] and the argument and exit code handling are platform neutral, so they are
//! tested without a real prompt.

use std::time::{Duration, Instant};

use dsa_host_core::root_approval::PromptAnswer;
use dsa_transport::local::LocalListener;
use dsa_transport::TransportError;

/// The argument the service starts an app's window program with to ask for approval. The
/// pipe's name follows.
pub const APPROVAL_ARGUMENT: &str = "--root-approval";

/// The argument the elevated copy runs with. The pipe's name follows.
pub const APPROVED_ARGUMENT: &str = "--root-approved";

/// The argument with the app's name, hex-encoded UTF-8, for the program's own dialog.
pub const LABEL_ARGUMENT: &str = "--root-label";

/// The argument with the command su runs, in words, hex-encoded UTF-8, for the program's own
/// dialog.
pub const COMMAND_ARGUMENT: &str = "--root-command";

/// What the start of every approval pipe's name is.
pub const PIPE_PREFIX: &str = "dsa-root-approval-";

/// Only SYSTEM and an elevated administrator may open an approval pipe.
pub const APPROVAL_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";

/// What the service answers an accepted approval with, so the elevated copy knows it arrived.
pub const RECEIVED: &[u8] = b"received";

/// How long the service waits for a connected client to say the pipe's name.
const ANSWER_READ: Duration = Duration::from_secs(5);

/// How often the service looks at the program while it waits for a connection.
const POLL: Duration = Duration::from_millis(200);

/// How the window program ends when it asks for approval.
pub mod exit {
    /// The elevated copy delivered the approval and the service received it.
    pub const APPROVED: u32 = 0;
    /// Something went wrong that the program could not say more about.
    pub const FAILED: u32 = 1;
    /// The UAC prompt, or the program's own dialog, was cancelled or refused.
    pub const REFUSED: u32 = 2;
    /// UAC is off and the user is not one of the computer's administrators.
    pub const NOT_ADMINISTRATOR: u32 = 3;
    /// The elevated copy ran but could not reach the service.
    pub const NOT_DELIVERED: u32 = 4;
    /// Windows could not run the program elevated, for a reason other than a refusal.
    pub const ELEVATION_FAILED: u32 = 5;
    /// The arguments did not name a valid approval pipe.
    pub const BAD_ARGUMENTS: u32 = 6;
}

/// What the window program's exit code says, when it ended before an approval arrived.
pub fn answer_of_exit(code: u32) -> PromptAnswer {
    match code {
        exit::REFUSED => PromptAnswer::Denied,
        exit::NOT_ADMINISTRATOR => PromptAnswer::NotAdministrator,
        exit::APPROVED => {
            PromptAnswer::Unavailable("the approval prompt ended without an answer".to_string())
        }
        exit::NOT_DELIVERED => {
            PromptAnswer::Unavailable("the approval could not reach the DSA service".to_string())
        }
        exit::ELEVATION_FAILED => PromptAnswer::Unavailable(
            "Windows could not show the User Account Control prompt".to_string(),
        ),
        exit::BAD_ARGUMENTS => {
            PromptAnswer::Unavailable("the approval prompt was started wrongly".to_string())
        }
        other => PromptAnswer::Unavailable(format!("the approval prompt ended with code {other}")),
    }
}

/// The name of an approval pipe made from `secret`.
pub fn pipe_name(secret: &[u8]) -> String {
    format!("{PIPE_PREFIX}{}", hex(secret))
}

/// Whether `name` is the name of an approval pipe: the prefix and lowercase hex digits only,
/// so it can go on a command line unquoted.
pub fn is_pipe_name(name: &str) -> bool {
    name.strip_prefix(PIPE_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

/// The arguments that start an app's window program to ask for approval on `pipe`.
pub fn approval_arguments(pipe: &str, label: &str, command: &str) -> String {
    format!(
        "{APPROVAL_ARGUMENT} {pipe} {LABEL_ARGUMENT} {} {COMMAND_ARGUMENT} {}",
        hex(label.as_bytes()),
        hex(command.as_bytes())
    )
}

/// What a window program started for approval was asked: the pipe, the app's name and the
/// command. `None` when the arguments ask for no approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperMode {
    /// Not elevated: bring up the prompt.
    Ask {
        pipe: String,
        label: String,
        command: String,
    },
    /// Elevated: deliver the approval.
    Deliver { pipe: String },
    /// An approval was asked for, but not with a valid pipe.
    Invalid,
}

/// What `args` ask of the window program, when they ask for an approval.
pub fn helper_mode(args: &[String]) -> Option<HelperMode> {
    let value = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let text = |flag: &str| {
        value(flag)
            .and_then(|hex| unhex(&hex))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    if args.iter().any(|arg| arg == APPROVED_ARGUMENT) {
        return Some(match value(APPROVED_ARGUMENT) {
            Some(pipe) if is_pipe_name(&pipe) => HelperMode::Deliver { pipe },
            _ => HelperMode::Invalid,
        });
    }
    if args.iter().any(|arg| arg == APPROVAL_ARGUMENT) {
        return Some(match value(APPROVAL_ARGUMENT) {
            Some(pipe) if is_pipe_name(&pipe) => HelperMode::Ask {
                pipe,
                label: text(LABEL_ARGUMENT),
                command: text(COMMAND_ARGUMENT),
            },
            _ => HelperMode::Invalid,
        });
    }
    None
}

/// The window program the service started to ask, as the service waits on it.
pub trait PromptProcess {
    /// Its exit code once it has ended; `None` while it runs.
    fn exit_code(&mut self) -> Option<u32>;
    /// Ends it, when nobody answered in time.
    fn end(&mut self);
}

/// Waits at most `timeout` for the approval on `listener`: a client Windows says is an
/// administrator, that sends `secret` (the pipe's name). The program's exit before that says
/// why there is no approval. A client that is not an administrator, or says something else, is
/// ignored, so only a real elevation approves.
pub fn wait_for_answer(
    listener: &mut dyn LocalListener,
    secret: &[u8],
    helper: &mut dyn PromptProcess,
    timeout: Duration,
) -> PromptAnswer {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            helper.end();
            return PromptAnswer::TimedOut;
        }
        match listener.accept(left.min(POLL)) {
            Ok((mut connection, peer)) => {
                if !peer.administrator {
                    continue;
                }
                let read = left.min(ANSWER_READ);
                if let Ok(frame) = connection.recv(Some(read)) {
                    if frame == secret {
                        let _ = connection.send(RECEIVED);
                        return PromptAnswer::Approved;
                    }
                }
                continue;
            }
            Err(TransportError::Timeout) => {}
            Err(error) => {
                helper.end();
                return PromptAnswer::Unavailable(format!("the approval's pipe failed: {error}"));
            }
        }
        if let Some(code) = helper.exit_code() {
            return answer_of_exit(code);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
pub use self::os::{run_helper, UacPrompt};

#[cfg(windows)]
mod os {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use dsa_host_core::root_approval::{ApprovalPrompt, ApprovalRequest, PromptAnswer};
    use dsa_transport::Connector;
    use windows_sys::Win32::Foundation::{ERROR_CANCELLED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
    };

    use super::{
        approval_arguments, exit, helper_mode, pipe_name, wait_for_answer, HelperMode,
        PromptProcess, APPROVAL_PIPE_SDDL, APPROVED_ARGUMENT, RECEIVED,
    };
    use crate::pipe::{wide, NamedPipeConnector, NamedPipeListener, OwnedHandle};

    /// How long the elevated copy tries to reach the service.
    const DELIVER_WAIT: Duration = Duration::from_secs(5);

    /// The UAC prompt for the su calls of the user with SID `sid`.
    pub struct UacPrompt {
        sid: String,
    }

    impl UacPrompt {
        pub fn new(sid: &str) -> UacPrompt {
            UacPrompt {
                sid: sid.to_string(),
            }
        }

        /// The app's own window program, made when it is missing; the installed window host
        /// when it cannot be, which still brings up the prompt, under DSA's own name and icon.
        fn program(&self, request: &ApprovalRequest) -> Result<PathBuf, String> {
            let service = std::env::current_exe().ok();
            let template = crate::app_window::launch::window_manager_path(service.as_deref())
                .and_then(|wm| {
                    wm.parent()
                        .map(|dir| dir.join(crate::app_exe::TEMPLATE_NAME))
                })
                .filter(|path| path.is_file())
                .or_else(|| crate::app_exe::default_template().filter(|path| path.is_file()))
                .ok_or_else(|| {
                    format!(
                        "{} is not installed; repair the installation",
                        crate::app_exe::TEMPLATE_NAME
                    )
                })?;
            let package = &request.app.package_name;
            let apps_dir = crate::host_config::local_app_data(&self.sid)
                .map(|dir| crate::paths::UserRoot::in_local_app_data(&dir).apps());
            let apps_dir = match apps_dir {
                Ok(dir) => dir,
                Err(why) => {
                    eprintln!("root: asking under DSA's own name, as {package}'s program has no folder: {why}");
                    return Ok(template);
                }
            };
            let existing = crate::app_exe::program_path(&apps_dir, package, request.app_name());
            // The app's program lives in the user's own folder, so it is checked and made
            // with the user's own rights.
            let made = crate::safe_fs::as_user(&self.sid, || -> Result<PathBuf, String> {
                if existing.is_file() {
                    return Ok(existing);
                }
                crate::app_exe::ensure(&template, &apps_dir, package, Some(&request.app))
                    .map(|(program, _)| program)
            });
            match made {
                Ok(Ok(program)) => Ok(program),
                Ok(Err(why)) | Err(why) => {
                    eprintln!(
                        "root: asking under DSA's own name, as {package}'s program cannot be \
                         made: {why}"
                    );
                    Ok(template)
                }
            }
        }
    }

    impl ApprovalPrompt for UacPrompt {
        fn ask(&self, request: &ApprovalRequest, timeout: Duration) -> PromptAnswer {
            let secret = match dsa_protocol::auth::nonce() {
                Ok(secret) => secret,
                Err(why) => return PromptAnswer::Unavailable(why),
            };
            let pipe = pipe_name(&secret);
            let mut listener = match NamedPipeListener::bind(&pipe, APPROVAL_PIPE_SDDL) {
                Ok(listener) => listener,
                Err(error) => {
                    return PromptAnswer::Unavailable(format!("the approval pipe: {error}"))
                }
            };
            let program = match self.program(request) {
                Ok(program) => program,
                Err(why) => return PromptAnswer::Unavailable(why),
            };
            let arguments = approval_arguments(&pipe, request.app_name(), &request.command_text());
            let process =
                match crate::window_manager::start_as_user(&self.sid, &program, &arguments) {
                    Ok(process) => process,
                    Err(why) => return PromptAnswer::Unavailable(why),
                };
            let mut helper = WindowsProcess(process);
            wait_for_answer(&mut listener, pipe.as_bytes(), &mut helper, timeout)
        }
    }

    struct WindowsProcess(OwnedHandle);

    impl PromptProcess for WindowsProcess {
        fn exit_code(&mut self) -> Option<u32> {
            // SAFETY: a process handle this struct owns.
            if unsafe { WaitForSingleObject(self.0 .0, 0) } != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            // SAFETY: the same handle, and an out pointer that outlives the call.
            if unsafe { GetExitCodeProcess(self.0 .0, &mut code) } == 0 {
                return Some(exit::FAILED);
            }
            Some(code)
        }

        fn end(&mut self) {
            // SAFETY: a process handle this struct owns; ending an ended process does nothing.
            unsafe { TerminateProcess(self.0 .0, exit::FAILED) };
        }
    }

    /// Runs the window program's part of an approval when `args` ask for one, and returns its
    /// exit code; `None` when they do not, and the program goes on as a window host.
    pub fn run_helper(args: &[String], program: &Path) -> Option<u32> {
        Some(match helper_mode(args)? {
            HelperMode::Invalid => exit::BAD_ARGUMENTS,
            HelperMode::Deliver { pipe } => deliver(&pipe),
            HelperMode::Ask {
                pipe,
                label,
                command,
            } => {
                if uac_enabled() {
                    elevate(program, &pipe)
                } else if !crate::pipe::current_process_is_administrator() {
                    exit::NOT_ADMINISTRATOR
                } else if confirm(&label, &command) {
                    // With UAC off an administrator's programs run with its rights already.
                    deliver(&pipe)
                } else {
                    exit::REFUSED
                }
            }
        })
    }

    /// Sends the approval to the service, as the elevated copy.
    fn deliver(pipe: &str) -> u32 {
        let mut connector = NamedPipeConnector {
            name: pipe.to_string(),
        };
        let Ok(mut connection) = connector.connect(DELIVER_WAIT) else {
            return exit::NOT_DELIVERED;
        };
        if connection.send(pipe.as_bytes()).is_err() {
            return exit::NOT_DELIVERED;
        }
        match connection.recv(Some(DELIVER_WAIT)) {
            Ok(answer) if answer == RECEIVED => exit::APPROVED,
            _ => exit::NOT_DELIVERED,
        }
    }

    /// Whether UAC is on: `EnableLUA` is not 0. On when it cannot be read, as Windows has it by
    /// default.
    fn uac_enabled() -> bool {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD,
        };
        let key = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System");
        let value = wide("EnableLUA");
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: NUL-terminated names and a DWORD buffer of the size given.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u32).cast(),
                &mut size,
            )
        };
        status != 0 || data != 0
    }

    /// Runs this program elevated to deliver the approval, which brings up the UAC prompt with
    /// its name and icon, and waits for it. Its exit code is the answer.
    fn elevate(program: &Path, pipe: &str) -> u32 {
        use windows_sys::Win32::System::Com::{
            CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
        };
        use windows_sys::Win32::UI::Shell::{
            ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
        // SAFETY: this thread's COM, which the shell needs; kept for the life of the program.
        unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        let verb = wide("runas");
        let file = wide(&program.to_string_lossy());
        let parameters = wide(&format!("{APPROVED_ARGUMENT} {pipe}"));
        // SAFETY: plain data, zero is its documented initial state.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.nShow = SW_HIDE;
        // SAFETY: a filled structure whose strings outlive the call.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                exit::REFUSED
            } else {
                exit::ELEVATION_FAILED
            };
        }
        if info.hProcess.is_null() {
            return exit::ELEVATION_FAILED;
        }
        let process = OwnedHandle(info.hProcess);
        // The service ends this program when nobody answers in time.
        // SAFETY: the elevated process's handle, owned above.
        unsafe { WaitForSingleObject(process.0, u32::MAX) };
        let mut code = exit::FAILED;
        // SAFETY: the same handle, and an out pointer that outlives the call.
        if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
            return exit::FAILED;
        }
        code
    }

    /// Asks in the program's own dialog, with the app's icon, when UAC is off. True on Yes.
    fn confirm(label: &str, command: &str) -> bool {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxIndirectW, IDYES, MB_DEFBUTTON2, MB_SETFOREGROUND, MB_TOPMOST, MB_USERICON,
            MB_YESNO, MSGBOXPARAMSW,
        };
        let label = if label.trim().is_empty() {
            "An Android app"
        } else {
            label.trim()
        };
        let caption = wide(&format!("{label}: administrator approval"));
        let text = wide(&format!(
            "{label} wants to run this as root in Android:\n\n{command}\n\nUser Account \
             Control is off on this computer, so Desktop Subsystem for Android asks you \
             itself. Allow it?"
        ));
        // SAFETY: plain data, zero is its documented initial state.
        let mut params: MSGBOXPARAMSW = unsafe { std::mem::zeroed() };
        params.cbSize = std::mem::size_of::<MSGBOXPARAMSW>() as u32;
        // SAFETY: this program's own module, which carries the app's icon.
        params.hInstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        params.lpszText = text.as_ptr();
        params.lpszCaption = caption.as_ptr();
        params.dwStyle = MB_YESNO | MB_DEFBUTTON2 | MB_USERICON | MB_SETFOREGROUND | MB_TOPMOST;
        // The icon group by its resource number, as MAKEINTRESOURCE gives it.
        params.lpszIcon = dsa_exe_resources::ICON_GROUP_ID as usize as *const u16;
        // SAFETY: a filled structure whose strings outlive the call.
        unsafe { MessageBoxIndirectW(&params) == IDYES }
    }
}

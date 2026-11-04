//! Starting a user's window manager from the service.
//!
//! The service runs as SYSTEM in session 0, and a window manager there could show nothing, so it
//! has to run in the user's own session, as the user. SYSTEM may do that: it asks Windows for the
//! token of the session's user (`WTSQueryUserToken`, which is the limited token of an
//! administrator too, as a program started at sign-in has), and creates the process with it on
//! the session's desktop. That the window manager runs is read from its pipe, whose name says
//! which session it serves.
//!
//! `dsa-hostd serve`, run by hand, has no such right. It can start a window manager for its own
//! user, in its own session, as any program can.

use std::io;
use std::path::{Path, PathBuf};

use dsa_host_core::window_manager::WindowManagerHost;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PRIVILEGE_NOT_HELD, HANDLE};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSActive, WTSConnected, WTSDisconnected, WTSEnumerateSessionsW, WTSFreeMemory,
    WTSQueryUserToken, WTS_SESSION_INFOW,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::app_window::launch::{pipe_exists, run_program, window_manager_path};
use crate::compositor::current_session;
use crate::compositor::hosts::pipe_name;
use crate::pipe::{current_user_sid, token_user_sid, OwnedHandle};

/// One of the user's Windows sessions.
struct UserSession {
    id: u32,
    active: bool,
    token: OwnedHandle,
}

/// The sessions the user with SID `sid` is signed in to. An error when this process may not ask.
fn sessions_of(sid: &str) -> io::Result<Vec<UserSession>> {
    let mut list: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: this server (a null handle), and out pointers that outlive the call.
    if unsafe { WTSEnumerateSessionsW(std::ptr::null_mut(), 0, 1, &mut list, &mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Windows returned `count` entries at `list`, freed below.
    let infos = unsafe { std::slice::from_raw_parts(list, count as usize) };
    let mut sessions = Vec::new();
    let mut refused = None;
    for info in infos {
        let usable = [WTSActive, WTSConnected, WTSDisconnected].contains(&info.State);
        if !usable {
            continue;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: an out pointer that outlives the call.
        if unsafe { WTSQueryUserToken(info.SessionId, &mut token) } == 0 {
            let error = io::Error::last_os_error();
            // Most other failures just mean that no one is signed in to that session.
            if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) {
                refused = Some(error);
            }
            continue;
        }
        let token = OwnedHandle(token);
        if token_user_sid(&token).is_ok_and(|user| user == sid) {
            sessions.push(UserSession {
                id: info.SessionId,
                active: info.State == WTSActive,
                token,
            });
        }
    }
    // SAFETY: the list WTSEnumerateSessionsW allocated, freed once; `infos` is not used after.
    unsafe { WTSFreeMemory(list.cast()) };
    match refused {
        Some(error) => Err(error),
        None => Ok(sessions),
    }
}

/// Starts the window manager at `program` in every session someone is signed in to, as that
/// session's user, the way signing in starts it. A window manager already running in a
/// session keeps it, and the new one ends at once. Returns what could not be started.
pub fn start_for_signed_in_users(program: &Path) -> Vec<String> {
    let mut list: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: this server (a null handle), and out pointers that outlive the call.
    if unsafe { WTSEnumerateSessionsW(std::ptr::null_mut(), 0, 1, &mut list, &mut count) } == 0 {
        return vec![format!(
            "listing the signed-in sessions: {}",
            io::Error::last_os_error()
        )];
    }
    // SAFETY: Windows returned `count` entries at `list`, freed below.
    let ids: Vec<u32> = unsafe { std::slice::from_raw_parts(list, count as usize) }
        .iter()
        .filter(|info| [WTSActive, WTSConnected, WTSDisconnected].contains(&info.State))
        .map(|info| info.SessionId)
        .collect();
    // SAFETY: the list WTSEnumerateSessionsW allocated, freed once and not used after.
    unsafe { WTSFreeMemory(list.cast()) };
    let mut problems = Vec::new();
    for id in ids {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: an out pointer that outlives the call. A session with no one signed in to
        // it has no token, and nothing is started there.
        if unsafe { WTSQueryUserToken(id, &mut token) } == 0 {
            continue;
        }
        let token = OwnedHandle(token);
        if let Err(error) = run_in_session(&token, program) {
            problems.push(format!(
                "starting {} in session {id}: {error}",
                program.display()
            ));
        }
    }
    problems
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Runs `program` in the session `token` belongs to, as its user, with the user's environment,
/// and lets it go.
fn run_in_session(token: &OwnedHandle, program: &Path) -> io::Result<()> {
    let application = wide(&program.to_string_lossy());
    let mut command_line = wide(&format!("\"{}\"", program.display()));
    let folder = program.parent().map(|dir| wide(&dir.to_string_lossy()));
    let mut desktop = wide("winsta0\\default");
    let mut environment = std::ptr::null_mut();
    // SAFETY: an out pointer and a valid token. The block is destroyed below.
    if unsafe { CreateEnvironmentBlock(&mut environment, token.0, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: plain structures, zero is their documented initial state.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.lpDesktop = desktop.as_mut_ptr();
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated strings that outlive the call, a writable command line, and an
    // environment block of the kind the flag says. No handle is inherited.
    let created = unsafe {
        CreateProcessAsUserW(
            token.0,
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT,
            environment,
            folder
                .as_ref()
                .map_or(std::ptr::null(), |folder| folder.as_ptr()),
            &startup,
            &mut process,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: the block CreateEnvironmentBlock made, destroyed once.
    unsafe { DestroyEnvironmentBlock(environment) };
    if created == 0 {
        return Err(error);
    }
    // SAFETY: handles CreateProcessAsUserW returned, closed once.
    unsafe {
        CloseHandle(process.hProcess);
        CloseHandle(process.hThread);
    }
    Ok(())
}

/// The window manager of the user with SID `sid`.
pub struct UserWindowManager {
    sid: String,
}

impl UserWindowManager {
    pub fn new(sid: &str) -> UserWindowManager {
        UserWindowManager {
            sid: sid.to_string(),
        }
    }

    /// Whether this process runs as the user, as `dsa-hostd serve` by hand does.
    fn is_own_user(&self) -> bool {
        current_user_sid().is_ok_and(|own| own == self.sid)
    }

    fn program(&self) -> Result<PathBuf, String> {
        let service = std::env::current_exe().ok();
        window_manager_path(service.as_deref())
            .ok_or_else(|| "dsa-wm.exe is not installed; repair the installation".to_string())
    }
}

impl WindowManagerHost for UserWindowManager {
    fn running(&mut self) -> bool {
        match sessions_of(&self.sid) {
            Ok(sessions) => sessions
                .iter()
                .any(|session| pipe_exists(&pipe_name(session.id))),
            Err(_) => self.is_own_user() && pipe_exists(&pipe_name(current_session())),
        }
    }

    fn start(&mut self) -> Result<(), String> {
        let program = self.program()?;
        let sessions = match sessions_of(&self.sid) {
            Ok(sessions) => sessions,
            // Not SYSTEM: it can start a program only as itself.
            Err(_) if self.is_own_user() => return run_program(&program, ""),
            Err(error) => {
                return Err(format!(
                    "this service may not start programs in another user's session ({error}); \
                     run it as the installed service"
                ))
            }
        };
        let Some(session) = sessions
            .iter()
            .find(|session| session.active)
            .or(sessions.first())
        else {
            return Err(
                "you have no signed-in Windows session on this computer. Android waits until \
                 you sign in, or stop it with `dsa stop`"
                    .to_string(),
            );
        };
        run_in_session(&session.token, &program)
            .map_err(|error| format!("could not run {}: {error}", program.display()))
    }
}

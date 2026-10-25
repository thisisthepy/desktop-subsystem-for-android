//! The local API over a Windows named pipe.
//!
//! The service creates the pipe with a security descriptor that decides who may open it,
//! and learns each client's user by impersonating the client for a moment and reading the
//! thread token's user SID. It never trusts a user named in a message.
//!
//! I/O is overlapped so that accepting and reading can time out, which the shared framing
//! needs; a timed-out read is cancelled and leaves the connection usable.

use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use dsa_transport::local::{LocalListener, Peer};
use dsa_transport::stream::{FramedConnection, StreamSocket};
use dsa_transport::{Connection, Connector, TransportError};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, LocalFree, DUPLICATE_SAME_ACCESS, ERROR_BROKEN_PIPE,
    ERROR_CANNOT_IMPERSONATE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, RevertToSelf, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    ImpersonateNamedPipeClient, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken,
    OpenThreadToken, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Only SYSTEM and interactively signed-in users may open the pipe; the service then
/// checks which user each client is.
pub const DEFAULT_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)";

const BUFFER_SIZE: u32 = 64 * 1024;

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// `\\.\pipe\<name>`.
pub fn pipe_path(name: &str) -> String {
    format!(r"\\.\pipe\{name}")
}

pub(crate) struct OwnedHandle(pub(crate) HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: an owned handle, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for OwnedHandle {}

pub(crate) fn new_event() -> io::Result<OwnedHandle> {
    // SAFETY: an unnamed, manual-reset, initially unset event.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(last_error());
    }
    Ok(OwnedHandle(event))
}

pub(crate) fn millis(timeout: Option<Duration>) -> u32 {
    match timeout {
        None => u32::MAX, // INFINITE
        Some(timeout) => timeout.as_millis().clamp(1, u32::MAX as u128 - 1) as u32,
    }
}

/// Whether `error` means the other end of the pipe is gone.
fn is_closed_pipe(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|code| code as u32),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA)
    )
}

/// One end of a connected pipe.
pub(crate) struct PipeStream {
    /// `None` once shut down: closing the handle is what lets the peer read end of stream
    /// after the data already in the pipe.
    handle: Option<OwnedHandle>,
    event: OwnedHandle,
    read_timeout: Option<Duration>,
}

impl PipeStream {
    pub(crate) fn new(handle: OwnedHandle) -> io::Result<PipeStream> {
        Ok(PipeStream {
            handle: Some(handle),
            event: new_event()?,
            read_timeout: None,
        })
    }

    /// Runs one overlapped read or write and waits for it, at most `timeout`.
    fn overlapped(
        &mut self,
        timeout: Option<Duration>,
        start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        let handle = match &self.handle {
            Some(handle) => handle.0,
            None => return Err(io::Error::from(io::ErrorKind::NotConnected)),
        };
        // SAFETY: OVERLAPPED is plain data; zero is its documented initial state.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = self.event.0;
        let mut transferred = 0u32;
        if start(handle, &mut overlapped) == 0 {
            let error = last_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            // SAFETY: a valid event handle.
            let waited = unsafe { WaitForSingleObject(self.event.0, millis(timeout)) };
            if waited == WAIT_TIMEOUT {
                // SAFETY: cancels exactly this operation, then waits for it to finish so the
                // OVERLAPPED and the buffer are no longer in use.
                let finished = unsafe {
                    CancelIoEx(handle, &overlapped);
                    GetOverlappedResult(handle, &overlapped, &mut transferred, 1)
                };
                // The operation may have completed just before the cancel; its bytes count.
                if finished != 0 && transferred > 0 {
                    return Ok(transferred as usize);
                }
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            if waited != WAIT_OBJECT_0 {
                return Err(last_error());
            }
        }
        // SAFETY: the operation has completed.
        if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) } == 0 {
            return Err(last_error());
        }
        Ok(transferred as usize)
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ptr = buf.as_mut_ptr();
        let timeout = self.read_timeout;
        match self.overlapped(timeout, |handle, overlapped| {
            // SAFETY: the buffer outlives the operation, which `overlapped` waits for.
            unsafe { ReadFile(handle, ptr, len, std::ptr::null_mut(), overlapped) }
        }) {
            // A closed pipe reads as end of stream, like a socket.
            Err(error) if is_closed_pipe(&error) => Ok(0),
            other => other,
        }
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ptr = buf.as_ptr();
        self.overlapped(None, |handle, overlapped| {
            // SAFETY: the buffer outlives the operation, which `overlapped` waits for.
            unsafe { WriteFile(handle, ptr, len, std::ptr::null_mut(), overlapped) }
        })
        .map_err(|error| {
            // Writing to a pipe the peer has closed reports a closed connection.
            if is_closed_pipe(&error) {
                io::Error::from(io::ErrorKind::BrokenPipe)
            } else {
                error
            }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StreamSocket for PipeStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }

    fn shutdown(&mut self) {
        // Closing, rather than DisconnectNamedPipe, keeps what is already in the pipe
        // readable by the peer, which then reads end of stream. Nothing is pending here:
        // every operation is waited for before it returns.
        self.handle = None;
    }

    /// A second handle on the same pipe, with its own event, so one thread can write while
    /// another waits in a read. The peer reads end of stream once every handle is closed.
    fn try_clone(&self) -> io::Result<PipeStream> {
        let handle = match &self.handle {
            Some(handle) => handle.0,
            None => return Err(io::Error::from(io::ErrorKind::NotConnected)),
        };
        let mut copy: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a handle of this process into this process.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                GetCurrentProcess(),
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(last_error());
        }
        PipeStream::new(OwnedHandle(copy))
    }
}

/// Reads the user SID of the client on `pipe` by impersonating it for a moment.
///
/// Windows may refuse to impersonate before anything has been read from the pipe; then the
/// user is read from the client process's token instead, which the service, running as
/// SYSTEM, may always open.
fn client_sid(pipe: HANDLE) -> io::Result<String> {
    // SAFETY: a connected pipe; impersonation is undone below on every path.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        let error = last_error();
        if error.raw_os_error() == Some(ERROR_CANNOT_IMPERSONATE as i32) {
            return client_process_sid(pipe);
        }
        return Err(error);
    }
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the current thread's impersonation token, checked against our own access.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let open_error = last_error();
    // SAFETY: ends the impersonation started above.
    unsafe { RevertToSelf() };
    if opened == 0 {
        return Err(open_error);
    }
    token_user_sid(&OwnedHandle(token))
}

/// The user SID of the process at the client end of `pipe`.
fn client_process_sid(pipe: HANDLE) -> io::Result<String> {
    let mut pid = 0u32;
    // SAFETY: a connected pipe.
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
        return Err(last_error());
    }
    process_sid(pid)
}

/// The user SID of the process `pid`.
fn process_sid(pid: u32) -> io::Result<String> {
    // SAFETY: opened for query only, closed by OwnedHandle.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(last_error());
    }
    let process = OwnedHandle(process);
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: a process handle with query access.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    token_user_sid(&OwnedHandle(token))
}

/// The user SID of this process, in the same form [`NamedPipeListener`] reports clients.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: our own process token, opened for query only.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    token_user_sid(&OwnedHandle(token))
}

/// The user SID in `token`, as an `S-1-...` string.
fn token_user_sid(token: &OwnedHandle) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: a size query.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: the buffer holds at least `needed` bytes, aligned for TOKEN_USER.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(last_error());
    }
    // SAFETY: the buffer holds a TOKEN_USER.
    let sid = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: a valid SID; the string is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(last_error());
    }
    // SAFETY: a NUL-terminated string from LocalAlloc, freed once.
    let sid = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text as HLOCAL);
        value
    };
    Ok(sid)
}

/// The server end: creates one pipe instance at a time and hands each client over.
pub struct NamedPipeListener {
    path: Vec<u16>,
    descriptor: PSECURITY_DESCRIPTOR,
    first: bool,
    pending: Option<OwnedHandle>,
}

// SAFETY: the descriptor is only read, and the handles are kernel handles.
unsafe impl Send for NamedPipeListener {}

impl NamedPipeListener {
    /// Serves `\\.\pipe\<name>` to the callers `sddl` allows. The first instance is created
    /// with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so another process cannot squat on the name, and
    /// remote clients are refused.
    pub fn bind(name: &str, sddl: &str) -> Result<NamedPipeListener, TransportError> {
        let sddl_w = wide(sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string; the descriptor is freed on drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_w.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(TransportError::Io {
                kind: io::ErrorKind::InvalidInput,
                message: format!(
                    "{sddl:?} is not a valid security descriptor: {}",
                    last_error()
                ),
            });
        }
        let mut listener = NamedPipeListener {
            path: wide(&pipe_path(name)),
            descriptor,
            first: true,
            pending: None,
        };
        // Create the first instance now, so the name is ours from the start.
        listener.pending = Some(listener.create_instance()?);
        Ok(listener)
    }

    fn create_instance(&mut self) -> io::Result<OwnedHandle> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if self.first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: a NUL-terminated name and valid security attributes.
        let handle = unsafe {
            CreateNamedPipeW(
                self.path.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER_SIZE,
                BUFFER_SIZE,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        self.first = false;
        Ok(OwnedHandle(handle))
    }
}

impl Drop for NamedPipeListener {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.descriptor as HLOCAL) };
    }
}

impl LocalListener for NamedPipeListener {
    fn accept(&mut self, timeout: Duration) -> Result<(Box<dyn Connection>, Peer), TransportError> {
        let instance = match self.pending.take() {
            Some(instance) => instance,
            None => self.create_instance()?,
        };
        let event = new_event()?;
        // SAFETY: OVERLAPPED is plain data.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.0;
        // SAFETY: a fresh instance and an OVERLAPPED that outlives the wait below.
        let connected = unsafe { ConnectNamedPipe(instance.0, &mut overlapped) } != 0;
        if !connected {
            let error = last_error();
            match error.raw_os_error() {
                // A client connected between creation and this call.
                Some(code) if code == ERROR_PIPE_CONNECTED as i32 => {}
                Some(code) if code == ERROR_IO_PENDING as i32 => {
                    // SAFETY: a valid event.
                    let waited = unsafe { WaitForSingleObject(event.0, millis(Some(timeout))) };
                    if waited == WAIT_TIMEOUT {
                        let mut ignored = 0u32;
                        // SAFETY: cancel and wait for this operation before reusing anything.
                        unsafe {
                            CancelIoEx(instance.0, &overlapped);
                            GetOverlappedResult(instance.0, &overlapped, &mut ignored, 1);
                        }
                        // The instance stays pending for the next call.
                        self.pending = Some(instance);
                        return Err(TransportError::Timeout);
                    }
                    let mut ignored = 0u32;
                    // SAFETY: the operation has completed.
                    if unsafe { GetOverlappedResult(instance.0, &overlapped, &mut ignored, 0) } == 0
                    {
                        return Err(last_error().into());
                    }
                }
                _ => return Err(error.into()),
            }
        }
        // Keep an instance waiting at all times, so a client that connects before the next
        // accept finds the pipe instead of "not found". Failing here only delays that.
        if let Ok(next) = self.create_instance() {
            self.pending = Some(next);
        }
        let sid = client_sid(instance.0)?;
        let stream = PipeStream::new(instance)?;
        Ok((Box::new(FramedConnection::new(stream)), Peer { user: sid }))
    }
}

/// The client end.
pub struct NamedPipeConnector {
    pub name: String,
}

impl Connector for NamedPipeConnector {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let handle = open_client(&self.name, timeout)?;
        Ok(Box::new(FramedConnection::new(PipeStream::new(handle)?)))
    }
}

/// The client end of a pipe that only a process of the same user may serve: before anything
/// is sent, it checks who runs the process at the other end, so another user who took the
/// pipe's name first is never talked to.
pub struct SameUserPipeConnector {
    pub name: String,
}

impl Connector for SameUserPipeConnector {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let handle = open_client(&self.name, timeout)?;
        let server = server_process_sid(handle.0)?;
        let own = current_user_sid()?;
        if server != own {
            return Err(TransportError::Io {
                kind: io::ErrorKind::PermissionDenied,
                message: format!(
                    "{} is served by another user ({server}), not this one",
                    pipe_path(&self.name)
                ),
            });
        }
        Ok(Box::new(FramedConnection::new(PipeStream::new(handle)?)))
    }
}

/// The user SID of the process at the server end of `pipe`.
fn server_process_sid(pipe: HANDLE) -> io::Result<String> {
    let mut pid = 0u32;
    // SAFETY: a connected pipe.
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
        return Err(last_error());
    }
    process_sid(pid)
}

/// The security descriptor of a pipe only the current user may open.
pub fn current_user_only_sddl() -> io::Result<String> {
    Ok(format!("D:P(A;;GA;;;{})", current_user_sid()?))
}

/// Opens the client end of `\\.\pipe\<name>`, waiting at most `timeout` for a free instance.
fn open_client(name: &str, timeout: Duration) -> Result<OwnedHandle, TransportError> {
    let path = wide(&pipe_path(name));
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: a NUL-terminated name. Identification level lets the server read our
        // user without being able to act as us.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut::<c_void>(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(OwnedHandle(handle));
        }
        let error = last_error();
        if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
            return Err(error.into());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(TransportError::Timeout);
        }
        // SAFETY: a NUL-terminated name.
        unsafe { WaitNamedPipeW(path.as_ptr(), millis(Some(left))) };
    }
}

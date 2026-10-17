//! Serving the named pipe the Android VM's serial console is connected to, and keeping
//! what arrives on it in the console log.
//!
//! HCS opens the pipe named in the VM's `ComPorts` as a client when the VM starts, so the
//! host creates the server end first, as WSL does for its debug console. One thread per boot
//! waits for that connection and appends everything read to the [`ConsoleLog`]. The thread
//! ends when the VM closes the pipe (it has exited) or when the capture is finished; every
//! wait in it is short, so it never holds up a stop, and nothing it meets ends the service.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dsa_transport::stream::StreamSocket;
use windows_sys::Win32::Foundation::{
    LocalFree, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HLOCAL, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::console_log::ConsoleLog;
use crate::pipe::{millis, new_event, wide, OwnedHandle, PipeStream};

/// Who may open the console pipe: SYSTEM and administrators, and the VM worker processes
/// (every VM's own account is in `NT VIRTUAL MACHINE\Virtual Machines`, S-1-5-83-0), which
/// HCS connects the serial port from. Nobody else can read the guest's console or write to
/// it.
pub const CONSOLE_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;S-1-5-83-0)";

/// How often the thread checks whether it should end.
const POLL: Duration = Duration::from_millis(200);
const BUFFER_SIZE: u32 = 64 * 1024;

/// Set by the thread when it ends, however it ends.
#[derive(Default)]
struct Done {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Done {
    fn wait(&self, timeout: Duration) -> bool {
        let done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        let (done, _) = self
            .changed
            .wait_timeout_while(done, timeout, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        *done
    }
}

/// Marks the thread done when dropped, so a panic in it still counts as ending.
struct DoneOnDrop(Arc<Done>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        *self.0.done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0.changed.notify_all();
    }
}

/// One boot's console capture: the pipe's server end and the thread reading it.
pub struct ConsoleCapture {
    log: Arc<Mutex<ConsoleLog>>,
    stop: Arc<AtomicBool>,
    done: Arc<Done>,
    thread: Option<JoinHandle<()>>,
}

fn create_server(pipe: &str, sddl: &str) -> io::Result<OwnedHandle> {
    let sddl_w = wide(sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated SDDL string; the descriptor is freed below.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_w.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{sddl:?} is not a valid security descriptor: {error}"),
        ));
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = wide(pipe);
    // SAFETY: a NUL-terminated name and valid security attributes. One instance only, and
    // the first: a name someone else already holds is an error, not a shared pipe.
    let handle = unsafe {
        CreateNamedPipeW(
            path.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            &attributes,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW; the pipe
    // keeps its own copy.
    unsafe { LocalFree(descriptor as HLOCAL) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            error.kind(),
            format!("creating the console pipe {pipe}: {error}"),
        ));
    }
    Ok(OwnedHandle(handle))
}

/// Waits for the VM to open the pipe. Returns false if `stop` was set first.
fn wait_for_client(server: &OwnedHandle, stop: &AtomicBool) -> io::Result<bool> {
    let event = new_event()?;
    // SAFETY: OVERLAPPED is plain data; zero is its documented initial state.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.0;
    // SAFETY: a listening instance and an OVERLAPPED that outlives the operation, which is
    // waited for or cancelled below before returning.
    if unsafe { ConnectNamedPipe(server.0, &mut overlapped) } != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_PIPE_CONNECTED) => return Ok(true),
        Some(ERROR_IO_PENDING) => {}
        _ => return Err(error),
    }
    loop {
        // SAFETY: a valid event.
        let waited = unsafe { WaitForSingleObject(event.0, millis(Some(POLL))) };
        if waited == WAIT_OBJECT_0 {
            let mut ignored = 0u32;
            // SAFETY: the operation has completed.
            if unsafe { GetOverlappedResult(server.0, &overlapped, &mut ignored, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(true);
        }
        if waited != WAIT_TIMEOUT {
            return Err(io::Error::last_os_error());
        }
        if stop.load(Ordering::Relaxed) {
            let mut ignored = 0u32;
            // SAFETY: cancels this operation and waits for it, so the OVERLAPPED is free.
            unsafe {
                CancelIoEx(server.0, &overlapped);
                GetOverlappedResult(server.0, &overlapped, &mut ignored, 1);
            }
            return Ok(false);
        }
    }
}

fn run(server: OwnedHandle, log: Arc<Mutex<ConsoleLog>>, stop: Arc<AtomicBool>) {
    let note = |text: &str| {
        log.lock().unwrap_or_else(|e| e.into_inner()).note(text);
    };
    match wait_for_client(&server, &stop) {
        Ok(true) => {}
        Ok(false) => return note("the Android VM never opened its console"),
        Err(error) => return note(&format!("waiting for the Android VM's console: {error}")),
    }
    let mut stream = match PipeStream::new(server) {
        Ok(stream) => stream,
        Err(error) => return note(&format!("reading the Android VM's console: {error}")),
    };
    let _ = stream.set_read_timeout(Some(POLL));
    let mut buffer = vec![0u8; BUFFER_SIZE as usize];
    loop {
        match stream.read(&mut buffer) {
            // The VM closed its end: it has exited.
            Ok(0) => return,
            Ok(read) => log
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
            }
            Err(error) => return note(&format!("reading the Android VM's console: {error}")),
        }
    }
}

impl ConsoleCapture {
    /// Serves the console pipe `pipe` (`\\.\pipe\...`) with [`CONSOLE_PIPE_SDDL`] and
    /// records what the VM writes to it in `log`. The pipe exists once this returns, so the
    /// VM can be started. On failure the log is handed back, to say why in it.
    pub fn start(
        pipe: &str,
        log: ConsoleLog,
    ) -> Result<ConsoleCapture, Box<(io::Error, ConsoleLog)>> {
        ConsoleCapture::start_with_sddl(pipe, CONSOLE_PIPE_SDDL, log)
    }

    /// [`start`](Self::start) with another security descriptor, for tests that connect as
    /// an ordinary user.
    pub fn start_with_sddl(
        pipe: &str,
        sddl: &str,
        log: ConsoleLog,
    ) -> Result<ConsoleCapture, Box<(io::Error, ConsoleLog)>> {
        let server = match create_server(pipe, sddl) {
            Ok(server) => server,
            Err(error) => return Err(Box::new((error, log))),
        };
        let log = Arc::new(Mutex::new(log));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(Done::default());
        let spawned = {
            let (log, stop, done) = (Arc::clone(&log), Arc::clone(&stop), Arc::clone(&done));
            thread::Builder::new()
                .name("dsa-console".into())
                .spawn(move || {
                    let _done = DoneOnDrop(done);
                    run(server, log, stop);
                })
        };
        let thread = match spawned {
            Ok(thread) => thread,
            Err(error) => {
                // The closure, and the clones it held, are gone with the failed spawn.
                let log = Arc::try_unwrap(log)
                    .map(|log| log.into_inner().unwrap_or_else(|e| e.into_inner()))
                    .unwrap_or_else(|_| unreachable!("no thread holds the log"));
                return Err(Box::new((error, log)));
            }
        };
        Ok(ConsoleCapture {
            log,
            stop,
            done,
            thread: Some(thread),
        })
    }

    /// Ends the capture. Waits up to `drain` for the VM's end of the pipe to close, so the
    /// last output is kept, then tells the thread to stop, which it does within a fraction
    /// of a second. Returns whether the thread has ended; one that has not is left to end
    /// on its own and never blocks the caller.
    pub fn finish(&mut self, drain: Duration) -> bool {
        if !self.done.wait(drain) {
            self.stop.store(true, Ordering::Relaxed);
            if !self.done.wait(POLL * 5) {
                return false;
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        true
    }

    /// Whether the thread has ended: the VM closed the pipe, or the capture was finished.
    pub fn ended(&self) -> bool {
        self.done.wait(Duration::ZERO)
    }

    /// The last non-empty console lines of this boot.
    pub fn tail(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).tail()
    }

    pub fn log_path(&self) -> PathBuf {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .path()
            .to_path_buf()
    }

    /// What a failed start reports about this boot's console.
    pub fn failure_report(&self) -> String {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure_report()
    }
}

impl Drop for ConsoleCapture {
    fn drop(&mut self) {
        self.finish(Duration::ZERO);
    }
}

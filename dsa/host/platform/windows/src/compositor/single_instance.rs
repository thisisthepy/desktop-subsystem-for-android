//! Single-instance guard per user session using a named mutex.

use std::io;
use std::ptr::null;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// Default named mutex in the session namespace.
pub const DEFAULT_MUTEX_NAME: &str = r"Local\DSA_Compositor_SingleInstance";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An owned single-instance mutex.
///
/// Holding this guard ensures that only one compositor instance runs in the user session.
/// Dropping it releases and closes the named mutex.
#[derive(Debug)]
pub struct SingleInstanceMutex {
    handle: HANDLE,
}

impl SingleInstanceMutex {
    /// Attempts to acquire the named mutex.
    ///
    /// Returns an error if another instance already holds the mutex or if creation fails.
    pub fn acquire(name: &str) -> Result<SingleInstanceMutex, String> {
        let wide_name = wide(name);
        // SAFETY: creating a named mutex with standard Win32 API.
        let handle = unsafe { CreateMutexW(null(), 0, wide_name.as_ptr()) };
        if handle.is_null() {
            return Err(format!(
                "could not create named mutex {name:?}: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: reading last Win32 error code.
        let last_error = unsafe { GetLastError() };
        if last_error == ERROR_ALREADY_EXISTS {
            // SAFETY: closing owned handle.
            unsafe { CloseHandle(handle) };
            return Err(format!(
                "another instance is already running in this user session (mutex {name:?})"
            ));
        }
        Ok(SingleInstanceMutex { handle })
    }
}

impl Drop for SingleInstanceMutex {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: closing owned handle on drop.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

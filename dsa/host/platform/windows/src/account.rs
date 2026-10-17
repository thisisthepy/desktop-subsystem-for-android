//! The Windows account the host runs as.

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The SID of the user this process runs as, as an `S-1-...` string.
pub fn current_user_sid() -> Result<String, String> {
    let error = |what: &str| format!("{what} failed: {}", std::io::Error::last_os_error());
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle for this process and an out pointer for the token.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(error("OpenProcessToken"));
    }
    let result = (|| {
        let mut needed = 0u32;
        // SAFETY: a size query with no buffer; it fails and reports the size needed.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(error("GetTokenInformation"));
        }
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: the buffer holds at least `needed` bytes.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(error("GetTokenInformation"));
        }
        // SAFETY: the buffer now holds a TOKEN_USER whose SID points into the buffer.
        let sid = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: a valid SID and an out pointer; the string is freed below.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(error("ConvertSidToStringSidW"));
        }
        // SAFETY: a NUL-terminated wide string from LocalAlloc, freed once.
        let sid_string = unsafe {
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as HLOCAL);
            value
        };
        Ok(sid_string)
    })();
    // SAFETY: the token handle is owned here and closed once.
    unsafe { CloseHandle(token) };
    result
}

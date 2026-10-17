//! Downloads for image updates, through WinHTTP.
//!
//! WinHTTP ships with Windows, follows the system's proxy settings and validates TLS
//! certificates against the Windows certificate store, so nothing extra is installed.
//! Integrity does not rest on it: every file is checked against the signed release list.

use std::io::Write;
use std::time::Duration;

use dsa_host_core::update::store::Fetch;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_INTERNET_SCHEME_HTTP, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(raw: *mut core::ffi::c_void, what: &str) -> Result<Handle, String> {
        if raw.is_null() {
            Err(last_error(what))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a WinHTTP handle, closed once.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// Fetches `https:` URLs (and `http:`, for a mirror on the local network).
pub struct WinHttpFetch {
    /// How long one network step (resolve, connect, send, receive) may take.
    pub timeout: Duration,
}

impl Default for WinHttpFetch {
    fn default() -> Self {
        WinHttpFetch {
            timeout: Duration::from_secs(60),
        }
    }
}

impl Fetch for WinHttpFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let url_w = wide(url);
        let mut parts = URL_COMPONENTS {
            dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
            // Non-zero lengths with null pointers ask for pointers into the URL itself.
            dwHostNameLength: u32::MAX,
            dwUrlPathLength: u32::MAX,
            dwExtraInfoLength: u32::MAX,
            ..Default::default()
        };
        // SAFETY: a NUL-terminated URL that outlives `parts`, whose pointers point into it.
        if unsafe { WinHttpCrackUrl(url_w.as_ptr(), 0, 0, &mut parts) } == 0 {
            return Err(format!(
                "{url:?} is not a URL ({})",
                last_error("WinHttpCrackUrl")
            ));
        }
        let secure = match parts.nScheme {
            WINHTTP_INTERNET_SCHEME_HTTPS => true,
            WINHTTP_INTERNET_SCHEME_HTTP => false,
            _ => return Err(format!("{url:?} is neither https nor http")),
        };
        // SAFETY: the component pointers point into `url_w` with the lengths WinHTTP gave.
        let (host, path) = unsafe {
            let host = String::from_utf16_lossy(std::slice::from_raw_parts(
                parts.lpszHostName,
                parts.dwHostNameLength as usize,
            ));
            let mut path = String::from_utf16_lossy(std::slice::from_raw_parts(
                parts.lpszUrlPath,
                parts.dwUrlPathLength as usize,
            ));
            if !parts.lpszExtraInfo.is_null() {
                path.push_str(&String::from_utf16_lossy(std::slice::from_raw_parts(
                    parts.lpszExtraInfo,
                    parts.dwExtraInfoLength as usize,
                )));
            }
            (host, path)
        };

        let agent = wide(&format!("DSA/{}", crate::VERSION));
        // SAFETY: a NUL-terminated agent string; the proxy follows the system settings.
        let session = Handle::new(
            unsafe {
                WinHttpOpen(
                    agent.as_ptr(),
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            },
            "WinHttpOpen",
        )?;
        let ms = self.timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: a valid session handle.
        unsafe { WinHttpSetTimeouts(session.0, ms, ms, ms, ms) };
        let host_w = wide(&host);
        // SAFETY: a valid session and a NUL-terminated host name.
        let connection = Handle::new(
            unsafe { WinHttpConnect(session.0, host_w.as_ptr(), parts.nPort, 0) },
            "WinHttpConnect",
        )?;
        let verb = wide("GET");
        let path_w = wide(&path);
        // SAFETY: valid handles and NUL-terminated strings.
        let request = Handle::new(
            unsafe {
                WinHttpOpenRequest(
                    connection.0,
                    verb.as_ptr(),
                    path_w.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    if secure { WINHTTP_FLAG_SECURE } else { 0 },
                )
            },
            "WinHttpOpenRequest",
        )?;
        // SAFETY: a valid request with no extra headers or body.
        if unsafe { WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) }
            == 0
        {
            return Err(format!("{url}: {}", last_error("sending the request")));
        }
        // SAFETY: a sent request.
        if unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) } == 0 {
            return Err(format!("{url}: {}", last_error("receiving the response")));
        }
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: a received response and a u32 buffer of the given size.
        if unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut size,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!("{url}: {}", last_error("reading the status")));
        }
        if status != 200 {
            return Err(format!("{url}: the server answered HTTP {status}"));
        }
        let mut buffer = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let mut read = 0u32;
            // SAFETY: a buffer of the given length; `read` receives the count.
            if unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                )
            } == 0
            {
                return Err(format!("{url}: {}", last_error("reading the body")));
            }
            if read == 0 {
                return Ok(total);
            }
            out.write_all(&buffer[..read as usize])
                .map_err(|e| format!("{url}: writing the download: {e}"))?;
            total += u64::from(read);
        }
    }
}

/// Fetches `https:` and `http:` URLs through WinHTTP, and anything else as a file path:
/// a mirror on a local or network drive. Integrity rests on the signed list either way.
#[derive(Default)]
pub struct UrlFetch(pub WinHttpFetch);

impl Fetch for UrlFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            self.0.fetch(url, out)
        } else {
            dsa_host_core::update::store::FileFetch.fetch(url, out)
        }
    }
}

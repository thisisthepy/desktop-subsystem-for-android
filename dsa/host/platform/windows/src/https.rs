//! Downloads through WinHTTP: the files of image updates, and the documents a passkey check
//! reads from a website.
//!
//! WinHTTP ships with Windows, follows the system's proxy settings and validates TLS
//! certificates against the Windows certificate store, so nothing extra is installed. No
//! certificate check is ever turned off here.
//!
//! An image update's integrity does not rest on WinHTTP: every file is checked against the
//! signed release list, so a download may follow redirects to a mirror. A passkey check is
//! different: what a site's `/.well-known/assetlinks.json` says decides which apps may use its
//! passkeys, so [`WinHttpsGet`] reads only `https:` addresses, never follows a redirect (a
//! redirect could hand the answer to another site) and stops at a size limit.

use std::io::Write;
use std::time::Duration;

use dsa_host_core::passkeys::asset_links::HttpsGet;
use dsa_host_core::update::store::Fetch;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, WinHttpSetTimeouts, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTP, WINHTTP_INTERNET_SCHEME_HTTPS,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
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

/// How one request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// How long one network step (resolve, connect, send, receive) may take.
    pub timeout: Duration,
    /// Whether only `https:` addresses are read.
    pub https_only: bool,
    /// Whether redirects are followed. When not, an answer that redirects is an error.
    pub follow_redirects: bool,
    /// The most bytes the body may have; a longer one is an error.
    pub limit: Option<u64>,
}

/// What is wrong with an answer of HTTP `status`, if anything: only 200 is a body, and a
/// redirect is named as one when redirects are not followed.
pub fn status_problem(url: &str, status: u32, follow_redirects: bool) -> Option<String> {
    match status {
        200 => None,
        300..=399 if !follow_redirects => Some(format!(
            "{url}: the server answered HTTP {status}, a redirect, which is not followed: \
             only the document at that exact address counts"
        )),
        _ => Some(format!("{url}: the server answered HTTP {status}")),
    }
}

/// Reads `url` into `out` as `policy` says, and returns how many bytes were written.
pub fn get(url: &str, policy: &Policy, out: &mut dyn Write) -> Result<u64, String> {
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
        WINHTTP_INTERNET_SCHEME_HTTP if !policy.https_only => false,
        WINHTTP_INTERNET_SCHEME_HTTP => {
            return Err(format!(
                "{url:?} is not an https address, and only https is read here"
            ))
        }
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
    let ms = policy.timeout.as_millis().min(i32::MAX as u128) as i32;
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
    if !policy.follow_redirects {
        let never: u32 = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        // SAFETY: a valid request and a u32 option value of the size given.
        if unsafe {
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&never as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        } == 0
        {
            return Err(format!(
                "{url}: {}",
                last_error("turning redirects off for the request")
            ));
        }
    }
    // SAFETY: a valid request with no extra headers or body.
    if unsafe { WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) } == 0
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
    if let Some(problem) = status_problem(url, status, policy.follow_redirects) {
        return Err(problem);
    }
    let chunk = match policy.limit {
        Some(limit) => (limit + 1).clamp(1, 1 << 20) as usize,
        None => 1 << 20,
    };
    let mut buffer = vec![0u8; chunk];
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
        total += u64::from(read);
        if let Some(limit) = policy.limit {
            if total > limit {
                return Err(format!(
                    "{url}: the document is larger than {limit} bytes, the most that is read"
                ));
            }
        }
        out.write_all(&buffer[..read as usize])
            .map_err(|e| format!("{url}: writing the download: {e}"))?;
    }
}

/// Fetches `https:` URLs (and `http:`, for a mirror on the local network), following
/// redirects.
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
        let policy = Policy {
            timeout: self.timeout,
            https_only: false,
            follow_redirects: true,
            limit: None,
        };
        get(url, &policy, out)
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

/// The documents a passkey check reads: `https:` only, the server's certificate checked
/// against the Windows certificate store, no redirect followed, at most the limit asked for.
pub struct WinHttpsGet {
    /// How long one network step may take. A site that does not answer refuses the request
    /// rather than holding the app's sign-in.
    pub timeout: Duration,
}

impl Default for WinHttpsGet {
    fn default() -> Self {
        WinHttpsGet {
            timeout: Duration::from_secs(10),
        }
    }
}

impl WinHttpsGet {
    /// The policy a read of at most `limit` bytes goes by.
    pub fn policy(&self, limit: usize) -> Policy {
        Policy {
            timeout: self.timeout,
            https_only: true,
            follow_redirects: false,
            limit: Some(limit as u64),
        }
    }
}

impl HttpsGet for WinHttpsGet {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        get(url, &self.policy(limit), &mut body)?;
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_a_passkey_check_reads_https_only_and_follows_no_redirect() {
        let policy = WinHttpsGet::default().policy(1024);
        assert!(policy.https_only);
        assert!(!policy.follow_redirects);
        assert_eq!(policy.limit, Some(1024));
        let mut body = Vec::new();
        let error = get(
            "http://example.com/.well-known/assetlinks.json",
            &policy,
            &mut body,
        )
        .unwrap_err();
        assert!(error.contains("not an https address"), "{error}");
    }

    #[test]
    fn fr36_a_redirect_is_an_error_when_redirects_are_not_followed() {
        let url = "https://example.com/.well-known/assetlinks.json";
        assert_eq!(status_problem(url, 200, false), None);
        for status in [301, 302, 303, 307, 308] {
            let problem = status_problem(url, status, false).unwrap();
            assert!(problem.contains("redirect"), "{problem}");
        }
        assert!(status_problem(url, 404, false).unwrap().contains("404"));
        // An update download follows redirects, and WinHTTP never shows it a 3xx itself.
        assert!(!status_problem(url, 302, true).unwrap().contains("redirect"));
    }
}

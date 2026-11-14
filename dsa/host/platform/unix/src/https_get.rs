//! The HTTPS fetch the passkey check reads a website's Digital Asset Links with, through the
//! system's `curl`, which checks the server's certificate against the system's store.
//!
//! What the check needs, and how curl is told:
//! - HTTPS only (`--proto =https`), with the certificate checked (curl's default, and a
//!   `~/.curlrc` that turns it off is never read: `-q` comes first).
//! - No redirect followed: no `--location`, and a `3xx` answer is an error. A redirect could
//!   hand the check to another site.
//! - At most the size the caller allows (`--max-filesize`, and the output is cut at one byte
//!   more, so a server that does not say its size is caught too).
//! - Bounded in time (`--connect-timeout`, `--max-time`), so a slow site cannot hold an app's
//!   sign-in for long.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use dsa_host_core::passkeys::asset_links::HttpsGet;

/// How long curl may take to connect, and to finish, in seconds.
pub const CONNECT_SECONDS: u32 = 10;
pub const TOTAL_SECONDS: u32 = 20;

/// What curl writes after the body: the status code, three digits (`000` without one).
const STATUS_LEN: usize = 3;

/// curl's arguments for fetching `url`, at most `limit` bytes.
pub fn curl_args(url: &str, limit: usize) -> Vec<String> {
    let mut args: Vec<String> = [
        // Ignore ~/.curlrc, which could follow redirects or skip the certificate check.
        "-q",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--connect-timeout",
    ]
    .map(str::to_string)
    .to_vec();
    args.push(CONNECT_SECONDS.to_string());
    args.push("--max-time".into());
    args.push(TOTAL_SECONDS.to_string());
    args.push("--max-filesize".into());
    args.push(limit.to_string());
    args.push("--write-out".into());
    args.push("%{http_code}".into());
    args.push("--".into());
    args.push(url.to_string());
    args
}

/// The body in what curl wrote (the body, then the status code), or why it is refused.
pub fn read_answer(url: &str, output: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if output.len() < STATUS_LEN {
        return Err(format!("{url} gave no answer"));
    }
    let (body, status) = output.split_at(output.len() - STATUS_LEN);
    let status: u16 = std::str::from_utf8(status)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("{url} gave no status"))?;
    match status {
        200 => {}
        300..=399 => {
            return Err(format!(
                "{url} redirects elsewhere (HTTP {status}), which the check does not follow"
            ))
        }
        0 => return Err(format!("{url} could not be reached")),
        _ => return Err(format!("{url} answered HTTP {status}")),
    }
    if body.len() > limit {
        return Err(format!("{url} is larger than {limit} bytes"));
    }
    Ok(body.to_vec())
}

/// Something done to curl's command before it runs, such as running it as a user.
pub type Prepare = Box<dyn Fn(&mut Command) + Send + Sync>;

/// [`HttpsGet`] through curl.
pub struct CurlHttpsGet {
    curl: PathBuf,
    prepare: Option<Prepare>,
}

impl CurlHttpsGet {
    pub fn new(curl: impl Into<PathBuf>) -> CurlHttpsGet {
        CurlHttpsGet {
            curl: curl.into(),
            prepare: None,
        }
    }

    /// Runs `prepare` on curl's command first.
    pub fn with_prepare(mut self, prepare: Prepare) -> CurlHttpsGet {
        self.prepare = Some(prepare);
        self
    }
}

impl HttpsGet for CurlHttpsGet {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        if !url.starts_with("https://") {
            return Err(format!("{url} is not an HTTPS address"));
        }
        let mut command = Command::new(&self.curl);
        if let Some(prepare) = &self.prepare {
            prepare(&mut command);
        }
        let mut child = command
            .args(curl_args(url, limit))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl cannot run, so {url} cannot be read: {e}"))?;
        let mut output = Vec::new();
        let read = child
            .stdout
            .take()
            .ok_or_else(|| "curl gave no output".to_string())
            .and_then(|stdout| {
                // The body, one byte more to tell a body that is too long, and the status.
                stdout
                    .take((limit + 1 + STATUS_LEN) as u64)
                    .read_to_end(&mut output)
                    .map_err(|e| format!("reading {url}: {e}"))
            });
        if read.is_err() || output.len() > limit + STATUS_LEN {
            let _ = child.kill();
        }
        let mut error = String::new();
        if let Some(stderr) = child.stderr.take() {
            let _ = stderr.take(4096).read_to_string(&mut error);
        }
        let status = child.wait().map_err(|e| format!("waiting for curl: {e}"))?;
        read?;
        if output.len() > limit + STATUS_LEN {
            return Err(format!("{url} is larger than {limit} bytes"));
        }
        if !status.success() {
            return Err(format!("{url} could not be read: {}", error.trim()));
        }
        read_answer(url, &output, limit)
    }
}

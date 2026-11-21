//! Downloads for image updates on the Linux and macOS hosts, through `curl`.
//!
//! Both systems have `curl`, which uses the system's certificate store and proxy settings, so
//! the host needs no TLS library of its own. What is downloaded is checked against the signed
//! release list by the update store, whatever fetched it, so `http:` is as good a transport as
//! `https:`, as on the Windows host.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use dsa_host_core::update::store::FileFetch;

/// Whether `url` is one `curl` fetches (`http:` or `https:`); anything else is a file path or a
/// `file:` URL, a mirror on a local or network drive.
pub fn is_web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// curl's arguments for `url`: HTTP and HTTPS only, redirects followed within them, failing on
/// an HTTP error rather than saving the error page, a few tries, and `url` after `--` so it
/// can never be read as an option.
pub fn download_args(url: &str) -> Vec<String> {
    [
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https,http",
        "--proto-redir",
        "=https,http",
        "--retry",
        "3",
        "--connect-timeout",
        "30",
        "--",
        url,
    ]
    .map(str::to_string)
    .to_vec()
}

/// Writes what `url` holds to `out` and returns its length: web URLs through the `curl` at
/// `curl`, anything else read directly as a file.
pub fn fetch_with_curl(curl: &Path, url: &str, out: &mut dyn Write) -> Result<u64, String> {
    fetch_with_curl_as(curl, url, None, out)
}

/// As [`fetch_with_curl`], asking with `user_agent` when one is given.
pub fn fetch_with_curl_as(
    curl: &Path,
    url: &str,
    user_agent: Option<&str>,
    out: &mut dyn Write,
) -> Result<u64, String> {
    use dsa_host_core::update::store::Fetch;
    if !is_web_url(url) {
        return FileFetch.fetch(url, out);
    }
    let mut command = Command::new(curl);
    if let Some(agent) = user_agent {
        command.arg("--user-agent").arg(agent);
    }
    let mut child = command
        .args(download_args(url))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running curl for {url}: {e}"))?;
    let mut body = child.stdout.take().ok_or("curl gave no output")?;
    let copied = std::io::copy(&mut body, out).map_err(|e| format!("downloading {url}: {e}"));
    let mut error = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut error);
    }
    let status = child.wait().map_err(|e| format!("waiting for curl: {e}"))?;
    if !status.success() {
        return Err(format!("downloading {url}: {}", error.trim()));
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr14_images_are_downloaded_over_http_and_https_only() {
        assert!(is_web_url("https://example.invalid/releases.json"));
        assert!(is_web_url("HTTP://example.invalid/releases.json"));
        assert!(!is_web_url("/mirror/releases.json"));
        assert!(!is_web_url("file:///mirror/releases.json"));
        assert!(!is_web_url("ftp://example.invalid/releases.json"));
        let args = download_args("https://example.invalid/releases.json");
        assert!(args.windows(2).any(|w| w == ["--proto", "=https,http"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--proto-redir", "=https,http"]));
        assert_eq!(
            &args[args.len() - 2..],
            ["--", "https://example.invalid/releases.json"]
        );
    }

    #[test]
    fn fr14_a_url_that_looks_like_an_option_is_still_a_url() {
        let args = download_args("--output=/etc/passwd");
        assert_eq!(&args[args.len() - 2..], ["--", "--output=/etc/passwd"]);
        assert!(!is_web_url("--output=/etc/passwd"));
    }
}

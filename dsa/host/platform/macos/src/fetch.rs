//! Downloads for image updates on macOS, through the system's `curl`.
//!
//! Every Mac has `/usr/bin/curl`, which uses the system's certificate store, so the host
//! needs no TLS library of its own. What is downloaded is checked against the signed
//! release list by the update store, whatever fetched it.

use std::io::Write;
use std::path::Path;

use dsa_host_core::update::store::Fetch;

/// Fetches `https:` and `http:` URLs with curl, and `file:` URLs and paths directly.
pub struct CurlFetch;

impl Fetch for CurlFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        dsa_host_unix::fetch::fetch_with_curl(Path::new("/usr/bin/curl"), url, out)
    }
}

//! Downloads for image updates, through curl, which every distribution has and which uses the
//! system's certificate store and proxy settings.

use std::io::Write;

use dsa_host_core::update::store::{Fetch, FileFetch};

/// Fetches `https:` and `http:` URLs with curl, and `file:` URLs and paths directly.
#[derive(Default)]
pub struct CurlFetch;

impl Fetch for CurlFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        if !dsa_host_unix::fetch::is_web_url(url) {
            return FileFetch.fetch(url, out);
        }
        let curl = crate::network::find_program("curl")
            .ok_or("curl is not installed, so DSA cannot download Android images")?;
        dsa_host_unix::fetch::fetch_with_curl(&curl, url, out)
    }
}

//! Opening the web links apps in the user's Android open, in the user's default browser.
//!
//! The follower watches the links over the local API and hands each to `/usr/bin/open`,
//! which starts the browser the user chose in System Settings. Only http and https links it
//! has checked itself are handed on: given a file path or another scheme, `open` would start
//! whatever app handles it, and an app in Android must never be able to do that. The link is
//! one argument, never shell text, and it starts with `http`, so `open` cannot take it for
//! one of its options.

use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

use dsa_transport::Connector;

use super::Log;

/// The build the follower names itself by.
const BUILD: &str = "dsa-devices links";

/// The program that opens a link in the user's default browser.
pub const OPEN: &str = "/usr/bin/open";

pub use dsa_host_core::links::LinkOpener;

/// The command that opens `url` in the user's browser.
pub fn open_command(url: &str) -> Result<Command, String> {
    dsa_host_core::links::open_command(OPEN, url)
}

/// Opens links with `/usr/bin/open`.
pub struct MacOpener;

impl LinkOpener for MacOpener {
    fn open(&mut self, url: &str) -> Result<(), String> {
        let mut child = open_command(url)?
            .spawn()
            .map_err(|e| format!("{OPEN} cannot run: {e}"))?;
        // Reaped on a thread of its own, so a browser that takes its time holds nothing up.
        thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Watches the links of the user's Android through `connector` and opens each with `opener`,
/// until `stop` is set.
pub fn follow(
    mut connector: Box<dyn Connector>,
    opener: &mut dyn LinkOpener,
    stop: Arc<AtomicBool>,
    log: Log,
) {
    dsa_host_core::links::follow(&mut *connector, BUILD, opener, &stop, &*log);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr9_web_links_go_to_open_as_one_argument() {
        let command = open_command("https://example.com/a?q=1;rm$(id)").unwrap();
        assert_eq!(command.get_program(), OPEN);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["https://example.com/a?q=1;rm$(id)"]);
    }

    #[test]
    fn fr9_links_that_are_not_web_links_never_reach_open() {
        for url in [
            "file:///etc/passwd",
            "/Applications/Utilities/Terminal.app",
            "javascript:alert(1)",
            "-a Terminal",
            "--help",
            "smb://server/share",
            "x-apple.systempreferences:com.apple.preference.security",
        ] {
            assert!(open_command(url).is_err(), "{url} was let through");
        }
    }
}

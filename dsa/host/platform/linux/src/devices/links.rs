//! Opening the web links apps in the user's Android open, in the user's default browser.
//!
//! The follower watches the links over the local API and hands each to `xdg-open`, which
//! starts the browser the user chose on their desktop. Only http and https links it has
//! checked itself are handed on: given a file path or another scheme, `xdg-open` would start
//! whatever program handles it, and an app in Android must never be able to do that. The
//! link is one argument after `--`, never shell text.

use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

use dsa_transport::Connector;

use super::Log;

/// The build the follower names itself by.
const BUILD: &str = "dsa-devices links";

pub use dsa_host_core::links::LinkOpener;

/// The command that opens `url` in the user's browser.
pub fn open_command(url: &str) -> Result<Command, String> {
    // xdg-open takes no options, so the link cannot be read as one; it starts with http.
    dsa_host_core::links::open_command("xdg-open", url)
}

/// Opens links with `xdg-open`.
pub struct XdgOpener;

impl LinkOpener for XdgOpener {
    fn open(&mut self, url: &str) -> Result<(), String> {
        let mut child = open_command(url)?
            .spawn()
            .map_err(|e| format!("xdg-open cannot run: {e}"))?;
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
    fn fr9_web_links_go_to_xdg_open_as_one_argument() {
        let command = open_command("https://example.com/a?q=1;rm$(id)").unwrap();
        assert_eq!(command.get_program(), "xdg-open");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn fr9_links_that_are_not_web_links_never_reach_xdg_open() {
        for url in [
            "file:///etc/passwd",
            "/usr/bin/xterm",
            "javascript:alert(1)",
            "--help",
            "smb://server/share",
        ] {
            assert!(open_command(url).is_err(), "{url} was let through");
        }
    }
}

//! Opening the web links apps in the user's Android open, in the user's default browser.
//!
//! The service cannot open anything in the user's session, so the window manager, which runs
//! there, watches the links over the local API and hands each to the shell with the `open`
//! verb, which starts the browser the user chose in Windows. Only http and https links it has
//! checked itself go to the shell: given a file path or another scheme, the shell would run a
//! program, and an app in Android must never be able to do that.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// The build the window manager names itself by when it watches links.
const BUILD: &str = "dsa-wm links";

/// How long the watch waits for a link before it looks at `stop` again.
const POLL: Duration = Duration::from_millis(200);

/// How long the watch waits before it reconnects to the service.
const RETRY: Duration = Duration::from_secs(2);

/// Opens a link the user's Android opened.
pub trait LinkOpener: Send {
    fn open(&mut self, url: &str) -> Result<(), String>;
}

/// Checks a link from Android as the window manager does before it opens it: an http or
/// https link, with a host and nothing a shell could take for something else. Returns the
/// link to open.
pub fn openable(url: &str) -> Result<&str, String> {
    dsa_host_core::links::web_url(url)
}

/// Opens links in the user's default browser through the shell.
pub struct ShellOpener;

impl LinkOpener for ShellOpener {
    fn open(&mut self, url: &str) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let url = openable(url)?;
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL-terminated strings that outlive the call; no window owns it.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW reports success with a value above 32.
        if result as isize > 32 {
            Ok(())
        } else {
            Err(format!(
                "the shell did not open the link (error {})",
                result as isize
            ))
        }
    }
}

/// Watches the links of the user's Android through `connector` and opens each with `opener`,
/// until `stop` is set. A lost connection is made again; a link that fails the check is
/// dropped and logged.
pub fn follow(
    mut connector: Box<dyn Connector>,
    opener: &mut dyn LinkOpener,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let log = |message: &str| note(logger.as_deref(), message);
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, BUILD, Duration::from_secs(5))
            .and_then(|client| client.watch_links());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => {
                log(&format!("links: cannot watch Android's links yet: {why}"));
                wait(&stop, RETRY);
                continue;
            }
        };
        while !stop.load(Ordering::Relaxed) {
            match stream.next(POLL) {
                Ok(None) => {}
                Ok(Some(url)) => match openable(&url) {
                    Ok(url) => {
                        if let Err(why) = opener.open(url) {
                            log(&format!("links: {why}"));
                        }
                    }
                    Err(why) => log(&format!("links: dropped a link from Android: {why}")),
                },
                Err(why) => {
                    log(&format!("links: the watch of Android's links ended: {why}"));
                    wait(&stop, RETRY);
                    break;
                }
            }
        }
    }
}

/// Sleeps `time`, or less once `stop` is set.
fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        thread::sleep(step);
        waited += step;
    }
}

//! Opening the web links apps in the user's Android open, in the user's default browser.
//!
//! The service cannot open anything in the user's session, so the window manager, which runs
//! there, watches the links over the local API and hands each to the shell with the `open`
//! verb, which starts the browser the user chose in Windows. Only http and https links it has
//! checked itself go to the shell: given a file path or another scheme, the shell would run a
//! program, and an app in Android must never be able to do that.

use dsa_host_core::links::web_url;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// The build the window manager names itself by when it watches links.
const BUILD: &str = "dsa-wm links";

pub use dsa_host_core::links::LinkOpener;

/// Opens links in the user's default browser through the shell.
pub struct ShellOpener;

impl LinkOpener for ShellOpener {
    fn open(&mut self, url: &str) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let url = web_url(url)?;
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
/// until `stop` is set.
pub fn follow(
    mut connector: Box<dyn Connector>,
    opener: &mut dyn LinkOpener,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let log = |message: &str| note(logger.as_deref(), message);
    dsa_host_core::links::follow(&mut *connector, BUILD, opener, &stop, &log);
}

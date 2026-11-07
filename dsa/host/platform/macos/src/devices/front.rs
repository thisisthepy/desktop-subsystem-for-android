//! Whether one of Android's windows is in front, which decides whether a game controller's
//! buttons and sticks reach Android.
//!
//! Each Android app's windows belong to that app's own window host, a small app of its own
//! whose bundle identifier starts with [`BUNDLE_PREFIX`] (the window host's helper bundle in
//! DSA.app, [`HELPER_BUNDLE`], hosts a window before an app has its own). The frontmost app
//! of the Mac is asked every
//! [`CHECK_EVERY`]; the answer is kept in a flag the controller follower reads as often as
//! it likes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::shell::app_stubs::BUNDLE_PREFIX;

/// DSA.app's own bundle identifier. Its program is the settings app, which shows no Android
/// window.
pub const DSA_BUNDLE: &str = "org.thisisthepy.dsa";

/// The bundle identifier of the window host's helper bundle in DSA.app.
pub const HELPER_BUNDLE: &str = "org.thisisthepy.dsa.window-host";

/// How often the frontmost app is asked.
pub const CHECK_EVERY: Duration = Duration::from_millis(250);

/// Whether the app with bundle identifier `bundle` shows Android's windows.
pub fn is_android_window_host(bundle: &str) -> bool {
    bundle == HELPER_BUNDLE || bundle.starts_with(BUNDLE_PREFIX)
}

/// Keeps `in_front` set while the Mac's frontmost app shows Android's windows, until `stop`.
#[cfg(target_os = "macos")]
pub fn watch(in_front: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    use crate::objc::{class, on_main, string_of};

    while !stop.load(Ordering::Relaxed) {
        let flag = Arc::clone(&in_front);
        // NSWorkspace is read on the main thread, where AppKit keeps it current.
        on_main(move || unsafe {
            let Ok(workspace) = class(c"NSWorkspace") else {
                return;
            };
            let shared: *mut AnyObject = msg_send![workspace, sharedWorkspace];
            if shared.is_null() {
                return;
            }
            let front: *mut AnyObject = msg_send![shared, frontmostApplication];
            let bundle = if front.is_null() {
                String::new()
            } else {
                string_of(msg_send![front, bundleIdentifier])
            };
            flag.store(is_android_window_host(&bundle), Ordering::Relaxed);
        });
        std::thread::sleep(CHECK_EVERY);
    }
}

/// Without AppKit, nothing is ever in front.
#[cfg(not(target_os = "macos"))]
pub fn watch(in_front: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    in_front.store(false, Ordering::Relaxed);
    let _ = stop;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr30_a_controller_reaches_android_only_while_its_window_is_in_front() {
        assert!(is_android_window_host(HELPER_BUNDLE));
        assert!(!is_android_window_host(DSA_BUNDLE));
        assert!(is_android_window_host(
            &crate::shell::app_stubs::bundle_identifier("com.example.game")
        ));
        assert!(!is_android_window_host("com.apple.Safari"));
        assert!(!is_android_window_host("org.thisisthepy.dsa.devices"));
        assert!(!is_android_window_host(""));
    }
}

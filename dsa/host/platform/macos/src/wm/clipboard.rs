//! The window manager's side of the shared clipboard on macOS.
//!
//! The service runs as root and cannot reach the user's pasteboard, so the window manager,
//! which runs as the user, carries it: a thread reads the pasteboard's change count on a timer
//! and hands each new text or image to the loop every host runs
//! ([`dsa_host_core::clipboard::sync`]), which passes it to the service and puts what the
//! user's Android copies on the pasteboard. The service decides whether anything crosses, from
//! the user's clipboard setting.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_unix::socket::UnixLocalConnector;

use super::log;
use crate::shell::clipboard::Pasteboard;

/// The build the window manager names itself by on the local API for the clipboard.
const BUILD: &str = concat!("dsa-wm clipboard ", env!("CARGO_PKG_VERSION"));

/// How often the pasteboard's change count is read. A copy reaches Android this long after it
/// is made at most.
const POLL: Duration = Duration::from_millis(250);

/// Carries the clipboard between the user's pasteboard and their Android until `stop`, from
/// threads of its own. A pasteboard that cannot be reached is logged; the rest of the window
/// manager goes on.
pub fn share(host_api: PathBuf, stop: Arc<AtomicBool>) {
    let (mut watched, mut board) = match (Pasteboard::general(), Pasteboard::general()) {
        (Ok(watched), Ok(board)) => (watched, board),
        (Err(why), _) | (_, Err(why)) => {
            log(&format!("the clipboard is not shared with Android: {why}"));
            return;
        }
    };
    let (changes, receiver) = channel();
    {
        let stop = Arc::clone(&stop);
        let started = thread::Builder::new()
            .name("dsa-wm-pasteboard".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(clip) = watched.changed() {
                        if changes.send(clip).is_err() {
                            return;
                        }
                    }
                    thread::sleep(POLL);
                }
            });
        if let Err(why) = started {
            log(&format!("could not start the clipboard's thread: {why}"));
            return;
        }
    }
    let started = thread::Builder::new()
        .name("dsa-wm-clipboard".into())
        .spawn(move || {
            dsa_host_core::clipboard::sync(
                Box::new(UnixLocalConnector { path: host_api }),
                BUILD,
                &mut board,
                receiver,
                stop,
                &|line| log(line),
            );
        });
    if let Err(why) = started {
        log(&format!("could not start the clipboard's thread: {why}"));
    }
}

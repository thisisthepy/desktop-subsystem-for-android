//! What the programs that follow the user's Android for a host share: the log line, the wait
//! that ends early, and the thread that reports when it cannot start.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Where a follower writes the lines it reports.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Waits `time`, or less when `stop` is set.
pub fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        thread::sleep(step);
        waited += step;
    }
}

/// Runs `run` on a thread named `name`. A thread that cannot start is logged, and `None`
/// says so.
pub fn spawn(name: &str, log: &Log, run: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    match thread::Builder::new().name(name.into()).spawn(run) {
        Ok(handle) => Some(handle),
        Err(error) => {
            log(&format!("could not start {name}: {error}"));
            None
        }
    }
}

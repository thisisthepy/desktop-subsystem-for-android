//! Keeping a user's window manager running while their Android runs.
//!
//! The window manager hosts the renderer, and Android cannot finish booting without it. The
//! service runs in no user's session, so a platform says how to look for the program in the
//! user's session and how to start it there ([`WindowManagerHost`]); when to do so is decided
//! here.

use std::time::{Duration, Instant};

/// The user's window manager, as a platform reaches it.
pub trait WindowManagerHost: Send {
    /// Whether it runs and serves. It must not take long.
    fn running(&mut self) -> bool;
    /// Starts it in the user's session. An error says why it cannot run, in words for the user.
    fn start(&mut self) -> Result<(), String>;
}

/// How long a started window manager is given to serve before it is started again. Each start
/// that does not last doubles this, up to [`MAX_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(10);
const MAX_RETRY: Duration = Duration::from_secs(60);
/// How soon a start that could not be made is tried again.
const FAILED_RETRY: Duration = Duration::from_secs(5);
/// A window manager that has served this long no longer counts as one that keeps exiting.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// How many starts in a row may fail to last before it is called a window manager that keeps
/// exiting rather than one that is on its way.
const KEEPS_EXITING_AFTER: u32 = 3;

/// Starts the window manager when it is not running, and backs off when it does not stay up.
pub struct WindowManagerKeeper<H> {
    host: H,
    /// Starts since it last served for [`STABLE_AFTER`].
    starts: u32,
    up_since: Option<Instant>,
    next_start: Option<Instant>,
    /// What to say until the next start.
    waiting: String,
}

impl<H: WindowManagerHost> WindowManagerKeeper<H> {
    pub fn new(host: H) -> WindowManagerKeeper<H> {
        WindowManagerKeeper {
            host,
            starts: 0,
            up_since: None,
            next_start: None,
            waiting: String::new(),
        }
    }

    /// Looks at the window manager at `now`, and starts it when it is time to. `None` when it
    /// serves; otherwise why Android has to wait for it.
    pub fn check(&mut self, now: Instant) -> Option<String> {
        if self.host.running() {
            let since = *self.up_since.get_or_insert(now);
            self.next_start = None;
            if now.duration_since(since) >= STABLE_AFTER {
                self.starts = 0;
            }
            return None;
        }
        self.up_since = None;
        if self.next_start.is_some_and(|next| now < next) {
            return Some(self.waiting.clone());
        }
        match self.host.start() {
            Ok(()) => {
                self.starts += 1;
                self.next_start = Some(now + retry_after(self.starts));
                self.waiting = if self.starts > KEEPS_EXITING_AFTER {
                    format!(
                        "the DSA Window Manager keeps exiting (started {} times), and Android \
                         cannot boot without its renderer. Its log, \
                         %LOCALAPPDATA%\\DSA\\logs\\dsa-wm.log, says why",
                        self.starts
                    )
                } else {
                    "the DSA Window Manager is starting; Android needs its renderer".to_string()
                };
                eprintln!("window manager: started it (start {})", self.starts);
            }
            Err(why) => {
                self.next_start = Some(now + FAILED_RETRY);
                self.waiting = format!(
                    "the DSA Window Manager is not running and cannot be started, and Android \
                     cannot boot without its renderer: {why}"
                );
            }
        }
        Some(self.waiting.clone())
    }
}

fn retry_after(starts: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(1 << starts.saturating_sub(1).min(8))
        .min(MAX_RETRY)
}

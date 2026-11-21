//! Whether Android runs, as the host's own processes see it, and what they hold for it.
//!
//! The window manager keeps a graphics renderer process, Direct3D or Metal devices and their
//! caches, and the service keeps buffers, all of which only a running Android uses. While
//! Android is stopped (not dozing: a dozing Android still runs and opens apps at once) none of
//! that is needed, and the user expects the host to hold close to nothing.
//!
//! [`Presence`] is the one rule every host follows, fed with the session state the service
//! reports:
//!
//! - The renderer runs while Android runs, from the moment it starts (`Starting`, before the
//!   guest draws its first frame) until it has been stopped for [`STOP_LINGER`]. The linger
//!   keeps a restart of Android (stop, then start) from ending and starting the renderer.
//! - Once Android has been stopped that long, what was held for it is released, once per stop
//!   ([`Presence::release_due`]).
//! - A state that cannot be read (the service restarting, a call that failed) changes nothing:
//!   a running Android keeps its renderer through a moment without an answer.
//! - Before any state has been read, nothing runs: a window manager that starts while
//!   Android is stopped starts no renderer, except one probe ([`Presence::probe`]): the first
//!   renderer starts at once, so the window manager learns which GPU and which Vulkan it has
//!   and the service whether Android would have anything to draw on, and it ends as soon as
//!   it has started when Android is stopped.
//!
//! [`SharedPresence`] is the same rule shared between the threads that feed it and those that
//! follow it, and [`follow_in_background`] feeds it from the service on a thread of its own,
//! for a host whose window manager has no status poll of its own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::host_api::HostApiClient;
use crate::session::SessionState;

/// How long Android stays stopped before the renderer ends and what was held for it is
/// released. A restart of Android stops and starts it within this time.
pub const STOP_LINGER: Duration = Duration::from_secs(10);

/// How often [`follow_in_background`] asks the service for the session state.
pub const FOLLOW_EVERY: Duration = Duration::from_millis(250);

/// Where Android is, as far as the host's processes need to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No state has been read yet.
    Unknown,
    /// Android runs: starting, active, dozing, frozen, stalled or stopping.
    Running,
    /// Android is stopped, since `since`. `after_running` when it ran before in this process's
    /// life, so the renderer lingers.
    Stopped { since: Instant, after_running: bool },
}

/// Whether Android runs, and what follows from it for the host's processes.
#[derive(Debug, Clone)]
pub struct Presence {
    phase: Phase,
    released: bool,
    probing: bool,
    linger: Duration,
}

impl Default for Presence {
    fn default() -> Self {
        Presence::new()
    }
}

/// Whether `state` is one in which Android runs (and may draw).
pub fn android_runs(state: SessionState) -> bool {
    !matches!(state, SessionState::Stopped)
}

impl Presence {
    /// Nothing known yet; the renderer lingers for [`STOP_LINGER`].
    pub fn new() -> Presence {
        Presence::with_linger(STOP_LINGER)
    }

    /// The same with another linger, for tests.
    pub fn with_linger(linger: Duration) -> Presence {
        Presence {
            phase: Phase::Unknown,
            released: false,
            probing: false,
            linger,
        }
    }

    /// Takes the session state the service reported at `now`; `None` when it could not be
    /// read, which changes nothing.
    pub fn observe(&mut self, state: Option<SessionState>, now: Instant) {
        let Some(state) = state else {
            return;
        };
        if android_runs(state) {
            self.phase = Phase::Running;
            self.released = false;
            return;
        }
        match self.phase {
            Phase::Stopped { .. } => {}
            Phase::Running => {
                self.phase = Phase::Stopped {
                    since: now,
                    after_running: true,
                }
            }
            Phase::Unknown => {
                self.phase = Phase::Stopped {
                    since: now,
                    after_running: false,
                }
            }
        }
    }

    /// Wants one renderer whatever Android's state, until [`probe_done`](Self::probe_done).
    pub fn probe(&mut self) {
        self.probing = true;
    }

    /// The probe's renderer has started, or could not.
    pub fn probe_done(&mut self) {
        self.probing = false;
    }

    /// Whether the graphics renderer should run at `now`.
    pub fn renderer_wanted(&self, now: Instant) -> bool {
        if self.probing {
            return true;
        }
        match self.phase {
            Phase::Unknown => false,
            Phase::Running => true,
            Phase::Stopped {
                since,
                after_running,
            } => after_running && now.saturating_duration_since(since) < self.linger,
        }
    }

    /// Whether Android is stopped for good at `now`: stopped, and past the linger.
    pub fn settled_stopped(&self, now: Instant) -> bool {
        matches!(self.phase, Phase::Stopped { .. }) && !self.renderer_wanted(now)
    }

    /// True once for each time Android settles stopped: the moment to release what was held
    /// for it and to trim the process.
    pub fn release_due(&mut self, now: Instant) -> bool {
        if self.released || !self.settled_stopped(now) {
            return false;
        }
        self.released = true;
        true
    }
}

/// A [`Presence`] shared between threads: one feeds it, others wait on it.
#[derive(Debug, Default)]
pub struct SharedPresence {
    presence: Mutex<Presence>,
    changed: Condvar,
}

impl SharedPresence {
    pub fn new() -> Arc<SharedPresence> {
        Arc::new(SharedPresence::default())
    }

    /// The same with another linger, for tests.
    pub fn with_linger(linger: Duration) -> Arc<SharedPresence> {
        Arc::new(SharedPresence {
            presence: Mutex::new(Presence::with_linger(linger)),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Presence> {
        self.presence.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// [`Presence::observe`], now.
    pub fn observe(&self, state: Option<SessionState>) {
        let mut presence = self.lock();
        let before = presence.renderer_wanted(Instant::now());
        presence.observe(state, Instant::now());
        if presence.renderer_wanted(Instant::now()) != before {
            self.changed.notify_all();
        }
    }

    /// [`Presence::probe`].
    pub fn probe(&self) {
        self.lock().probe();
        self.changed.notify_all();
    }

    /// [`Presence::probe_done`].
    pub fn probe_done(&self) {
        self.lock().probe_done();
        self.changed.notify_all();
    }

    /// [`Presence::renderer_wanted`], now.
    pub fn renderer_wanted(&self) -> bool {
        self.lock().renderer_wanted(Instant::now())
    }

    /// [`Presence::settled_stopped`], now.
    pub fn settled_stopped(&self) -> bool {
        self.lock().settled_stopped(Instant::now())
    }

    /// [`Presence::release_due`], now.
    pub fn release_due(&self) -> bool {
        self.lock().release_due(Instant::now())
    }

    /// Waits up to `timeout` for the renderer to be wanted; true when it is.
    pub fn wait_wanted(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut presence = self.lock();
        loop {
            if presence.renderer_wanted(Instant::now()) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            presence = self
                .changed
                .wait_timeout(presence, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// Feeds `presence` with the session state the service reports, every [`FOLLOW_EVERY`], on a
/// thread of its own until `stop` is set. `connect` opens a connection to the service; one
/// that fails, or breaks, is opened again on the next round.
pub fn follow_in_background(
    mut connect: impl FnMut() -> Result<HostApiClient, String> + Send + 'static,
    presence: Arc<SharedPresence>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("dsa-android-presence".into())
        .spawn(move || {
            let mut client: Option<HostApiClient> = None;
            while !stop.load(Ordering::Relaxed) {
                if client.is_none() {
                    client = connect().ok();
                }
                let state = match client.as_mut().map(HostApiClient::status) {
                    Some(Ok(status)) => Some(status.state),
                    Some(Err(_)) => {
                        client = None;
                        None
                    }
                    None => None,
                };
                presence.observe(state);
                thread::sleep(FOLLOW_EVERY);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINGER: Duration = Duration::from_secs(10);

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn nfr1_no_renderer_runs_while_android_is_stopped_from_the_start() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        assert!(!presence.renderer_wanted(start), "nothing known yet");
        presence.observe(Some(SessionState::Stopped), start);
        assert!(!presence.renderer_wanted(start));
        assert!(!presence.renderer_wanted(at(start, 60)));
        // What was held is released at once: Android never ran here.
        assert!(presence.release_due(start));
        assert!(!presence.release_due(at(start, 1)), "once per stop");
    }

    #[test]
    fn nfr1_the_renderer_starts_as_android_starts_before_its_first_frame() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        presence.observe(Some(SessionState::Stopped), start);
        presence.observe(Some(SessionState::Starting), at(start, 1));
        assert!(presence.renderer_wanted(at(start, 1)));
        for running in [
            SessionState::Active,
            SessionState::Dozing,
            SessionState::Frozen,
            SessionState::Stalled,
            SessionState::Stopping,
        ] {
            presence.observe(Some(running), at(start, 2));
            assert!(presence.renderer_wanted(at(start, 2)), "{running:?}");
            assert!(!presence.release_due(at(start, 2)), "{running:?}");
        }
    }

    #[test]
    fn nfr1_the_renderer_ends_once_android_has_stopped_for_the_linger() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        presence.observe(Some(SessionState::Active), start);
        presence.observe(Some(SessionState::Stopped), at(start, 5));
        assert!(presence.renderer_wanted(at(start, 14)), "still lingering");
        assert!(!presence.release_due(at(start, 14)));
        assert!(!presence.renderer_wanted(at(start, 15)));
        assert!(presence.release_due(at(start, 15)));
        assert!(!presence.release_due(at(start, 16)));
        // Stopped reported again does not move the moment it stopped.
        presence.observe(Some(SessionState::Stopped), at(start, 20));
        assert!(!presence.renderer_wanted(at(start, 20)));
    }

    #[test]
    fn nfr1_a_restart_of_android_keeps_the_renderer() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        presence.observe(Some(SessionState::Active), start);
        presence.observe(Some(SessionState::Stopped), at(start, 1));
        presence.observe(Some(SessionState::Starting), at(start, 3));
        for secs in 1..30 {
            assert!(presence.renderer_wanted(at(start, secs)), "{secs} s");
        }
        assert!(!presence.release_due(at(start, 30)));
    }

    #[test]
    fn nfr1_a_state_that_cannot_be_read_changes_nothing() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        presence.observe(None, start);
        assert!(!presence.renderer_wanted(start));
        presence.observe(Some(SessionState::Active), start);
        presence.observe(None, at(start, 60));
        assert!(presence.renderer_wanted(at(start, 60)));
    }

    #[test]
    fn nfr1_each_stop_releases_once() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        presence.observe(Some(SessionState::Active), start);
        presence.observe(Some(SessionState::Stopped), at(start, 1));
        assert!(presence.release_due(at(start, 20)));
        presence.observe(Some(SessionState::Starting), at(start, 30));
        presence.observe(Some(SessionState::Stopped), at(start, 40));
        assert!(!presence.release_due(at(start, 45)));
        assert!(presence.release_due(at(start, 50)));
    }

    #[test]
    fn nfr1_one_probe_runs_the_first_renderer_while_android_is_stopped() {
        let start = Instant::now();
        let mut presence = Presence::with_linger(LINGER);
        presence.observe(Some(SessionState::Stopped), start);
        presence.probe();
        assert!(presence.renderer_wanted(start));
        assert!(
            !presence.release_due(start),
            "nothing is released while it probes"
        );
        presence.probe_done();
        assert!(!presence.renderer_wanted(start));
        assert!(presence.release_due(start));
    }

    #[test]
    fn nfr1_a_waiting_thread_is_woken_when_android_starts() {
        let shared = SharedPresence::with_linger(LINGER);
        shared.observe(Some(SessionState::Stopped));
        assert!(!shared.wait_wanted(Duration::from_millis(10)));
        let feeder = Arc::clone(&shared);
        let started = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            feeder.observe(Some(SessionState::Starting));
        });
        assert!(shared.wait_wanted(Duration::from_secs(10)));
        started.join().unwrap();
    }
}

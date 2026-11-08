//! The host computer's power, as the guests see it.
//!
//! A guest has no battery of its own. The host reads its own power (AC or battery, the
//! charge, battery saver) through the platform and hands it to every running Android, whose
//! agent makes the guest's battery state match, so apps and Doze behave as they would on a
//! device running on that power.
//!
//! The host's sleep travels the same way: before the host sleeps every running Android is
//! told and puts its data on disk, and when it wakes each one has its clock set and returns
//! to the state it had ([`PowerHub::host_sleeping`], [`PowerHub::host_woke`]).
//!
//! The host's memory travels the same way: when the host runs short of memory, every running
//! Android is told, and gives back what it can spare, cached app processes first (see
//! [`MemoryPressure`]).

use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use dsa_protocol::control;

use crate::session::Session;

/// The host's power at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerState {
    /// Whether the host runs on AC power; false while it runs on its battery.
    pub ac_online: bool,
    /// Whether the host has a battery.
    pub battery_present: bool,
    /// The battery's charge, from 0 to 100.
    pub battery_percent: u8,
    /// Whether the battery is charging.
    pub charging: bool,
    /// Whether the host's battery saver is on.
    pub power_saver: bool,
}

impl PowerState {
    /// A host without a battery: a full battery on AC power, as the guest is told.
    pub const MAINS_ONLY: PowerState = PowerState {
        ac_online: true,
        battery_present: false,
        battery_percent: 100,
        charging: false,
        power_saver: false,
    };

    /// The control message that carries this state.
    pub fn to_control(&self) -> control::PowerState {
        control::PowerState {
            ac_online: self.ac_online,
            battery_present: self.battery_present,
            battery_percent: u32::from(self.battery_percent.min(100)),
            charging: self.charging,
            power_saver: self.power_saver,
        }
    }
}

/// Says from the host's physical memory whether the host is short of it, with a margin so a
/// host near the line does not flip back and forth: short once less than [`LOW_PERCENT`] of
/// its memory is available, and no longer short once [`CLEAR_PERCENT`] is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryPressure {
    low: bool,
}

/// Below this share of physical memory available, in percent, the host is short of memory.
pub const LOW_PERCENT: u64 = 10;
/// At or above this share of physical memory available, in percent, it no longer is.
pub const CLEAR_PERCENT: u64 = 20;

impl MemoryPressure {
    pub fn new() -> MemoryPressure {
        MemoryPressure::default()
    }

    /// Takes in the host's memory now, both in KiB, and says whether the host is short of it.
    /// A total of zero (unknown) changes nothing.
    pub fn update(&mut self, total_kb: u64, available_kb: u64) -> bool {
        if total_kb == 0 {
            return self.low;
        }
        let percent = available_kb.saturating_mul(100) / total_kb;
        if self.low {
            self.low = percent < CLEAR_PERCENT;
        } else {
            self.low = percent < LOW_PERCENT;
        }
        self.low
    }

    pub fn low(&self) -> bool {
        self.low
    }
}

/// What the host's platform says about its sleep, from its suspend and resume notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEvent {
    /// The host is about to sleep, hibernate or enter modern standby's sleep.
    Sleeping,
    /// The host woke.
    Woke,
}

/// How long the host's sleep waits for the guests to put their data on disk. A host's suspend
/// notification gives its programs only a short time, and a sleep that a person asked for
/// must not wait on a guest that does not answer.
pub const SLEEP_WAIT: Duration = Duration::from_secs(2);

/// Hands the host's power, and whether the host is short of memory, to every Android the
/// service runs: the latest state to each new one, and every change to all of them.
#[derive(Default)]
pub struct PowerHub {
    inner: Mutex<HubState>,
}

#[derive(Default)]
struct HubState {
    latest: Option<PowerState>,
    memory_low: bool,
    sessions: Vec<Weak<Session>>,
}

impl PowerHub {
    pub fn new() -> PowerHub {
        PowerHub::default()
    }

    /// Follows `session` from now on, and hands it the latest state at once. The hub keeps
    /// no session alive: one that is dropped is forgotten.
    pub fn attach(&self, session: &Arc<Session>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.sessions.push(Arc::downgrade(session));
        if let Some(state) = inner.latest {
            session.set_power(state);
        }
        if inner.memory_low {
            session.host_memory_low(true);
        }
    }

    /// Records the host's power and hands it to every session, unless it did not change.
    pub fn update(&self, state: PowerState) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.latest == Some(state) {
            return;
        }
        inner.latest = Some(state);
        inner.sessions.retain(|session| match session.upgrade() {
            Some(session) => {
                session.set_power(state);
                true
            }
            None => false,
        });
    }

    /// Records whether the host is short of memory and tells every session, unless it did not
    /// change.
    pub fn update_memory(&self, low: bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.memory_low == low {
            return;
        }
        inner.memory_low = low;
        inner.sessions.retain(|session| match session.upgrade() {
            Some(session) => {
                session.host_memory_low(low);
                true
            }
            None => false,
        });
    }

    /// Says that the host is about to sleep, to every session, and waits until each guest has
    /// put its data on disk, for at most `wait` in all. Returns whether every guest answered in
    /// time. The host's platform calls this from its suspend notification, which Windows (and
    /// the other hosts) give only a short time.
    pub fn host_sleeping(&self, wait: Duration) -> bool {
        let answers: Vec<_> = self
            .live_sessions()
            .iter()
            .map(|session| session.host_sleeping())
            .collect();
        let deadline = Instant::now() + wait;
        let mut all = true;
        for answer in answers {
            let left = deadline.saturating_duration_since(Instant::now());
            match answer.recv_timeout(left) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => all = false,
            }
        }
        all
    }

    /// Hands one of the host's sleep notifications to every session, as every host does: before
    /// the host sleeps each guest puts its data on disk, waited for at most [`SLEEP_WAIT`] in
    /// all; when it wakes each guest has its clock set to the host's. Returns whether the
    /// sleep's wait ended with every guest's answer (always true on waking).
    pub fn on_sleep_event(&self, event: SleepEvent) -> bool {
        match event {
            SleepEvent::Sleeping => {
                eprintln!("power: the computer is going to sleep");
                let answered = self.host_sleeping(SLEEP_WAIT);
                if !answered {
                    eprintln!(
                        "power: an Android did not say its data was written within {} s; the \
                         computer sleeps anyway",
                        SLEEP_WAIT.as_secs()
                    );
                }
                answered
            }
            SleepEvent::Woke => {
                eprintln!("power: the computer woke");
                self.host_woke(SystemTime::now());
                true
            }
        }
    }

    /// Says that the host woke, its wall clock reading `wall_clock`, to every session.
    pub fn host_woke(&self, wall_clock: SystemTime) {
        for session in self.live_sessions() {
            session.host_woke(wall_clock);
        }
    }

    /// The sessions still alive, forgetting those that are not.
    fn live_sessions(&self) -> Vec<Arc<Session>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut live = Vec::new();
        inner.sessions.retain(|session| match session.upgrade() {
            Some(session) => {
                live.push(session);
                true
            }
            None => false,
        });
        live
    }

    /// Whether the host was last said to be short of memory.
    pub fn memory_low(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .memory_low
    }

    /// The latest state recorded.
    pub fn latest(&self) -> Option<PowerState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).latest
    }
}

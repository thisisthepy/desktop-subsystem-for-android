//! The host computer's power, as the guests see it.
//!
//! A guest has no battery of its own. The host reads its own power (AC or battery, the
//! charge, battery saver) through the platform and hands it to every running Android, whose
//! agent makes the guest's battery state match, so apps and Doze behave as they would on a
//! device running on that power.

use std::sync::{Arc, Mutex, Weak};

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

/// Hands the host's power to every Android the service runs: the latest state to each new
/// one, and every change to all of them.
#[derive(Default)]
pub struct PowerHub {
    inner: Mutex<HubState>,
}

#[derive(Default)]
struct HubState {
    latest: Option<PowerState>,
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

    /// The latest state recorded.
    pub fn latest(&self) -> Option<PowerState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).latest
    }
}

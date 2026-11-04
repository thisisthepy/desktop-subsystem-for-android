//! The guest's power hints, and how hard the host runs the guest's processors for them.
//!
//! The guest's power HAL runs on the host: Android's power manager tells it which power modes
//! are on (launching an app, a game, low power, the display off) and asks it for short boosts
//! when the user touches the screen. The HAL hands each one to the host as a `PowerHint`.
//! [`PowerHints`] keeps them and says which [`Performance`] the guest wants now; the platform
//! applies it to the processes that run the guest's processors. Nothing here names an
//! operating system.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use dsa_protocol::control::{self, PowerHintKind};

/// android.hardware.power's Mode values the host acts on.
pub mod mode {
    pub const LOW_POWER: i32 = 1;
    pub const SUSTAINED_PERFORMANCE: i32 = 2;
    pub const FIXED_PERFORMANCE: i32 = 3;
    pub const VR: i32 = 4;
    pub const LAUNCH: i32 = 5;
    pub const EXPENSIVE_RENDERING: i32 = 6;
    pub const INTERACTIVE: i32 = 7;
    pub const DEVICE_IDLE: i32 = 8;
    pub const DISPLAY_INACTIVE: i32 = 9;
    pub const AUDIO_STREAMING_LOW_LATENCY: i32 = 10;
    pub const GAME: i32 = 15;
    pub const GAME_LOADING: i32 = 16;
}

/// android.hardware.power's Boost values the host acts on.
pub mod boost {
    pub const INTERACTION: i32 = 0;
    pub const DISPLAY_UPDATE_IMMINENT: i32 = 1;
    pub const AUDIO_LAUNCH: i32 = 3;
    pub const CAMERA_LAUNCH: i32 = 4;
    pub const CAMERA_SHOT: i32 = 5;
}

/// The modes the guest's power HAL reports as supported: the ones that change what the host
/// does. The HAL answers `isModeSupported` with this list, so the framework sends no others.
pub const SUPPORTED_MODES: &[i32] = &[
    mode::LOW_POWER,
    mode::SUSTAINED_PERFORMANCE,
    mode::FIXED_PERFORMANCE,
    mode::VR,
    mode::LAUNCH,
    mode::EXPENSIVE_RENDERING,
    mode::INTERACTIVE,
    mode::DEVICE_IDLE,
    mode::DISPLAY_INACTIVE,
    mode::AUDIO_STREAMING_LOW_LATENCY,
    mode::GAME,
    mode::GAME_LOADING,
];

/// The boosts the guest's power HAL reports as supported.
pub const SUPPORTED_BOOSTS: &[i32] = &[
    boost::INTERACTION,
    boost::DISPLAY_UPDATE_IMMINENT,
    boost::AUDIO_LAUNCH,
    boost::CAMERA_LAUNCH,
    boost::CAMERA_SHOT,
];

/// Modes during which the guest runs at full speed.
const PERFORMANCE_MODES: &[i32] = &[
    mode::SUSTAINED_PERFORMANCE,
    mode::FIXED_PERFORMANCE,
    mode::VR,
    mode::LAUNCH,
    mode::EXPENSIVE_RENDERING,
    mode::AUDIO_STREAMING_LOW_LATENCY,
    mode::GAME,
    mode::GAME_LOADING,
];

/// Modes during which nothing the user sees runs, so the guest may run slowly.
const FRUGAL_MODES: &[i32] = &[mode::LOW_POWER, mode::DEVICE_IDLE, mode::DISPLAY_INACTIVE];

/// How long a boost lasts when the guest does not say.
pub const DEFAULT_BOOST: Duration = Duration::from_millis(1000);
/// The longest a single boost lasts, whatever the guest asks: a guest that keeps asking keeps
/// it, one that stops cannot hold the host's processors for long.
pub const MAX_BOOST: Duration = Duration::from_secs(5);

/// How long the host keeps the guest at full speed after it passes the user's input on: the
/// press, the app's response and the start of whatever the press opens. The host hears of a
/// press before Android does, so it boosts at once rather than waiting for the guest's own
/// interaction boost to come back over the control channel.
pub const HOST_INPUT_BOOST: Duration = Duration::from_millis(1500);

/// The key the host's own input boost is kept under, apart from the guest's boosts (which are
/// never negative).
const HOST_INPUT: i32 = -1;

/// How the host runs the guest's processors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Performance {
    /// Slowly, on the host's efficient cores and clocks: the guest's display is off, it is in
    /// Doze, or low power is on, no performance mode or boost is, and no task window is shown.
    Efficient,
    /// As the host schedules any program.
    Normal,
    /// At full speed, never throttled: a performance mode or a boost is on.
    Full,
}

/// One power hint, as the guest's power HAL sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerHint {
    Mode {
        id: i32,
        enabled: bool,
    },
    /// A boost for `duration_ms` (0 for the default, negative to end it).
    Boost {
        id: i32,
        duration_ms: i32,
    },
}

impl PowerHint {
    /// The hint a control message carries, or why it carries none.
    pub fn from_control(hint: &control::PowerHint) -> Result<PowerHint, String> {
        match PowerHintKind::try_from(hint.kind) {
            Ok(PowerHintKind::Mode) => Ok(PowerHint::Mode {
                id: hint.id,
                enabled: hint.enabled,
            }),
            Ok(PowerHintKind::Boost) => Ok(PowerHint::Boost {
                id: hint.id,
                duration_ms: hint.duration_ms,
            }),
            _ => Err(format!("a power hint of unknown kind {}", hint.kind)),
        }
    }
}

/// The guest's power modes and boosts, and the performance they ask for.
#[derive(Debug, Default)]
pub struct PowerHints {
    modes: BTreeSet<i32>,
    /// Each boost under way, until when.
    boosts: BTreeMap<i32, Instant>,
    /// Whether any task window is shown on the host.
    windows_shown: bool,
}

impl PowerHints {
    pub fn new() -> PowerHints {
        PowerHints::default()
    }

    /// Takes one hint the guest sent at `now`. Modes and boosts the host does not act on are
    /// ignored.
    pub fn hint(&mut self, hint: PowerHint, now: Instant) {
        match hint {
            PowerHint::Mode { id, enabled } => {
                if !SUPPORTED_MODES.contains(&id) {
                    return;
                }
                if enabled {
                    self.modes.insert(id);
                } else {
                    self.modes.remove(&id);
                }
            }
            PowerHint::Boost { id, duration_ms } => {
                if !SUPPORTED_BOOSTS.contains(&id) {
                    return;
                }
                if duration_ms < 0 {
                    self.boosts.remove(&id);
                    return;
                }
                let length = if duration_ms == 0 {
                    DEFAULT_BOOST
                } else {
                    Duration::from_millis(duration_ms as u64).min(MAX_BOOST)
                };
                let until = now + length;
                let entry = self.boosts.entry(id).or_insert(until);
                *entry = (*entry).max(until);
            }
        }
    }

    /// Says whether any task window is shown. While one is, the guest never runs frugally,
    /// whatever its modes say: the user is looking at it.
    pub fn windows_shown(&mut self, shown: bool) {
        self.windows_shown = shown;
    }

    /// Says that the host passed the user's input (a press, a release, a key, a scroll) to
    /// the guest at `now`: the guest runs at full speed for [`HOST_INPUT_BOOST`].
    pub fn host_input(&mut self, now: Instant) {
        let until = now + HOST_INPUT_BOOST;
        let entry = self.boosts.entry(HOST_INPUT).or_insert(until);
        *entry = (*entry).max(until);
    }

    /// Forgets every mode and boost, as when the guest stops: a new guest starts with none.
    /// Whether a window is shown is the host's to say, and stays.
    pub fn clear(&mut self) {
        self.modes.clear();
        self.boosts.clear();
    }

    /// Whether `mode` is on.
    pub fn mode_on(&self, mode: i32) -> bool {
        self.modes.contains(&mode)
    }

    /// The performance the guest wants at `now`.
    pub fn performance(&self, now: Instant) -> Performance {
        let boosted = self.boosts.values().any(|until| *until > now);
        if boosted || PERFORMANCE_MODES.iter().any(|m| self.modes.contains(m)) {
            Performance::Full
        } else if !self.windows_shown && FRUGAL_MODES.iter().any(|m| self.modes.contains(m)) {
            Performance::Efficient
        } else {
            Performance::Normal
        }
    }

    /// What sets the performance at `now`, in words for the log: the boosts under way, the
    /// modes that are on and whether a window is shown.
    pub fn explain(&self, now: Instant) -> String {
        let mut parts = Vec::new();
        for (id, until) in &self.boosts {
            if *until > now {
                if *id == HOST_INPUT {
                    parts.push("the user's input".to_string());
                } else {
                    parts.push(format!("boost {id}"));
                }
            }
        }
        for id in &self.modes {
            parts.push(format!("mode {id}"));
        }
        parts.push(if self.windows_shown {
            "a window shown".to_string()
        } else {
            "no window shown".to_string()
        });
        parts.join(", ")
    }

    /// When the performance may next change without a new hint: the end of the next boost.
    /// `None` when no boost is under way.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        self.boosts
            .values()
            .copied()
            .filter(|until| *until > now)
            .min()
    }
}

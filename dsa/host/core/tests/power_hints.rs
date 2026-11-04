//! How the guest's power hints set the performance the host runs the guest at.

use std::time::{Duration, Instant};

use dsa_host_core::power_hints::{
    boost, mode, Performance, PowerHint, PowerHints, DEFAULT_BOOST, MAX_BOOST,
};
use dsa_protocol::control::{self, PowerHintKind};

fn on(id: i32) -> PowerHint {
    PowerHint::Mode { id, enabled: true }
}

fn off(id: i32) -> PowerHint {
    PowerHint::Mode { id, enabled: false }
}

#[test]
fn pr4_a_guest_without_hints_runs_as_any_program() {
    let hints = PowerHints::new();
    assert_eq!(hints.performance(Instant::now()), Performance::Normal);
}

#[test]
fn pr4_launching_an_app_runs_the_guest_at_full_speed_until_the_launch_ends() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    hints.hint(on(mode::LAUNCH), now);
    assert_eq!(hints.performance(now), Performance::Full);
    hints.hint(off(mode::LAUNCH), now);
    assert_eq!(hints.performance(now), Performance::Normal);
}

#[test]
fn pr4_the_display_off_or_low_power_runs_the_guest_frugally() {
    let now = Instant::now();
    for id in [mode::LOW_POWER, mode::DEVICE_IDLE, mode::DISPLAY_INACTIVE] {
        let mut hints = PowerHints::new();
        hints.hint(on(id), now);
        assert_eq!(hints.performance(now), Performance::Efficient, "mode {id}");
    }
}

#[test]
fn pr4_a_performance_mode_wins_over_low_power() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    hints.hint(on(mode::LOW_POWER), now);
    hints.hint(on(mode::GAME), now);
    assert_eq!(hints.performance(now), Performance::Full);
}

#[test]
fn pr4_a_boost_lasts_as_long_as_asked_and_then_ends() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    hints.hint(on(mode::DISPLAY_INACTIVE), now);
    hints.hint(
        PowerHint::Boost {
            id: boost::INTERACTION,
            duration_ms: 80,
        },
        now,
    );
    assert_eq!(hints.performance(now), Performance::Full);
    assert_eq!(
        hints.next_change(now),
        Some(now + Duration::from_millis(80))
    );
    let later = now + Duration::from_millis(81);
    assert_eq!(hints.performance(later), Performance::Efficient);
    assert_eq!(hints.next_change(later), None);
}

#[test]
fn pr4_a_boost_of_no_length_lasts_the_default_and_a_long_one_is_capped() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    hints.hint(
        PowerHint::Boost {
            id: boost::INTERACTION,
            duration_ms: 0,
        },
        now,
    );
    assert_eq!(hints.next_change(now), Some(now + DEFAULT_BOOST));
    hints.hint(
        PowerHint::Boost {
            id: boost::CAMERA_LAUNCH,
            duration_ms: i32::MAX,
        },
        now,
    );
    assert_eq!(
        hints.performance(now + MAX_BOOST - Duration::from_millis(1)),
        Performance::Full
    );
    assert_eq!(hints.performance(now + MAX_BOOST), Performance::Normal);
}

#[test]
fn pr4_a_negative_length_ends_a_boost_now() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    let interaction = |duration_ms| PowerHint::Boost {
        id: boost::INTERACTION,
        duration_ms,
    };
    hints.hint(interaction(1000), now);
    hints.hint(interaction(-1), now);
    assert_eq!(hints.performance(now), Performance::Normal);
}

#[test]
fn pr4_hints_the_host_does_not_act_on_change_nothing() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    // DOUBLE_TAP_TO_WAKE and ML_ACC.
    hints.hint(on(0), now);
    hints.hint(
        PowerHint::Boost {
            id: 2,
            duration_ms: 1000,
        },
        now,
    );
    assert_eq!(hints.performance(now), Performance::Normal);
    assert!(!hints.mode_on(0));
}

#[test]
fn pr4_a_stopped_guest_leaves_no_mode_or_boost_on() {
    let now = Instant::now();
    let mut hints = PowerHints::new();
    hints.hint(on(mode::GAME), now);
    hints.hint(
        PowerHint::Boost {
            id: boost::INTERACTION,
            duration_ms: 1000,
        },
        now,
    );
    hints.clear();
    assert_eq!(hints.performance(now), Performance::Normal);
    assert_eq!(hints.next_change(now), None);
}

#[test]
fn pr4_a_hint_reads_from_its_control_message() {
    let launch = control::PowerHint {
        kind: PowerHintKind::Mode as i32,
        id: mode::LAUNCH,
        enabled: true,
        duration_ms: 0,
    };
    assert_eq!(PowerHint::from_control(&launch), Ok(on(mode::LAUNCH)));
    let interaction = control::PowerHint {
        kind: PowerHintKind::Boost as i32,
        id: boost::INTERACTION,
        enabled: false,
        duration_ms: 80,
    };
    assert_eq!(
        PowerHint::from_control(&interaction),
        Ok(PowerHint::Boost {
            id: boost::INTERACTION,
            duration_ms: 80,
        })
    );
    let unknown = control::PowerHint { kind: 0, ..launch };
    assert!(PowerHint::from_control(&unknown).is_err());
}

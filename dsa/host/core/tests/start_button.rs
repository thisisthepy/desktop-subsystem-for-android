//! The status card's first button: Start while Android is stopped, Restart while it runs.

use dsa_host_core::session::SessionState;
use dsa_host_core::start_button::{start_button, StartAction, StartButton};

#[test]
fn fr17_the_start_button_becomes_restart_while_android_runs() {
    let start = start_button(SessionState::Stopped, None);
    assert_eq!(start, StartButton::Start);
    assert!(start.enabled() && !start.is_restart() && !start.confirms());
    for running in [
        SessionState::Active,
        SessionState::Dozing,
        SessionState::Frozen,
        SessionState::Stalled,
    ] {
        let button = start_button(running, None);
        assert_eq!(button, StartButton::Restart, "{running}");
        assert!(button.enabled() && button.is_restart() && button.confirms());
    }

    // Under way: locked, with progress, and the label of what is under way.
    assert_eq!(
        start_button(SessionState::Starting, None),
        StartButton::Starting
    );
    assert_eq!(
        start_button(SessionState::Stopping, None),
        StartButton::Stopping
    );
    assert!(!StartButton::Stopping.enabled() && !StartButton::Stopping.is_restart());
    for state in [
        SessionState::Active,
        SessionState::Stopping,
        SessionState::Stopped,
        SessionState::Starting,
    ] {
        let button = start_button(state, Some(StartAction::Restart));
        assert_eq!(button, StartButton::Restarting, "{state}");
        assert!(!button.enabled() && button.in_progress() && button.is_restart());
        let button = start_button(state, Some(StartAction::Start));
        assert_eq!(button, StartButton::Starting, "{state}");
        assert!(!button.enabled() && button.in_progress() && !button.is_restart());
    }

    // The names the settings apps read from `dsa status --json` round-trip.
    for button in [
        StartButton::Start,
        StartButton::Restart,
        StartButton::Starting,
        StartButton::Restarting,
        StartButton::Stopping,
    ] {
        assert_eq!(StartButton::parse(button.as_str()), Some(button));
    }
    assert_eq!(StartButton::parse("later"), None);
}

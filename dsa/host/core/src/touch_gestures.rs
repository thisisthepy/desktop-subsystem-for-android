//! Whether a user's window manager can turn the host's own three- and four-finger touch
//! gestures off while one of the user's task windows is in front.
//!
//! While those gestures are on, the host keeps three or more fingers from every app, so a
//! multi-touch game or a drawing app gets fewer fingers than the user puts down. The user's
//! setting (`touch_gestures_off`) asks the window manager to turn them off while a task window
//! is in front and put the user's value back when another window comes to the front. Only the
//! window manager, which runs in the user's session, can change them; it tells the service
//! what it found, and the service reports that with the settings, so a client hides the
//! setting on a host without the gestures and disables it, with the reason, where the host
//! refused.

/// What the user's window manager says about the host's touch gestures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TouchGesturesState {
    /// The window manager has not said yet, or the service does not report it.
    #[default]
    Unknown,
    /// The host has no such gestures, so there is nothing to turn off.
    Unsupported,
    /// The window manager can turn them off.
    Available,
    /// The host refused to read or change them, for the reason given.
    Failed(String),
}

impl TouchGesturesState {
    /// The state's name on the wire: empty, "unsupported", "available" or "failed".
    pub fn as_str(&self) -> &'static str {
        match self {
            TouchGesturesState::Unknown => "",
            TouchGesturesState::Unsupported => "unsupported",
            TouchGesturesState::Available => "available",
            TouchGesturesState::Failed(_) => "failed",
        }
    }

    /// Why the host refused, or nothing.
    pub fn problem(&self) -> &str {
        match self {
            TouchGesturesState::Failed(problem) => problem,
            _ => "",
        }
    }

    /// The state a name and a problem on the wire stand for. A name this build does not know
    /// reads as unknown.
    pub fn from_wire(state: &str, problem: &str) -> TouchGesturesState {
        match state {
            "unsupported" => TouchGesturesState::Unsupported,
            "available" => TouchGesturesState::Available,
            "failed" => TouchGesturesState::Failed(if problem.is_empty() {
                "the host refused without saying why".to_string()
            } else {
                problem.to_string()
            }),
            _ => TouchGesturesState::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr5_each_touch_gestures_state_crosses_the_wire_unchanged() {
        for state in [
            TouchGesturesState::Unknown,
            TouchGesturesState::Unsupported,
            TouchGesturesState::Available,
            TouchGesturesState::Failed("access denied".into()),
        ] {
            assert_eq!(
                TouchGesturesState::from_wire(state.as_str(), state.problem()),
                state
            );
        }
        assert_eq!(
            TouchGesturesState::from_wire("something-new", ""),
            TouchGesturesState::Unknown
        );
        assert!(!TouchGesturesState::from_wire("failed", "")
            .problem()
            .is_empty());
    }
}

//! The first button of the settings apps' status card: what it reads and whether it can be
//! pressed, from the state of Android and from whether a start or a restart is under way.
//!
//! Every settings app shows the same button for the same state, so the rule lives here once.
//! The Linux app calls [`start_button`]; the Windows and macOS apps read the `start_button`
//! name that `dsa status --json` prints, which comes from the same function.

use crate::session::SessionState;

/// What the settings app is doing with the button right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartAction {
    /// The app asked Android to start and waits for it.
    Start,
    /// The app asked Android to restart and waits for it to run again.
    Restart,
}

/// The status card's first button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartButton {
    /// Android is stopped: "Start", which starts it.
    Start,
    /// Android runs (active, dozing, frozen or stalled): "Restart", which stops and starts it
    /// again after warning that its app windows close.
    Restart,
    /// A start is under way: "Start", locked, with progress beside it.
    Starting,
    /// A restart is under way: "Restart", locked, with progress beside it.
    Restarting,
    /// Android is stopping: "Start", locked, with progress beside it, until it has stopped.
    Stopping,
}

impl StartButton {
    /// Whether pressing the button does something now. A locked button shows progress.
    pub fn enabled(self) -> bool {
        matches!(self, StartButton::Start | StartButton::Restart)
    }

    /// Whether progress shows beside the locked button.
    pub fn in_progress(self) -> bool {
        !self.enabled()
    }

    /// Whether the button reads "Restart" rather than "Start".
    pub fn is_restart(self) -> bool {
        matches!(self, StartButton::Restart | StartButton::Restarting)
    }

    /// Whether pressing it first asks the user, since a restart closes every app window.
    pub fn confirms(self) -> bool {
        self == StartButton::Restart
    }

    /// The name `dsa status --json` prints for it.
    pub fn as_str(self) -> &'static str {
        match self {
            StartButton::Start => "start",
            StartButton::Restart => "restart",
            StartButton::Starting => "starting",
            StartButton::Restarting => "restarting",
            StartButton::Stopping => "stopping",
        }
    }

    /// The button a name of [`StartButton::as_str`] stands for.
    pub fn parse(name: &str) -> Option<StartButton> {
        Some(match name {
            "start" => StartButton::Start,
            "restart" => StartButton::Restart,
            "starting" => StartButton::Starting,
            "restarting" => StartButton::Restarting,
            "stopping" => StartButton::Stopping,
            _ => return None,
        })
    }
}

/// The button for Android in `state` while the app does `action`, if anything.
///
/// An action under way wins over the state: a restart passes through `Stopping`, `Stopped` and
/// `Starting`, and the button stays "Restart", locked, all the way.
pub fn start_button(state: SessionState, action: Option<StartAction>) -> StartButton {
    match action {
        Some(StartAction::Start) => return StartButton::Starting,
        Some(StartAction::Restart) => return StartButton::Restarting,
        None => {}
    }
    match state {
        SessionState::Stopped => StartButton::Start,
        SessionState::Starting => StartButton::Starting,
        SessionState::Stopping => StartButton::Stopping,
        SessionState::Active
        | SessionState::Dozing
        | SessionState::Frozen
        | SessionState::Stalled => StartButton::Restart,
    }
}

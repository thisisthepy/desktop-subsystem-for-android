//! What a press on a widget window is doing: a tap or a drag that is the widget's own, a long
//! press that lifts it for editing, a move or a resize of its window. When a resize passes its
//! size on, and what becomes of an edit when the widget's menu closes.

use std::time::{Duration, Instant};

use dsa_protocol::host_api::types::PointerAction;

use super::geometry::{resize_rect, Hit, ScreenRect, HOLD_TO_MOVE};

/// How long a resize must rest before the size it has reached is passed on. Each size Android
/// is told makes it lay out and draw the widget's views anew, which costs memory and time, so
/// a size is passed on when the user pauses on it rather than at every step of the drag.
pub const RESIZE_PAUSE: Duration = Duration::from_millis(150);

/// The least time between two sizes passed on for a widget being resized.
pub const RESIZE_REPORT: Duration = Duration::from_millis(250);

/// How often a window being resized checks whether its size is due to be passed on.
pub const RESIZE_POLL: Duration = Duration::from_millis(50);

/// When the size of a widget being resized is passed on: when the drag pauses on a size for
/// [`RESIZE_PAUSE`] and when it ends, never more often than every [`RESIZE_REPORT`], and only
/// a size other than the last one passed on. Between them the window shows the widget's
/// last image unscaled ([`super::geometry::unscaled`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeReports<T> {
    /// The size the drag has reached, and when it last changed.
    current: Option<(T, Instant)>,
    /// The size last passed on, and when.
    sent: Option<(T, Instant)>,
}

/// What a resize asks of its window when [`ResizeReports::poll`] is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizePoll<T> {
    /// Pass this size on now.
    Send(T),
    /// A size is waiting: check again in a while.
    Wait,
    /// Nothing is waiting to be passed on.
    Idle,
}

impl<T: Copy + PartialEq> Default for ResizeReports<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq> ResizeReports<T> {
    /// Nothing reached and nothing passed on yet.
    pub fn new() -> ResizeReports<T> {
        ResizeReports {
            current: None,
            sent: None,
        }
    }

    /// A new drag begins: what the last one passed on, and when, still counts.
    pub fn restart(&mut self) {
        self.current = None;
    }

    /// The drag reached `size` at `now`.
    pub fn moved(&mut self, size: T, now: Instant) {
        if self.current.map(|(at, _)| at) != Some(size) {
            self.current = Some((size, now));
        }
    }

    /// Whether the size the drag reached is due to be passed on at `now`; `ended` once the
    /// user let go, when it no longer waits for a pause.
    pub fn poll(&mut self, now: Instant, ended: bool) -> ResizePoll<T> {
        let Some((size, changed)) = self.current else {
            return ResizePoll::Idle;
        };
        if self.sent.map(|(sent, _)| sent) == Some(size) {
            return ResizePoll::Idle;
        }
        let paused = ended || now.saturating_duration_since(changed) >= RESIZE_PAUSE;
        let spaced = self
            .sent
            .is_none_or(|(_, at)| now.saturating_duration_since(at) >= RESIZE_REPORT);
        if paused && spaced {
            self.sent = Some((size, now));
            ResizePoll::Send(size)
        } else {
            ResizePoll::Wait
        }
    }
}

/// What a press on a widget window is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    Idle,
    /// Pressed inside the widget, at `down` on the screen, `since` then; Android has the
    /// press. Once it `slid` past the drag threshold it is the widget's own drag.
    Widget {
        down: (i32, i32),
        since: Instant,
        slid: bool,
    },
    /// A long press: held still for [`HOLD_TO_MOVE`], so the widget is lifted for editing and
    /// Android's press was cancelled. The pointer is at `at` on the screen, `grab` pixels
    /// from the window's top left corner; dragging past the threshold moves the widget, and
    /// letting go opens its menu.
    Lifted {
        at: (i32, i32),
        grab: (i32, i32),
    },
    /// Pressed on a widget being edited, at `down` on the screen: Android never has it. A
    /// drag moves the widget and a click opens its menu.
    Editing {
        down: (i32, i32),
    },
    /// Moving the window, the pointer `grab` pixels from its top left corner.
    Moving {
        grab: (i32, i32),
    },
    /// Resizing the window by `edge`, pressed at `down` on the screen when the window was
    /// `start`, within `min` and `max` (its width and height in pixels): an edge of the
    /// widget, or a handle or edge of one being edited. Android never has it.
    Resizing {
        edge: Hit,
        down: (i32, i32),
        start: ScreenRect,
        min: (i32, i32),
        max: (i32, i32),
    },
}

/// Why a press held until its timer came does not lift the widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldRefused {
    /// It moved past the drag threshold first, so it is the widget's own drag.
    Slid,
}

impl HoldRefused {
    /// The reason, for the log.
    pub fn reason(self) -> &'static str {
        match self {
            HoldRefused::Slid => {
                "it moved past the drag threshold first, so it is the widget's own drag"
            }
        }
    }
}

/// What a step of a gesture asks of the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Nothing,
    /// The pointer goes to Android as this action.
    Android(PointerAction),
    /// The press was held long enough: Android's press is cancelled, so it never becomes a
    /// click, and the widget is lifted for editing.
    Lift,
    /// The press is still a long press, but the hold was checked before [`HOLD_TO_MOVE`] had
    /// passed (the system's timer can come up to a tick early): it is checked again once
    /// this much more has passed.
    HoldOn(Duration),
    /// The hold was checked in time but does not lift the widget, for this reason.
    HoldRefused(HoldRefused),
    /// A long press, or a click on a widget being edited, ended where it began: the menu of
    /// the widget opens under it.
    EditMenu,
    /// The window starts moving, to this top left corner.
    StartMove {
        to: (i32, i32),
    },
    /// The window moves to this top left corner.
    MoveTo((i32, i32)),
    /// The move ended: the service is told where the window is, and an edit ends.
    EndMove,
    /// A resize begins by this edge: the cursor shows which way it goes.
    StartResize(Hit),
    /// The window takes this rectangle on the screen.
    ResizeTo(ScreenRect),
    /// The resize ended: the service is told where the window is and how large, and an edit
    /// ends.
    EndResize,
}

/// Whether `at` is past the drag threshold (`threshold` pixels each way) from `down`.
fn beyond(down: (i32, i32), at: (i32, i32), threshold: (i32, i32)) -> bool {
    (at.0 - down.0).abs() > threshold.0 || (at.1 - down.1).abs() > threshold.1
}

/// The milliseconds a timer is set for to check a hold again `left` from now: rounded up,
/// and at least one, so it never comes before the hold is due.
pub fn hold_timer_ms(left: Duration) -> u32 {
    let ms = left.as_micros().div_ceil(1000);
    ms.clamp(1, u128::from(u32::MAX)) as u32
}

/// What a press is about to do, from where it went down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Press {
    /// Where it hit the window ([`super::geometry::hit_test`]).
    pub hit: Hit,
    /// Whether the widget is being edited.
    pub editing: bool,
    /// The window's rectangle on the screen.
    pub window: ScreenRect,
    /// The smallest and largest the window may be, width and height in pixels.
    pub min: (i32, i32),
    pub max: (i32, i32),
}

impl Gesture {
    /// The button went down at `at` on the screen: on an edge that resizes the window, which
    /// starts a resize the same way for a widget and one being edited; on a widget being
    /// edited, which it moves or opens the menu of; or on the widget, whose press Android
    /// has until it is a long press.
    pub fn press(&mut self, press: Press, at: (i32, i32), now: Instant) -> Step {
        if press.hit.sides().is_some() {
            *self = Gesture::Resizing {
                edge: press.hit,
                down: at,
                start: press.window,
                min: press.min,
                max: press.max,
            };
            return Step::StartResize(press.hit);
        }
        self.down(press.editing, at, now)
    }

    /// The button went down at `at` on the screen, inside a widget being edited (`editing`)
    /// or not.
    pub fn down(&mut self, editing: bool, at: (i32, i32), now: Instant) -> Step {
        if editing {
            *self = Gesture::Editing { down: at };
            Step::Nothing
        } else {
            *self = Gesture::Widget {
                down: at,
                since: now,
                slid: false,
            };
            Step::Android(PointerAction::Down)
        }
    }

    /// The pointer moved to `at` on the screen, the window's top left corner at `origin`.
    pub fn moved(&mut self, at: (i32, i32), origin: (i32, i32), threshold: (i32, i32)) -> Step {
        match *self {
            Gesture::Idle => Step::Nothing,
            Gesture::Widget { down, since, slid } => {
                if !slid && beyond(down, at, threshold) {
                    *self = Gesture::Widget {
                        down,
                        since,
                        slid: true,
                    };
                }
                Step::Android(PointerAction::Move)
            }
            Gesture::Editing { down } if beyond(down, at, threshold) => {
                let grab = (down.0 - origin.0, down.1 - origin.1);
                *self = Gesture::Moving { grab };
                Step::StartMove {
                    to: (at.0 - grab.0, at.1 - grab.1),
                }
            }
            Gesture::Editing { .. } => Step::Nothing,
            Gesture::Lifted { at: held, grab } if beyond(held, at, threshold) => {
                *self = Gesture::Moving { grab };
                Step::StartMove {
                    to: (at.0 - grab.0, at.1 - grab.1),
                }
            }
            Gesture::Lifted { .. } => Step::Nothing,
            Gesture::Moving { grab } => Step::MoveTo((at.0 - grab.0, at.1 - grab.1)),
            Gesture::Resizing {
                edge,
                down,
                start,
                min,
                max,
            } => Step::ResizeTo(resize_rect(edge, start, down, at, min, max)),
        }
    }

    /// The press has been held until `now`, the pointer at `at` on the screen and the
    /// window's top left corner at `origin`: one held still for [`HOLD_TO_MOVE`] is a long
    /// press, which lifts the widget for editing. Checked a little early, it asks to be
    /// checked again ([`Step::HoldOn`]) rather than letting the long press go.
    pub fn held(&mut self, at: (i32, i32), origin: (i32, i32), now: Instant) -> Step {
        let Gesture::Widget { since, slid, .. } = *self else {
            return Step::Nothing;
        };
        if slid {
            return Step::HoldRefused(HoldRefused::Slid);
        }
        let waited = now.saturating_duration_since(since);
        if waited < HOLD_TO_MOVE {
            return Step::HoldOn(HOLD_TO_MOVE - waited);
        }
        let grab = (at.0 - origin.0, at.1 - origin.1);
        *self = Gesture::Lifted { at, grab };
        Step::Lift
    }

    /// The window grew or shrank by `by` pixels on its left and top as an edit began or
    /// ended under the press, so the pointer is that much further from its corner.
    pub fn regrab(&mut self, by: (i32, i32)) {
        if let Gesture::Lifted { grab, .. } | Gesture::Moving { grab } = self {
            *grab = (grab.0 + by.0, grab.1 + by.1);
        }
    }

    /// The button went up.
    pub fn up(&mut self) -> Step {
        match std::mem::replace(self, Gesture::Idle) {
            Gesture::Idle => Step::Nothing,
            Gesture::Widget { .. } => Step::Android(PointerAction::Up),
            Gesture::Lifted { .. } | Gesture::Editing { .. } => Step::EditMenu,
            Gesture::Moving { .. } => Step::EndMove,
            Gesture::Resizing { .. } => Step::EndResize,
        }
    }

    /// The window lost the mouse before the button went up.
    pub fn cancel(&mut self) -> Step {
        match std::mem::replace(self, Gesture::Idle) {
            Gesture::Idle | Gesture::Lifted { .. } | Gesture::Editing { .. } => Step::Nothing,
            Gesture::Widget { .. } => Step::Android(PointerAction::Cancel),
            Gesture::Moving { .. } => Step::EndMove,
            Gesture::Resizing { .. } => Step::EndResize,
        }
    }

    /// Whether the user is moving or resizing the window, which a placement from the service
    /// does not undo meanwhile.
    pub fn dragging(&self) -> bool {
        matches!(self, Gesture::Moving { .. } | Gesture::Resizing { .. })
    }
}

/// Where a widget's menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAt {
    /// Under the widget, after a long press or a click on it while it is edited.
    Widget,
    /// At this point on the screen, where it was right-clicked.
    Point(i32, i32),
    /// Under the widget, from the keyboard (the menu key or Shift+F10).
    Keyboard,
}

/// What becomes of an edit when its widget's menu closed without a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuClosed {
    /// Closed with Esc, or by a click that is over: the widget stays lifted.
    KeepEditing,
    /// Closed by a press beside the widget or on another window: the edit ends, and the
    /// window that had the keyboard before gets it back when `give_back`.
    EndEdit { give_back: bool },
    /// Closed by a press of the left button on a handle or an edge, still held: the menu
    /// took the press, so it goes on as a press of that handle, which resizes the widget.
    /// Without this a handle could not be dragged after the menu of a long press opened: the
    /// first press only closed the menu. A press on the widget itself only closes the menu,
    /// so letting go there does not open it again.
    GoOnPressing,
}

/// What a widget being edited does after its menu closed without a choice, the pointer over
/// `hit` of its window ([`super::geometry::hit_test`]), the left and right buttons `left` and
/// `right` held or not, and the window still `foreground` or not.
pub fn menu_closed_by(hit: Hit, left: bool, right: bool, foreground: bool) -> MenuClosed {
    if !foreground {
        return MenuClosed::EndEdit { give_back: false };
    }
    match hit {
        Hit::Outside if left || right => MenuClosed::EndEdit { give_back: true },
        _ if left && hit.sides().is_some() => MenuClosed::GoOnPressing,
        _ => MenuClosed::KeepEditing,
    }
}

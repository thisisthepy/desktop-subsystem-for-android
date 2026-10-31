//! Pointer input: what the mouse, a finger or a pen does in a task's window, as the guest
//! task receives it.
//!
//! A platform's window turns its own messages into [`WindowInput`], in the window's client
//! pixels, and hands them to the window's [`PointerMapper`]. The mapper keeps the mouse's
//! state (which buttons are down, whether it hovers), converts every position to the task's
//! pixels on the guest display, and returns the [`PointerEvent`]s to send, with whether the
//! window must hold on to the mouse while a button is down. Nothing here names a window
//! system, so the rules run in tests and in whichever process owns the window.

use dsa_protocol::control;
use dsa_protocol::host_api::types as wire;

/// The buttons held down, as bits of [`PointerEvent::buttons`]. The values are the guest's
/// own, so they pass through unchanged.
pub mod buttons {
    pub const PRIMARY: u32 = 1;
    pub const SECONDARY: u32 = 2;
    pub const TERTIARY: u32 = 4;
    pub const BACK: u32 = 8;
    pub const FORWARD: u32 = 16;
    /// A pen's first barrel button.
    pub const STYLUS_PRIMARY: u32 = 32;
    /// A pen's second barrel button.
    pub const STYLUS_SECONDARY: u32 = 64;
}

/// What a pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerAction {
    /// The pointer touched down, or a mouse's first button went down.
    Down,
    /// The pointer moved while down, or a mouse's buttons changed while one stays down.
    Move,
    /// The pointer lifted, or a mouse's last button went up.
    Up,
    /// The gesture ended without completing; the guest drops it.
    Cancel,
    /// The wheel turned.
    Scroll,
    /// The pointer moved over the window without touching it.
    HoverMove,
    /// The pointer left the window, or a pen went out of range.
    HoverExit,
}

/// What kind of pointer it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerTool {
    Mouse,
    Touch,
    Pen,
    /// A pen's eraser end.
    Eraser,
}

/// One pointer event for a guest task.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub task_id: i32,
    pub action: PointerAction,
    pub tool: PointerTool,
    /// Tells the fingers of one touch apart; the same from down to up.
    pub pointer_id: u32,
    /// The position relative to the task's top left corner, in guest display pixels.
    pub x: f32,
    pub y: f32,
    /// The buttons held down after this event ([`buttons`]).
    pub buttons: u32,
    /// From 0 to 1.
    pub pressure: f32,
    /// For [`PointerAction::Scroll`]: notches; positive scrolls right.
    pub scroll_x: f32,
    /// For [`PointerAction::Scroll`]: notches; positive scrolls up.
    pub scroll_y: f32,
}

impl PointerEvent {
    /// An event with no buttons, no pressure and no scroll.
    pub fn new(
        task_id: i32,
        action: PointerAction,
        tool: PointerTool,
        (x, y): (f32, f32),
    ) -> PointerEvent {
        PointerEvent {
            task_id,
            action,
            tool,
            pointer_id: 0,
            x,
            y,
            buttons: 0,
            pressure: 0.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
        }
    }

    /// The event as the control channel carries it to the guest.
    pub fn to_control(&self) -> control::PointerEvent {
        control::PointerEvent {
            task_id: self.task_id,
            action: match self.action {
                PointerAction::Down => control::PointerAction::Down,
                PointerAction::Move => control::PointerAction::Move,
                PointerAction::Up => control::PointerAction::Up,
                PointerAction::Cancel => control::PointerAction::Cancel,
                PointerAction::Scroll => control::PointerAction::Scroll,
                PointerAction::HoverMove => control::PointerAction::HoverMove,
                PointerAction::HoverExit => control::PointerAction::HoverExit,
            } as i32,
            tool: match self.tool {
                PointerTool::Mouse => control::PointerTool::Mouse,
                PointerTool::Touch => control::PointerTool::Touch,
                PointerTool::Pen => control::PointerTool::Pen,
                PointerTool::Eraser => control::PointerTool::Eraser,
            } as i32,
            pointer_id: self.pointer_id,
            x: self.x,
            y: self.y,
            buttons: self.buttons,
            pressure: self.pressure,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    /// The event as the local API carries it to the service.
    pub fn to_wire(&self) -> wire::PointerEvent {
        wire::PointerEvent {
            task_id: self.task_id,
            action: match self.action {
                PointerAction::Down => wire::PointerAction::Down,
                PointerAction::Move => wire::PointerAction::Move,
                PointerAction::Up => wire::PointerAction::Up,
                PointerAction::Cancel => wire::PointerAction::Cancel,
                PointerAction::Scroll => wire::PointerAction::Scroll,
                PointerAction::HoverMove => wire::PointerAction::HoverMove,
                PointerAction::HoverExit => wire::PointerAction::HoverExit,
            } as i32,
            tool: match self.tool {
                PointerTool::Mouse => wire::PointerTool::Mouse,
                PointerTool::Touch => wire::PointerTool::Touch,
                PointerTool::Pen => wire::PointerTool::Pen,
                PointerTool::Eraser => wire::PointerTool::Eraser,
            } as i32,
            pointer_id: self.pointer_id,
            x: self.x,
            y: self.y,
            buttons: self.buttons,
            pressure: self.pressure,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
        }
    }

    /// The event the local API carried, or a reason it makes no sense: an action or tool
    /// this side does not know, or a position that is not a number.
    pub fn from_wire(event: &wire::PointerEvent) -> Result<PointerEvent, String> {
        let action = match wire::PointerAction::try_from(event.action) {
            Ok(wire::PointerAction::Down) => PointerAction::Down,
            Ok(wire::PointerAction::Move) => PointerAction::Move,
            Ok(wire::PointerAction::Up) => PointerAction::Up,
            Ok(wire::PointerAction::Cancel) => PointerAction::Cancel,
            Ok(wire::PointerAction::Scroll) => PointerAction::Scroll,
            Ok(wire::PointerAction::HoverMove) => PointerAction::HoverMove,
            Ok(wire::PointerAction::HoverExit) => PointerAction::HoverExit,
            Ok(wire::PointerAction::Unspecified) | Err(_) => {
                return Err(format!("unknown pointer action {}", event.action))
            }
        };
        let tool = match wire::PointerTool::try_from(event.tool) {
            Ok(wire::PointerTool::Mouse) => PointerTool::Mouse,
            Ok(wire::PointerTool::Touch) => PointerTool::Touch,
            Ok(wire::PointerTool::Pen) => PointerTool::Pen,
            Ok(wire::PointerTool::Eraser) => PointerTool::Eraser,
            Ok(wire::PointerTool::Unspecified) | Err(_) => {
                return Err(format!("unknown pointer tool {}", event.tool))
            }
        };
        let numbers = [
            event.x,
            event.y,
            event.pressure,
            event.scroll_x,
            event.scroll_y,
        ];
        if numbers.iter().any(|number| !number.is_finite()) {
            return Err("a pointer position, pressure or scroll is not a number".into());
        }
        Ok(PointerEvent {
            task_id: event.task_id,
            action,
            tool,
            pointer_id: event.pointer_id,
            x: event.x,
            y: event.y,
            buttons: event.buttons,
            pressure: event.pressure.clamp(0.0, 1.0),
            scroll_x: event.scroll_x,
            scroll_y: event.scroll_y,
        })
    }
}

/// One event of a task window's input, of any kind, in the order the window saw them.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Pointer(PointerEvent),
    Key(crate::keys::KeyEvent),
    /// Text the window's input method composed or committed, or a key typed.
    Ime(crate::ime::ImeEvent),
}

impl From<crate::ime::ImeEvent> for InputEvent {
    fn from(event: crate::ime::ImeEvent) -> InputEvent {
        InputEvent::Ime(event)
    }
}

impl From<PointerEvent> for InputEvent {
    fn from(event: PointerEvent) -> InputEvent {
        InputEvent::Pointer(event)
    }
}

impl From<crate::keys::KeyEvent> for InputEvent {
    fn from(event: crate::keys::KeyEvent) -> InputEvent {
        InputEvent::Key(event)
    }
}

/// The sizes a window's input is converted between: its client area, in the window
/// system's pixels, and the task it shows, in guest display pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub client: (u32, u32),
    pub task: (u32, u32),
}

impl WindowGeometry {
    /// Converts a point in the client area to the task's guest pixels.
    ///
    /// The client area shows the whole task, whatever the window's size and the monitor's
    /// scale, so the conversion is the ratio of the two sizes. A point inside the client
    /// area always lands inside the task, so a click on the window's edge never reaches the
    /// task beside it on the guest display. A point outside (the mouse dragged out of the
    /// window while a button is down) keeps going, as it would on a phone's screen edge.
    pub fn to_guest(&self, (x, y): (f64, f64)) -> (f32, f32) {
        let axis = |position: f64, client: u32, task: u32| -> f32 {
            if client == 0 || task == 0 {
                return position as f32;
            }
            let scaled = position * f64::from(task) / f64::from(client);
            if (0.0..f64::from(client)).contains(&position) {
                scaled.clamp(0.0, f64::from(task - 1)) as f32
            } else {
                scaled as f32
            }
        };
        (
            axis(x, self.client.0, self.task.0),
            axis(y, self.client.1, self.task.1),
        )
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    /// The button's bit in [`PointerEvent::buttons`]. The right button is the secondary
    /// button, which the guest treats as a context click rather than a long press.
    pub fn bit(self) -> u32 {
        match self {
            MouseButton::Left => buttons::PRIMARY,
            MouseButton::Right => buttons::SECONDARY,
            MouseButton::Middle => buttons::TERTIARY,
            MouseButton::Back => buttons::BACK,
            MouseButton::Forward => buttons::FORWARD,
        }
    }
}

/// Where a finger or a pen is in its contact with the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactPhase {
    /// It touched the screen.
    Down,
    /// It moved while touching.
    Move,
    /// It lifted.
    Up,
    /// The window system took the contact away (a gesture of its own, say).
    Cancel,
    /// A pen moved in range without touching.
    Hover,
    /// A pen went out of range, or left the window.
    HoverExit,
}

/// What happened in a window, in its client pixels, before any of it means anything to
/// the guest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowInput {
    /// The mouse moved to `position`.
    MouseMove { position: (f64, f64) },
    /// A mouse button went down or up at `position`.
    MouseButton {
        button: MouseButton,
        down: bool,
        position: (f64, f64),
    },
    /// The wheel turned with the mouse at `position`, by `notches` (horizontal, vertical;
    /// positive is right and away from the user).
    Wheel {
        position: (f64, f64),
        notches: (f32, f32),
    },
    /// The mouse left the window.
    MouseLeave,
    /// The window lost hold of the mouse while it had it (another window took it, or the
    /// user switched away).
    CaptureLost,
    /// A finger or a pen.
    Contact {
        tool: PointerTool,
        pointer_id: u32,
        phase: ContactPhase,
        position: (f64, f64),
        /// From 0 to 1; `None` when the device does not measure it.
        pressure: Option<f32>,
        /// A pen's barrel buttons ([`buttons::STYLUS_PRIMARY`], [`buttons::STYLUS_SECONDARY`]).
        buttons: u32,
    },
}

/// Whether the window must take or give up hold of the mouse, so that a drag that leaves
/// the window still reaches the task and ends there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    Take,
    Release,
}

/// What a [`WindowInput`] comes to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mapped {
    /// The events to send to the guest, in order.
    pub events: Vec<PointerEvent>,
    pub capture: Option<Capture>,
}

/// One window's pointer state.
#[derive(Debug, Clone)]
pub struct PointerMapper {
    task_id: i32,
    /// The mouse buttons down now.
    buttons: u32,
    /// Whether the mouse is hovering over the window with no button down.
    hovering: bool,
    /// Where the mouse last was, in guest pixels.
    last: (f32, f32),
}

impl PointerMapper {
    pub fn new(task_id: i32) -> PointerMapper {
        PointerMapper {
            task_id,
            buttons: 0,
            hovering: false,
            last: (0.0, 0.0),
        }
    }

    /// The mouse buttons down now.
    pub fn buttons(&self) -> u32 {
        self.buttons
    }

    fn mouse(&self, action: PointerAction, position: (f32, f32)) -> PointerEvent {
        let mut event = PointerEvent::new(self.task_id, action, PointerTool::Mouse, position);
        event.buttons = self.buttons;
        event.pressure = if self.buttons != 0 { 1.0 } else { 0.0 };
        event
    }

    /// Turns one thing that happened in the window into what the guest receives.
    pub fn handle(&mut self, input: WindowInput, geometry: WindowGeometry) -> Mapped {
        let mut mapped = Mapped::default();
        match input {
            WindowInput::MouseMove { position } => {
                let position = geometry.to_guest(position);
                self.last = position;
                if self.buttons != 0 {
                    mapped
                        .events
                        .push(self.mouse(PointerAction::Move, position));
                } else {
                    self.hovering = true;
                    mapped
                        .events
                        .push(self.mouse(PointerAction::HoverMove, position));
                }
            }
            WindowInput::MouseButton {
                button,
                down,
                position,
            } => {
                let position = geometry.to_guest(position);
                self.last = position;
                let bit = button.bit();
                let before = self.buttons;
                if down {
                    if before & bit != 0 {
                        return mapped;
                    }
                    if before == 0 && self.hovering {
                        // The guest's pointer stops hovering before it presses.
                        self.hovering = false;
                        mapped
                            .events
                            .push(self.mouse(PointerAction::HoverExit, position));
                    }
                    self.buttons |= bit;
                    if before == 0 {
                        mapped
                            .events
                            .push(self.mouse(PointerAction::Down, position));
                        mapped.capture = Some(Capture::Take);
                    } else {
                        mapped
                            .events
                            .push(self.mouse(PointerAction::Move, position));
                    }
                } else {
                    if before & bit == 0 {
                        return mapped;
                    }
                    self.buttons &= !bit;
                    if self.buttons == 0 {
                        let mut up = self.mouse(PointerAction::Up, position);
                        // The pressure of the press that ends, as the guest expects of an up.
                        up.pressure = 1.0;
                        mapped.events.push(up);
                        mapped.capture = Some(Capture::Release);
                    } else {
                        mapped
                            .events
                            .push(self.mouse(PointerAction::Move, position));
                    }
                }
            }
            WindowInput::Wheel { position, notches } => {
                let position = geometry.to_guest(position);
                self.last = position;
                let mut event = self.mouse(PointerAction::Scroll, position);
                event.scroll_x = notches.0;
                event.scroll_y = notches.1;
                mapped.events.push(event);
            }
            WindowInput::MouseLeave => {
                if self.buttons == 0 && self.hovering {
                    self.hovering = false;
                    mapped
                        .events
                        .push(self.mouse(PointerAction::HoverExit, self.last));
                }
            }
            WindowInput::CaptureLost => {
                if self.buttons != 0 {
                    // Neither a click nor a drop: the guest is told the gesture is off.
                    let mut cancel = self.mouse(PointerAction::Cancel, self.last);
                    self.buttons = 0;
                    cancel.buttons = 0;
                    mapped.events.push(cancel);
                }
            }
            WindowInput::Contact {
                tool,
                pointer_id,
                phase,
                position,
                pressure,
                buttons,
            } => {
                let action = match phase {
                    ContactPhase::Down => PointerAction::Down,
                    ContactPhase::Move => PointerAction::Move,
                    ContactPhase::Up => PointerAction::Up,
                    ContactPhase::Cancel => PointerAction::Cancel,
                    ContactPhase::Hover => PointerAction::HoverMove,
                    ContactPhase::HoverExit => PointerAction::HoverExit,
                };
                let touching = matches!(phase, ContactPhase::Down | ContactPhase::Move);
                let mut event =
                    PointerEvent::new(self.task_id, action, tool, geometry.to_guest(position));
                event.pointer_id = pointer_id;
                event.buttons = buttons;
                event.pressure = match (pressure, phase) {
                    (Some(pressure), _) => pressure.clamp(0.0, 1.0),
                    (None, ContactPhase::Up) => 1.0,
                    (None, _) if touching => 1.0,
                    (None, _) => 0.0,
                };
                mapped.events.push(event);
            }
        }
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client size a window has at `scale` when it shows a task of `task` pixels at
    /// one window pixel per guest pixel per 100 %.
    fn at_scale(task: (u32, u32), scale: f64) -> WindowGeometry {
        let client = (
            (f64::from(task.0) * scale).round() as u32,
            (f64::from(task.1) * scale).round() as u32,
        );
        WindowGeometry { client, task }
    }

    fn corners(geometry: WindowGeometry) -> [((f64, f64), (f32, f32)); 4] {
        let (cw, ch) = (f64::from(geometry.client.0), f64::from(geometry.client.1));
        let (tw, th) = (geometry.task.0 as f32, geometry.task.1 as f32);
        [
            ((0.0, 0.0), (0.0, 0.0)),
            ((cw - 1.0, 0.0), (tw - 1.0, 0.0)),
            ((0.0, ch - 1.0), (0.0, th - 1.0)),
            ((cw - 1.0, ch - 1.0), (tw - 1.0, th - 1.0)),
        ]
    }

    fn assert_within(actual: (f32, f32), expected: (f32, f32), slack: f32, what: &str) {
        assert!(
            (actual.0 - expected.0).abs() <= slack && (actual.1 - expected.1).abs() <= slack,
            "{what}: landed at {actual:?}, expected {expected:?} within {slack} px"
        );
    }

    #[test]
    fn fr5_the_four_corners_land_within_2_px_at_every_scale() {
        for task in [(548, 974), (1080, 1920), (800, 600), (1, 1)] {
            for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
                let geometry = at_scale(task, scale);
                for (client, guest) in corners(geometry) {
                    let landed = geometry.to_guest(client);
                    assert_within(
                        landed,
                        guest,
                        2.0,
                        &format!("task {task:?} at {scale}x, client point {client:?}"),
                    );
                    // A corner of the window is always a point of the task.
                    assert!(landed.0 >= 0.0 && landed.0 <= (task.0 - 1) as f32);
                    assert!(landed.1 >= 0.0 && landed.1 <= (task.1 - 1) as f32);
                }
            }
        }
    }

    #[test]
    fn fr5_a_window_smaller_than_its_task_still_reaches_every_corner() {
        // A task taller than the screen opens in a smaller window of the same shape.
        let geometry = WindowGeometry {
            client: (585, 1040),
            task: (720, 1280),
        };
        for (client, guest) in corners(geometry) {
            assert_within(geometry.to_guest(client), guest, 2.0, "shrunk window");
        }
    }

    #[test]
    fn fr5_a_point_is_relative_to_the_task_window() {
        let geometry = at_scale((400, 300), 1.5);
        assert_eq!(geometry.to_guest((300.0, 225.0)), (200.0, 150.0));
        let geometry = at_scale((400, 300), 1.0);
        assert_eq!(geometry.to_guest((10.0, 20.0)), (10.0, 20.0));
    }

    #[test]
    fn fr5_a_drag_out_of_the_window_keeps_its_position() {
        let geometry = at_scale((400, 300), 2.0);
        assert_eq!(geometry.to_guest((-20.0, 700.0)), (-10.0, 350.0));
    }

    fn geometry() -> WindowGeometry {
        WindowGeometry {
            client: (800, 600),
            task: (400, 300),
        }
    }

    fn button(button: MouseButton, down: bool, at: (f64, f64)) -> WindowInput {
        WindowInput::MouseButton {
            button,
            down,
            position: at,
        }
    }

    fn actions(mapped: &Mapped) -> Vec<(PointerAction, u32)> {
        mapped
            .events
            .iter()
            .map(|event| (event.action, event.buttons))
            .collect()
    }

    #[test]
    fn fr5_a_left_click_is_a_down_and_an_up_with_the_mouse_held_between() {
        let mut mapper = PointerMapper::new(7);
        let down = mapper.handle(button(MouseButton::Left, true, (100.0, 50.0)), geometry());
        assert_eq!(
            actions(&down),
            vec![(PointerAction::Down, buttons::PRIMARY)]
        );
        assert_eq!(down.capture, Some(Capture::Take));
        let event = down.events[0];
        assert_eq!((event.task_id, event.tool), (7, PointerTool::Mouse));
        assert_eq!((event.x, event.y), (50.0, 25.0));
        assert_eq!(event.pressure, 1.0);

        let drag = mapper.handle(
            WindowInput::MouseMove {
                position: (120.0, 60.0),
            },
            geometry(),
        );
        assert_eq!(
            actions(&drag),
            vec![(PointerAction::Move, buttons::PRIMARY)]
        );
        assert_eq!(drag.capture, None);

        let up = mapper.handle(button(MouseButton::Left, false, (120.0, 60.0)), geometry());
        assert_eq!(actions(&up), vec![(PointerAction::Up, 0)]);
        assert_eq!(up.capture, Some(Capture::Release));
    }

    #[test]
    fn fr5_a_right_click_is_a_secondary_click_not_a_long_press() {
        let mut mapper = PointerMapper::new(7);
        let down = mapper.handle(button(MouseButton::Right, true, (10.0, 10.0)), geometry());
        assert_eq!(
            actions(&down),
            vec![(PointerAction::Down, buttons::SECONDARY)]
        );
        let up = mapper.handle(button(MouseButton::Right, false, (10.0, 10.0)), geometry());
        assert_eq!(actions(&up), vec![(PointerAction::Up, 0)]);
    }

    #[test]
    fn fr5_a_second_button_changes_the_buttons_without_a_second_down() {
        let mut mapper = PointerMapper::new(7);
        mapper.handle(button(MouseButton::Left, true, (10.0, 10.0)), geometry());
        let second = mapper.handle(button(MouseButton::Right, true, (10.0, 10.0)), geometry());
        assert_eq!(
            actions(&second),
            vec![(PointerAction::Move, buttons::PRIMARY | buttons::SECONDARY)]
        );
        assert_eq!(second.capture, None);
        let first_up = mapper.handle(button(MouseButton::Left, false, (10.0, 10.0)), geometry());
        assert_eq!(
            actions(&first_up),
            vec![(PointerAction::Move, buttons::SECONDARY)]
        );
        assert_eq!(first_up.capture, None);
        let last_up = mapper.handle(button(MouseButton::Right, false, (10.0, 10.0)), geometry());
        assert_eq!(actions(&last_up), vec![(PointerAction::Up, 0)]);
        assert_eq!(last_up.capture, Some(Capture::Release));
    }

    #[test]
    fn fr5_a_button_the_window_never_saw_go_down_is_ignored() {
        let mut mapper = PointerMapper::new(7);
        assert!(mapper
            .handle(button(MouseButton::Left, false, (10.0, 10.0)), geometry())
            .events
            .is_empty());
        mapper.handle(button(MouseButton::Left, true, (10.0, 10.0)), geometry());
        assert!(mapper
            .handle(button(MouseButton::Left, true, (10.0, 10.0)), geometry())
            .events
            .is_empty());
    }

    #[test]
    fn fr5_the_mouse_hovers_until_it_presses_and_after_it_leaves_it_exits() {
        let mut mapper = PointerMapper::new(7);
        let hover = mapper.handle(
            WindowInput::MouseMove {
                position: (40.0, 40.0),
            },
            geometry(),
        );
        assert_eq!(actions(&hover), vec![(PointerAction::HoverMove, 0)]);
        assert_eq!(hover.events[0].pressure, 0.0);
        let down = mapper.handle(button(MouseButton::Left, true, (40.0, 40.0)), geometry());
        assert_eq!(
            actions(&down),
            vec![
                (PointerAction::HoverExit, 0),
                (PointerAction::Down, buttons::PRIMARY)
            ]
        );
        mapper.handle(button(MouseButton::Left, false, (40.0, 40.0)), geometry());
        mapper.handle(
            WindowInput::MouseMove {
                position: (60.0, 40.0),
            },
            geometry(),
        );
        let leave = mapper.handle(WindowInput::MouseLeave, geometry());
        assert_eq!(actions(&leave), vec![(PointerAction::HoverExit, 0)]);
        assert_eq!((leave.events[0].x, leave.events[0].y), (30.0, 20.0));
        assert!(mapper
            .handle(WindowInput::MouseLeave, geometry())
            .events
            .is_empty());
    }

    #[test]
    fn fr5_the_wheel_scrolls_where_the_mouse_is() {
        let mut mapper = PointerMapper::new(7);
        let scroll = mapper.handle(
            WindowInput::Wheel {
                position: (200.0, 100.0),
                notches: (0.0, -2.0),
            },
            geometry(),
        );
        assert_eq!(actions(&scroll), vec![(PointerAction::Scroll, 0)]);
        let event = scroll.events[0];
        assert_eq!((event.x, event.y), (100.0, 50.0));
        assert_eq!((event.scroll_x, event.scroll_y), (0.0, -2.0));
    }

    #[test]
    fn fr5_losing_the_mouse_mid_drag_cancels_the_gesture() {
        let mut mapper = PointerMapper::new(7);
        mapper.handle(button(MouseButton::Left, true, (10.0, 10.0)), geometry());
        let lost = mapper.handle(WindowInput::CaptureLost, geometry());
        assert_eq!(actions(&lost), vec![(PointerAction::Cancel, 0)]);
        assert_eq!(mapper.buttons(), 0);
        // Releasing the mouse after its own up ends nothing more.
        assert!(mapper
            .handle(WindowInput::CaptureLost, geometry())
            .events
            .is_empty());
        assert!(mapper
            .handle(button(MouseButton::Left, false, (10.0, 10.0)), geometry())
            .events
            .is_empty());
    }

    fn contact(
        tool: PointerTool,
        pointer_id: u32,
        phase: ContactPhase,
        at: (f64, f64),
    ) -> WindowInput {
        WindowInput::Contact {
            tool,
            pointer_id,
            phase,
            position: at,
            pressure: None,
            buttons: 0,
        }
    }

    #[test]
    fn fr5_each_finger_of_a_touch_keeps_its_own_pointer() {
        let mut mapper = PointerMapper::new(3);
        let first = mapper.handle(
            contact(PointerTool::Touch, 11, ContactPhase::Down, (100.0, 100.0)),
            geometry(),
        );
        let second = mapper.handle(
            contact(PointerTool::Touch, 12, ContactPhase::Down, (300.0, 100.0)),
            geometry(),
        );
        let lift = mapper.handle(
            contact(PointerTool::Touch, 11, ContactPhase::Up, (100.0, 120.0)),
            geometry(),
        );
        let events: Vec<(PointerAction, u32, f32, f32)> = [first, second, lift]
            .iter()
            .map(|mapped| {
                let event = mapped.events[0];
                assert_eq!(event.tool, PointerTool::Touch);
                assert_eq!(mapped.capture, None);
                (event.action, event.pointer_id, event.x, event.y)
            })
            .collect();
        assert_eq!(
            events,
            vec![
                (PointerAction::Down, 11, 50.0, 50.0),
                (PointerAction::Down, 12, 150.0, 50.0),
                (PointerAction::Up, 11, 50.0, 60.0),
            ]
        );
    }

    #[test]
    fn fr5_a_pen_brings_its_pressure_buttons_and_hover() {
        let mut mapper = PointerMapper::new(3);
        let hover = mapper.handle(
            WindowInput::Contact {
                tool: PointerTool::Pen,
                pointer_id: 2,
                phase: ContactPhase::Hover,
                position: (10.0, 10.0),
                pressure: None,
                buttons: buttons::STYLUS_PRIMARY,
            },
            geometry(),
        );
        let event = hover.events[0];
        assert_eq!(event.action, PointerAction::HoverMove);
        assert_eq!(event.buttons, buttons::STYLUS_PRIMARY);
        assert_eq!(event.pressure, 0.0);
        let press = mapper.handle(
            WindowInput::Contact {
                tool: PointerTool::Eraser,
                pointer_id: 2,
                phase: ContactPhase::Down,
                position: (10.0, 10.0),
                pressure: Some(0.4),
                buttons: 0,
            },
            geometry(),
        );
        let event = press.events[0];
        assert_eq!(
            (event.action, event.tool),
            (PointerAction::Down, PointerTool::Eraser)
        );
        assert_eq!(event.pressure, 0.4);
    }

    #[test]
    fn fr5_events_cross_the_local_api_unchanged() {
        let mut event = PointerEvent::new(9, PointerAction::Scroll, PointerTool::Mouse, (1.5, 2.0));
        event.scroll_y = -1.0;
        event.buttons = buttons::SECONDARY;
        assert_eq!(PointerEvent::from_wire(&event.to_wire()), Ok(event));
        let control = event.to_control();
        assert_eq!(control.action, control::PointerAction::Scroll as i32);
        assert_eq!(control.scroll_y, -1.0);
        assert_eq!(control.buttons, buttons::SECONDARY);
    }

    #[test]
    fn nfr5_a_pointer_event_that_makes_no_sense_is_refused() {
        let good = PointerEvent::new(1, PointerAction::Down, PointerTool::Touch, (1.0, 1.0));
        let mut unknown = good.to_wire();
        unknown.action = 99;
        assert!(PointerEvent::from_wire(&unknown).is_err());
        let mut no_tool = good.to_wire();
        no_tool.tool = 0;
        assert!(PointerEvent::from_wire(&no_tool).is_err());
        let mut nan = good.to_wire();
        nan.x = f32::NAN;
        assert!(PointerEvent::from_wire(&nan).is_err());
    }
}

//! Pointer input in a task window: the window's mouse, touch and pen messages, as the
//! platform-neutral input that `PointerMapper` turns into the guest's pointer events.
//!
//! This is the only part of pointer input that knows Win32. It decodes the messages, holds
//! the mouse while a button is down, and hands each event to a channel; the rules (button
//! state, hover, coordinates) are the mapper's. It keeps no state of its own beyond what a
//! window needs, so it moves with the window into whichever process owns it.
//!
//! The mouse arrives as the classic mouse messages. Touch and pen arrive as pointer
//! messages, which this module handles itself, so Windows does not also turn them into
//! mouse messages; the few mouse messages it still makes from them are dropped.
//!
//! The window is never registered for `WM_TOUCH` (`RegisterTouchWindow`). A window that is
//! gets a finger as `WM_TOUCH` in place of pointer messages, and when it leaves `WM_TOUCH` to
//! Windows, the finger becomes mouse messages marked as made from touch, which this module
//! drops: a finger then does nothing in the app.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;

use crate::compositor::log::{input_trace, input_trace_on};
use dsa_host_core::drag::{DragOut, LeaveWatch};
use dsa_host_core::input::{
    buttons, Capture, ContactPhase, InputEvent, MouseButton, PointerAction, PointerMapper,
    PointerTool, WindowGeometry, WindowInput,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Input::Pointer::{
    GetPointerPenInfo, GetPointerTouchInfo, GetPointerType, POINTER_FLAG_CANCELED,
    POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_INFO, POINTER_PEN_INFO,
    POINTER_TOUCH_INFO,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetMessageExtraInfo, RemovePropW, SetPropW, PEN_FLAG_BARREL, PEN_FLAG_ERASER,
    PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, POINTER_INPUT_TYPE,
    PT_PEN, PT_TOUCH, TOUCH_MASK_PRESSURE, WHEEL_DELTA, WM_CAPTURECHANGED, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERLEAVE, WM_POINTERUP, WM_POINTERUPDATE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

use super::drag_source::DragStarter;

/// The mouse left the window (`TrackMouseEvent`). windows-sys keeps it with the common
/// controls, which this crate does not otherwise need.
const WM_MOUSELEAVE: u32 = 0x02a3;

/// The window property the tablet service reads, and the behaviours it turns off in the
/// window: press and hold, its feedback, and flicks. Android has its own long press, and
/// Windows must not hold a contact back to see whether it is one.
const TABLET_SERVICE_PROPERTY: &str = "MicrosoftTabletPenServiceProperty";
const TABLET_DISABLE_PRESSANDHOLD: usize = 0x0000_0001;
const TABLET_DISABLE_PENTAPFEEDBACK: usize = 0x0000_0008;
const TABLET_DISABLE_PENBARRELFEEDBACK: usize = 0x0000_0010;
const TABLET_DISABLE_FLICKS: usize = 0x0001_0000;

/// What [`deliver_contacts_at_once`] sets the tablet service's property to.
pub const TABLET_SERVICE_FLAGS: usize = TABLET_DISABLE_PRESSANDHOLD
    | TABLET_DISABLE_PENTAPFEEDBACK
    | TABLET_DISABLE_PENBARRELFEEDBACK
    | TABLET_DISABLE_FLICKS;

/// The tablet service's property name, NUL-terminated.
pub fn tablet_service_property() -> Vec<u16> {
    TABLET_SERVICE_PROPERTY
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// Makes Windows hand the window's touch and pen contacts over without delay: it does not
/// hold a contact back to see whether it becomes a press and hold or a flick.
///
/// It leaves the window unregistered for `WM_TOUCH`, so fingers keep arriving as pointer
/// messages (see the module's notes).
///
/// # Safety
///
/// `hwnd` is a live window of this thread.
pub unsafe fn deliver_contacts_at_once(hwnd: HWND) {
    SetPropW(
        hwnd,
        tablet_service_property().as_ptr(),
        TABLET_SERVICE_FLAGS as *mut std::ffi::c_void,
    );
}

/// Undoes what stays behind of [`deliver_contacts_at_once`] when the window goes.
///
/// # Safety
///
/// `hwnd` is a live window of this thread.
pub unsafe fn forget_contacts(hwnd: HWND) {
    RemovePropW(hwnd, tablet_service_property().as_ptr());
}

/// How long ago, in milliseconds, Windows stamped a pointer message with `stamp`. 0 when the
/// device gives no stamp.
fn pointer_age_millis(stamp: u64) -> f64 {
    if stamp == 0 {
        return 0.0;
    }
    let (mut now, mut frequency) = (0i64, 0i64);
    // SAFETY: both write one local integer.
    unsafe {
        QueryPerformanceCounter(&mut now);
        QueryPerformanceFrequency(&mut frequency);
    }
    if frequency <= 0 {
        return 0.0;
    }
    (now as f64 - stamp as f64) * 1000.0 / frequency as f64
}

/// The low and high 16 bits of a message parameter, as Windows packs them.
fn low_word(value: usize) -> u16 {
    (value & 0xffff) as u16
}

fn high_word(value: usize) -> u16 {
    ((value >> 16) & 0xffff) as u16
}

/// The client point a mouse message carries in its `lparam`. The coordinates are signed:
/// while the window holds the mouse, a drag past its left or top edge is negative.
pub fn message_point(lparam: LPARAM) -> (f64, f64) {
    let x = low_word(lparam as usize) as i16;
    let y = high_word(lparam as usize) as i16;
    (f64::from(x), f64::from(y))
}

/// The mouse button a button message is about, and whether it went down.
pub fn mouse_button(message: u32, wparam: WPARAM) -> Option<(MouseButton, bool)> {
    Some(match message {
        WM_LBUTTONDOWN => (MouseButton::Left, true),
        WM_LBUTTONUP => (MouseButton::Left, false),
        WM_RBUTTONDOWN => (MouseButton::Right, true),
        WM_RBUTTONUP => (MouseButton::Right, false),
        WM_MBUTTONDOWN => (MouseButton::Middle, true),
        WM_MBUTTONUP => (MouseButton::Middle, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let button = match high_word(wparam) {
                XBUTTON1 => MouseButton::Back,
                XBUTTON2 => MouseButton::Forward,
                _ => return None,
            };
            (button, message == WM_XBUTTONDOWN)
        }
        _ => return None,
    })
}

/// How far a wheel message turned the wheel, in notches, as (horizontal, vertical).
/// Positive is right for the tilt wheel and away from the user for the wheel, which the
/// guest reads as scrolling right and up.
pub fn wheel_notches(message: u32, wparam: WPARAM) -> Option<(f32, f32)> {
    let delta = f32::from(high_word(wparam) as i16) / WHEEL_DELTA as f32;
    match message {
        WM_MOUSEWHEEL => Some((0.0, delta)),
        WM_MOUSEHWHEEL => Some((delta, 0.0)),
        _ => None,
    }
}

/// Whether a mouse message was made by Windows from a touch or pen contact rather than by
/// a mouse (its extra information carries Windows' signature for that).
pub fn made_from_touch_or_pen(extra_info: LPARAM) -> bool {
    const SIGNATURE_MASK: usize = 0xffff_ff00;
    const MI_WP_SIGNATURE: usize = 0xff51_5700;
    (extra_info as usize) & SIGNATURE_MASK == MI_WP_SIGNATURE
}

/// Where a contact is, from a pointer message and the pointer's flags. A finger that is not
/// touching is not there at all, so only a pen hovers.
pub fn contact_phase(tool: PointerTool, message: u32, flags: u32) -> Option<ContactPhase> {
    let phase = if flags & POINTER_FLAG_CANCELED != 0 {
        ContactPhase::Cancel
    } else {
        match message {
            WM_POINTERDOWN => ContactPhase::Down,
            WM_POINTERUP => ContactPhase::Up,
            WM_POINTERUPDATE if flags & POINTER_FLAG_INCONTACT != 0 => ContactPhase::Move,
            WM_POINTERUPDATE if flags & POINTER_FLAG_INRANGE != 0 => ContactPhase::Hover,
            WM_POINTERUPDATE | WM_POINTERLEAVE => ContactPhase::HoverExit,
            WM_POINTERCAPTURECHANGED => ContactPhase::Cancel,
            _ => return None,
        }
    };
    match (tool, phase) {
        (PointerTool::Touch, ContactPhase::Hover | ContactPhase::HoverExit) => None,
        _ => Some(phase),
    }
}

/// A pen's tool (its tip, or its eraser end) and its barrel buttons, from its flags.
pub fn pen_tool_and_buttons(pen_flags: u32) -> (PointerTool, u32) {
    let tool = if pen_flags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0 {
        PointerTool::Eraser
    } else {
        PointerTool::Pen
    };
    let buttons = if pen_flags & PEN_FLAG_BARREL != 0 {
        buttons::STYLUS_PRIMARY
    } else {
        0
    };
    (tool, buttons)
}

/// A pen's or a finger's pressure, which Windows reports from 0 to 1024, from 0 to 1.
pub fn pressure(raw: u32) -> f32 {
    (raw.min(1024) as f32) / 1024.0
}

/// A finger's pointer message as the window's input. `client_origin` is where the window's
/// client area begins on the screen, in the same pixels as the pointer's position. `None`
/// when the message says nothing about a contact (a finger that is not touching).
pub fn touch_input(
    message: u32,
    touch: &POINTER_TOUCH_INFO,
    client_origin: (i32, i32),
) -> Option<WindowInput> {
    let info = &touch.pointerInfo;
    let phase = contact_phase(PointerTool::Touch, message, info.pointerFlags)?;
    let measured = (touch.touchMask & TOUCH_MASK_PRESSURE != 0).then(|| pressure(touch.pressure));
    Some(WindowInput::Contact {
        tool: PointerTool::Touch,
        pointer_id: info.pointerId,
        phase,
        position: in_client(info, client_origin),
        pressure: measured,
        buttons: 0,
        tilt: (0.0, 0.0),
    })
}

/// A pen's pointer message as the window's input, as [`touch_input`] reads a finger's. A pen
/// also hovers, and a hovering pen has no pressure.
pub fn pen_input(
    message: u32,
    pen: &POINTER_PEN_INFO,
    client_origin: (i32, i32),
) -> Option<WindowInput> {
    let info = &pen.pointerInfo;
    let (tool, barrel) = pen_tool_and_buttons(pen.penFlags);
    let phase = contact_phase(tool, message, info.pointerFlags)?;
    let measured = (pen.penMask & PEN_MASK_PRESSURE != 0).then(|| pressure(pen.pressure));
    Some(WindowInput::Contact {
        tool,
        pointer_id: info.pointerId,
        phase,
        position: in_client(info, client_origin),
        pressure: if phase == ContactPhase::Hover {
            Some(0.0)
        } else {
            measured
        },
        buttons: barrel,
        tilt: pen_tilt(pen),
    })
}

/// A pen's tilt in degrees, (toward the right, toward the user), as Windows measures it;
/// 0 on an axis the pen does not measure.
pub fn pen_tilt(pen: &POINTER_PEN_INFO) -> (f32, f32) {
    let axis = |mask: u32, degrees: i32| {
        if pen.penMask & mask != 0 {
            degrees.clamp(-90, 90) as f32
        } else {
            0.0
        }
    };
    (
        axis(PEN_MASK_TILT_X, pen.tiltX),
        axis(PEN_MASK_TILT_Y, pen.tiltY),
    )
}

/// A pointer's position in the client area that begins at `client_origin` on the screen.
fn in_client(info: &POINTER_INFO, (left, top): (i32, i32)) -> (f64, f64) {
    let point = info.ptPixelLocation;
    (f64::from(point.x - left), f64::from(point.y - top))
}

/// One task window's pointer input.
pub struct PointerWindow {
    task_id: i32,
    mapper: RefCell<PointerMapper>,
    /// The size of the task the window shows, in guest pixels.
    task_size: Cell<(u32, u32)>,
    /// Window pixels per task pixels, as the window shows the task's frames: 1:1 unless the
    /// task keeps its density on a monitor of another scale.
    scale: Cell<(u32, u32)>,
    /// Whether Windows will say when the mouse leaves the window.
    tracking_leave: Cell<bool>,
    events: Sender<InputEvent>,
    /// When a press of the left button leaves the window, which may carry an app's drag out.
    leave: RefCell<LeaveWatch>,
    /// Asks whether an app drags out of the window; `None` starts no drags on the host.
    drags: Option<DragStarter>,
}

impl PointerWindow {
    pub fn new(
        task_id: i32,
        task_size: (u32, u32),
        events: Sender<InputEvent>,
        drags: Option<DragStarter>,
    ) -> Self {
        PointerWindow {
            task_id,
            mapper: RefCell::new(PointerMapper::new(task_id)),
            task_size: Cell::new(task_size),
            scale: Cell::new((1, 1)),
            tracking_leave: Cell::new(false),
            events,
            leave: RefCell::new(LeaveWatch::default()),
            drags,
        }
    }

    /// Asks whether an app drags out of the window, when the press has just left it.
    unsafe fn watch_leave(&self, hwnd: HWND, position: (f64, f64)) {
        let Some(drags) = &self.drags else {
            return;
        };
        let client = client_size(hwnd);
        let Some(inside) = self.leave.borrow_mut().moved(position, client) else {
            return;
        };
        let (x, y) = self.geometry(client).to_guest(inside);
        drags(
            hwnd as isize,
            DragOut {
                task_id: self.task_id,
                x,
                y,
                tool: dsa_protocol::control::PointerTool::Mouse as i32,
                pointer_id: 0,
            },
        );
    }

    /// The guest resized the task the window shows.
    pub fn task_resized(&self, task_size: (u32, u32)) {
        self.task_size.set(task_size);
    }

    /// The window shows the task's frames at `scale` (window pixels per task pixels) from now
    /// on, so a point in the window is that point over the scale in the task.
    pub fn set_scale(&self, scale: (u32, u32)) {
        self.scale.set(scale);
    }

    /// What the window's points are converted with, for a client area of `client`.
    fn geometry(&self, client: (u32, u32)) -> WindowGeometry {
        WindowGeometry {
            client,
            task: self.task_size.get(),
            scale: self.scale.get(),
        }
    }

    /// Handles a mouse, touch or pen message. `None` for any other message, or one that
    /// Windows must also see.
    ///
    /// # Safety
    ///
    /// `hwnd` is the window the message is for, on this thread.
    pub unsafe fn handle(
        &self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_MOUSEMOVE => {
                if made_from_touch_or_pen(GetMessageExtraInfo()) {
                    return Some(0);
                }
                if !self.tracking_leave.get() {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    if TrackMouseEvent(&mut track) != 0 {
                        self.tracking_leave.set(true);
                    }
                }
                let position = message_point(lparam);
                self.deliver(hwnd, WindowInput::MouseMove { position });
                self.watch_leave(hwnd, position);
                Some(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
            | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP => {
                // An X button message that is handled returns TRUE; the others return 0.
                let handled = if matches!(message, WM_XBUTTONDOWN | WM_XBUTTONUP) {
                    1
                } else {
                    0
                };
                if made_from_touch_or_pen(GetMessageExtraInfo()) {
                    return Some(handled);
                }
                let (button, down) = mouse_button(message, wparam)?;
                if button == MouseButton::Left {
                    self.leave.borrow_mut().pressed(down);
                }
                input_trace("app.pointer", self.task_id, &format!("mouse_down={down}"));
                self.deliver(
                    hwnd,
                    WindowInput::MouseButton {
                        button,
                        down,
                        position: message_point(lparam),
                    },
                );
                Some(handled)
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let notches = wheel_notches(message, wparam)?;
                // A wheel message carries the point on the screen, not in the window.
                let (x, y) = message_point(lparam);
                let mut point = POINT {
                    x: x as i32,
                    y: y as i32,
                };
                ScreenToClient(hwnd, &mut point);
                self.deliver(
                    hwnd,
                    WindowInput::Wheel {
                        position: (f64::from(point.x), f64::from(point.y)),
                        notches,
                    },
                );
                Some(0)
            }
            WM_MOUSELEAVE => {
                self.tracking_leave.set(false);
                self.deliver(hwnd, WindowInput::MouseLeave);
                Some(0)
            }
            WM_CAPTURECHANGED => {
                if lparam as HWND != hwnd {
                    self.leave.borrow_mut().cancel();
                    self.deliver(hwnd, WindowInput::CaptureLost);
                }
                Some(0)
            }
            WM_POINTERDOWN
            | WM_POINTERUPDATE
            | WM_POINTERUP
            | WM_POINTERLEAVE
            | WM_POINTERCAPTURECHANGED => self.pointer_message(hwnd, message, wparam),
            _ => None,
        }
    }

    /// A touch or pen message. A mouse that comes as pointer messages is left to Windows,
    /// which then sends the mouse messages above.
    unsafe fn pointer_message(&self, hwnd: HWND, message: u32, wparam: WPARAM) -> Option<LRESULT> {
        let pointer_id = u32::from(low_word(wparam));
        let mut kind: POINTER_INPUT_TYPE = 0;
        if GetPointerType(pointer_id, &mut kind) == 0 {
            return None;
        }
        let origin = client_origin(hwnd);
        let (input, stamp) = match kind {
            PT_TOUCH => {
                let mut touch: POINTER_TOUCH_INFO = std::mem::zeroed();
                if GetPointerTouchInfo(pointer_id, &mut touch) == 0 {
                    return None;
                }
                (
                    touch_input(message, &touch, origin),
                    touch.pointerInfo.PerformanceCount,
                )
            }
            PT_PEN => {
                let mut pen: POINTER_PEN_INFO = std::mem::zeroed();
                if GetPointerPenInfo(pointer_id, &mut pen) == 0 {
                    return None;
                }
                (
                    pen_input(message, &pen, origin),
                    pen.pointerInfo.PerformanceCount,
                )
            }
            _ => return None,
        };
        // Handled either way, so Windows makes no mouse messages from it.
        let Some(input) = input else {
            return Some(0);
        };
        if let WindowInput::Contact { phase, tool, .. } = input {
            if input_trace_on() && matches!(phase, ContactPhase::Down | ContactPhase::Up) {
                input_trace(
                    "app.pointer",
                    self.task_id,
                    &format!(
                        "phase={phase:?} tool={tool:?} pointer={pointer_id} age={:.1}",
                        pointer_age_millis(stamp)
                    ),
                );
            }
        }
        self.deliver(hwnd, input);
        Some(0)
    }

    unsafe fn deliver(&self, hwnd: HWND, input: WindowInput) {
        let geometry = self.geometry(client_size(hwnd));
        let mapped = match self.mapper.try_borrow_mut() {
            Ok(mut mapper) => mapper.handle(input, geometry),
            // Reached from inside its own handling (a capture change that SetCapture or
            // ReleaseCapture sends at once); the mapper has already acted on it.
            Err(_) => return,
        };
        match mapped.capture {
            Some(Capture::Take) => {
                SetCapture(hwnd);
            }
            Some(Capture::Release) => {
                ReleaseCapture();
            }
            None => {}
        }
        for event in mapped.events {
            if event.action == PointerAction::Down && input_trace_on() {
                // The window watches the pixels around a traced press for its first visible
                // change.
                crate::compositor::present::pressed(self.task_id, (event.x, event.y));
            }
            // The sender ends only when the compositor stops; input then has nowhere to go.
            let _ = self.events.send(InputEvent::Pointer(event));
        }
    }
}

/// Where the window's client area begins on the screen.
unsafe fn client_origin(hwnd: HWND) -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut point);
    (point.x, point.y)
}

/// The size of the window's client area, in its pixels.
unsafe fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    GetClientRect(hwnd, &mut rect);
    (
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
    )
}

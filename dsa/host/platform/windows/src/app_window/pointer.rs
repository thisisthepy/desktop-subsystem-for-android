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

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;

use dsa_host_core::input::{
    buttons, Capture, ContactPhase, MouseButton, PointerEvent, PointerMapper, PointerTool,
    WindowGeometry, WindowInput,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Input::Pointer::{
    GetPointerPenInfo, GetPointerTouchInfo, GetPointerType, POINTER_FLAG_CANCELED,
    POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_INFO, POINTER_PEN_INFO,
    POINTER_TOUCH_INFO,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetMessageExtraInfo, PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED,
    PEN_MASK_PRESSURE, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH, TOUCH_MASK_PRESSURE, WHEEL_DELTA,
    WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERLEAVE,
    WM_POINTERUP, WM_POINTERUPDATE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    XBUTTON1, XBUTTON2,
};

/// The mouse left the window (`TrackMouseEvent`). windows-sys keeps it with the common
/// controls, which this crate does not otherwise need.
const WM_MOUSELEAVE: u32 = 0x02a3;

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

/// One task window's pointer input.
pub struct PointerWindow {
    mapper: RefCell<PointerMapper>,
    /// The size of the task the window shows, in guest pixels.
    task_size: Cell<(u32, u32)>,
    /// Whether Windows will say when the mouse leaves the window.
    tracking_leave: Cell<bool>,
    events: Sender<PointerEvent>,
}

impl PointerWindow {
    pub fn new(task_id: i32, task_size: (u32, u32), events: Sender<PointerEvent>) -> Self {
        PointerWindow {
            mapper: RefCell::new(PointerMapper::new(task_id)),
            task_size: Cell::new(task_size),
            tracking_leave: Cell::new(false),
            events,
        }
    }

    /// The guest resized the task the window shows.
    pub fn task_resized(&self, task_size: (u32, u32)) {
        self.task_size.set(task_size);
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
                self.deliver(
                    hwnd,
                    WindowInput::MouseMove {
                        position: message_point(lparam),
                    },
                );
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
        let (tool, info, pressure_value, barrel) = match kind {
            PT_TOUCH => {
                let mut touch: POINTER_TOUCH_INFO = std::mem::zeroed();
                if GetPointerTouchInfo(pointer_id, &mut touch) == 0 {
                    return None;
                }
                let measured =
                    (touch.touchMask & TOUCH_MASK_PRESSURE != 0).then(|| pressure(touch.pressure));
                (PointerTool::Touch, touch.pointerInfo, measured, 0)
            }
            PT_PEN => {
                let mut pen: POINTER_PEN_INFO = std::mem::zeroed();
                if GetPointerPenInfo(pointer_id, &mut pen) == 0 {
                    return None;
                }
                let (tool, barrel) = pen_tool_and_buttons(pen.penFlags);
                let measured =
                    (pen.penMask & PEN_MASK_PRESSURE != 0).then(|| pressure(pen.pressure));
                (tool, pen.pointerInfo, measured, barrel)
            }
            _ => return None,
        };
        let phase = match contact_phase(tool, message, info.pointerFlags) {
            Some(phase) => phase,
            // Handled, so Windows makes no mouse messages from it.
            None => return Some(0),
        };
        let position = client_point(hwnd, &info);
        self.deliver(
            hwnd,
            WindowInput::Contact {
                tool,
                pointer_id,
                phase,
                position,
                pressure: if phase == ContactPhase::Hover {
                    Some(0.0)
                } else {
                    pressure_value
                },
                buttons: barrel,
            },
        );
        Some(0)
    }

    unsafe fn deliver(&self, hwnd: HWND, input: WindowInput) {
        let geometry = WindowGeometry {
            client: client_size(hwnd),
            task: self.task_size.get(),
        };
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
            // The sender ends only when the compositor stops; input then has nowhere to go.
            let _ = self.events.send(event);
        }
    }
}

/// A pointer's position in the window's client area.
unsafe fn client_point(hwnd: HWND, info: &POINTER_INFO) -> (f64, f64) {
    let mut point = info.ptPixelLocation;
    ScreenToClient(hwnd, &mut point);
    (f64::from(point.x), f64::from(point.y))
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

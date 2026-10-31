//! How a task window reads its mouse, touch and pen messages, before the platform-neutral
//! rules turn them into the guest's pointer events.
#![cfg(windows)]

use dsa_host_core::input::{buttons, ContactPhase, MouseButton, PointerTool};
use dsa_host_windows::app_window::pointer::{
    contact_phase, made_from_touch_or_pen, message_point, mouse_button, pen_tool_and_buttons,
    pressure, wheel_notches,
};
use windows_sys::Win32::UI::Input::Pointer::{
    POINTER_FLAG_CANCELED, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_POINTERCAPTURECHANGED,
    WM_POINTERDOWN, WM_POINTERLEAVE, WM_POINTERUP, WM_POINTERUPDATE, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP,
};

fn lparam(x: i16, y: i16) -> isize {
    ((y as u16 as usize) << 16 | x as u16 as usize) as isize
}

#[test]
fn fr5_a_mouse_message_carries_its_point_in_client_pixels() {
    assert_eq!(message_point(lparam(320, 240)), (320.0, 240.0));
    // A drag held past the window's left and top edges.
    assert_eq!(message_point(lparam(-12, -3)), (-12.0, -3.0));
}

#[test]
fn fr5_each_mouse_button_is_read_with_whether_it_went_down() {
    assert_eq!(
        mouse_button(WM_LBUTTONDOWN, 0),
        Some((MouseButton::Left, true))
    );
    assert_eq!(
        mouse_button(WM_LBUTTONUP, 0),
        Some((MouseButton::Left, false))
    );
    assert_eq!(
        mouse_button(WM_RBUTTONDOWN, 0),
        Some((MouseButton::Right, true))
    );
    assert_eq!(
        mouse_button(WM_RBUTTONUP, 0),
        Some((MouseButton::Right, false))
    );
    assert_eq!(
        mouse_button(WM_MBUTTONDOWN, 0),
        Some((MouseButton::Middle, true))
    );
    assert_eq!(
        mouse_button(WM_XBUTTONDOWN, 1 << 16),
        Some((MouseButton::Back, true))
    );
    assert_eq!(
        mouse_button(WM_XBUTTONUP, 2 << 16),
        Some((MouseButton::Forward, false))
    );
    assert_eq!(mouse_button(WM_XBUTTONDOWN, 3 << 16), None);
    assert_eq!(mouse_button(WM_MOUSEMOVE, 0), None);
}

#[test]
fn fr5_the_wheel_turns_in_notches() {
    let delta = |value: i16| (value as u16 as usize) << 16;
    assert_eq!(wheel_notches(WM_MOUSEWHEEL, delta(120)), Some((0.0, 1.0)));
    assert_eq!(wheel_notches(WM_MOUSEWHEEL, delta(-240)), Some((0.0, -2.0)));
    // A precise touchpad turns it by less than a notch.
    assert_eq!(wheel_notches(WM_MOUSEWHEEL, delta(30)), Some((0.0, 0.25)));
    assert_eq!(wheel_notches(WM_MOUSEHWHEEL, delta(120)), Some((1.0, 0.0)));
    assert_eq!(wheel_notches(WM_MOUSEMOVE, delta(120)), None);
}

#[test]
fn fr5_mouse_messages_windows_made_from_touch_or_pen_are_told_apart() {
    assert!(made_from_touch_or_pen(0xff51_5701u32 as isize));
    assert!(made_from_touch_or_pen(0xff51_5780u32 as isize));
    assert!(!made_from_touch_or_pen(0));
    assert!(!made_from_touch_or_pen(0x1234_5678));
}

#[test]
fn fr5_a_contact_is_down_moving_up_or_cancelled() {
    let touching = POINTER_FLAG_INCONTACT | POINTER_FLAG_INRANGE;
    for tool in [PointerTool::Touch, PointerTool::Pen] {
        assert_eq!(
            contact_phase(tool, WM_POINTERDOWN, touching),
            Some(ContactPhase::Down)
        );
        assert_eq!(
            contact_phase(tool, WM_POINTERUPDATE, touching),
            Some(ContactPhase::Move)
        );
        assert_eq!(
            contact_phase(tool, WM_POINTERUP, POINTER_FLAG_INRANGE),
            Some(ContactPhase::Up)
        );
        assert_eq!(
            contact_phase(tool, WM_POINTERUPDATE, touching | POINTER_FLAG_CANCELED),
            Some(ContactPhase::Cancel)
        );
        assert_eq!(
            contact_phase(tool, WM_POINTERCAPTURECHANGED, touching),
            Some(ContactPhase::Cancel)
        );
    }
}

#[test]
fn fr5_only_a_pen_hovers() {
    assert_eq!(
        contact_phase(PointerTool::Pen, WM_POINTERUPDATE, POINTER_FLAG_INRANGE),
        Some(ContactPhase::Hover)
    );
    assert_eq!(
        contact_phase(PointerTool::Pen, WM_POINTERLEAVE, 0),
        Some(ContactPhase::HoverExit)
    );
    assert_eq!(
        contact_phase(PointerTool::Touch, WM_POINTERUPDATE, POINTER_FLAG_INRANGE),
        None
    );
    assert_eq!(contact_phase(PointerTool::Touch, WM_POINTERLEAVE, 0), None);
}

#[test]
fn fr5_a_pens_eraser_and_barrel_button_are_read_from_its_flags() {
    assert_eq!(pen_tool_and_buttons(0), (PointerTool::Pen, 0));
    assert_eq!(
        pen_tool_and_buttons(PEN_FLAG_BARREL),
        (PointerTool::Pen, buttons::STYLUS_PRIMARY)
    );
    assert_eq!(
        pen_tool_and_buttons(PEN_FLAG_ERASER),
        (PointerTool::Eraser, 0)
    );
    assert_eq!(
        pen_tool_and_buttons(PEN_FLAG_INVERTED),
        (PointerTool::Eraser, 0)
    );
}

#[test]
fn fr5_pressure_runs_from_0_to_1() {
    assert_eq!(pressure(0), 0.0);
    assert_eq!(pressure(512), 0.5);
    assert_eq!(pressure(1024), 1.0);
    assert_eq!(pressure(5000), 1.0);
}

#[test]
fn fr5_windows_hands_a_windows_contacts_over_without_holding_them_back() {
    use dsa_host_windows::app_window::pointer::{
        deliver_contacts_at_once, tablet_service_property, TABLET_SERVICE_FLAGS,
    };
    use windows_sys::Win32::UI::Input::Touch::{IsTouchWindow, TWF_WANTPALM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetPropW, WS_POPUP,
    };

    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: a hidden window of this thread, destroyed below.
    unsafe {
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        assert!(!hwnd.is_null());
        deliver_contacts_at_once(hwnd);
        let mut flags = 0;
        assert_ne!(IsTouchWindow(hwnd, &mut flags), 0);
        assert_ne!(flags & TWF_WANTPALM, 0);
        assert_eq!(
            GetPropW(hwnd, tablet_service_property().as_ptr()) as usize,
            TABLET_SERVICE_FLAGS
        );
        DestroyWindow(hwnd);
    }
}

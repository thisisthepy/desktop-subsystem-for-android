//! How a task window reads its mouse, touch and pen messages, before the platform-neutral
//! rules turn them into the guest's pointer events.
#![cfg(windows)]

use dsa_host_core::input::{
    buttons, ContactPhase, MouseButton, PointerMapper, PointerTool, WindowGeometry, WindowInput,
};
use dsa_host_windows::app_window::pointer::{
    contact_phase, made_from_touch_or_pen, message_point, mouse_button, pen_input, pen_tilt,
    pen_tool_and_buttons, pressure, touch_input, wheel_notches,
};
use dsa_protocol::control;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::Pointer::{
    POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
    POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_PEN_INFO, POINTER_TOUCH_INFO,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
    PEN_MASK_TILT_Y, PT_PEN, PT_TOUCH, TOUCH_MASK_PRESSURE, WM_LBUTTONDOWN, WM_LBUTTONUP,
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

/// A hidden window of this thread, for the tests that check how a window is set up.
fn hidden_window() -> windows_sys::Win32::Foundation::HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, WS_POPUP};
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: the class name outlives the call; the caller destroys the window.
    let hwnd = unsafe {
        CreateWindowExW(
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
        )
    };
    assert!(!hwnd.is_null());
    hwnd
}

#[test]
fn fr5_windows_hands_a_windows_contacts_over_without_holding_them_back() {
    use dsa_host_windows::app_window::pointer::{
        deliver_contacts_at_once, tablet_service_property, TABLET_SERVICE_FLAGS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyWindow, GetPropW};

    let hwnd = hidden_window();
    // SAFETY: a live window of this thread, destroyed below.
    unsafe {
        deliver_contacts_at_once(hwnd);
        assert_eq!(
            GetPropW(hwnd, tablet_service_property().as_ptr()) as usize,
            TABLET_SERVICE_FLAGS
        );
        DestroyWindow(hwnd);
    }
}

/// A finger on a touchscreen did nothing in an app: the window was registered for
/// `WM_TOUCH`, so Windows sent its fingers as `WM_TOUCH` rather than as pointer messages,
/// and the mouse messages it then made from them were dropped as made from touch.
#[test]
fn fr5_a_task_window_takes_its_fingers_as_pointer_messages_not_wm_touch() {
    use dsa_host_windows::app_window::pointer::deliver_contacts_at_once;
    use windows_sys::Win32::UI::Input::Touch::IsTouchWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;

    let hwnd = hidden_window();
    // SAFETY: a live window of this thread, destroyed below.
    unsafe {
        deliver_contacts_at_once(hwnd);
        let mut flags = 0;
        assert_eq!(
            IsTouchWindow(hwnd, &mut flags),
            0,
            "a window registered for WM_TOUCH gets no pointer messages for its fingers"
        );
        DestroyWindow(hwnd);
    }
}

fn touch_info(pointer_id: u32, flags: u32, screen: (i32, i32)) -> POINTER_TOUCH_INFO {
    // SAFETY: plain data, for which all zero is a valid value.
    let mut touch: POINTER_TOUCH_INFO = unsafe { std::mem::zeroed() };
    touch.pointerInfo.pointerType = PT_TOUCH;
    touch.pointerInfo.pointerId = pointer_id;
    touch.pointerInfo.pointerFlags = flags;
    touch.pointerInfo.ptPixelLocation = POINT {
        x: screen.0,
        y: screen.1,
    };
    touch
}

const DOWN: u32 = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN;
const MOVING: u32 = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE;
const LIFTED: u32 = POINTER_FLAG_UP;

#[test]
fn fr5_a_fingers_pointer_message_is_a_touch_contact_in_client_pixels() {
    // The client area begins at (1000, 400) on the screen.
    let mut touch = touch_info(1503, DOWN, (1250, 700));
    assert_eq!(
        touch_input(WM_POINTERDOWN, &touch, (1000, 400)),
        Some(WindowInput::Contact {
            tool: PointerTool::Touch,
            pointer_id: 1503,
            phase: ContactPhase::Down,
            position: (250.0, 300.0),
            pressure: None,
            buttons: 0,
            tilt: (0.0, 0.0),
        })
    );
    touch.touchMask = TOUCH_MASK_PRESSURE;
    touch.pressure = 256;
    let Some(WindowInput::Contact { pressure, .. }) =
        touch_input(WM_POINTERDOWN, &touch, (1000, 400))
    else {
        panic!("a finger down is a contact");
    };
    assert_eq!(pressure, Some(0.25));
    // A finger held past the window's left edge goes on, as the mouse does.
    let left = touch_info(1503, MOVING, (990, 700));
    let Some(WindowInput::Contact { position, .. }) =
        touch_input(WM_POINTERUPDATE, &left, (1000, 400))
    else {
        panic!("a moving finger is a contact");
    };
    assert_eq!(position, (-10.0, 300.0));
    // A finger that is not touching is nothing.
    let away = touch_info(
        1503,
        POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE,
        (1250, 700),
    );
    assert_eq!(touch_input(WM_POINTERUPDATE, &away, (1000, 400)), None);
}

/// The window at 200%: its client pixels are the task's pixels, so a finger lands on the
/// task pixel under it, below the title bar as above it.
#[test]
fn fr5_at_200_percent_a_finger_lands_on_the_task_pixel_under_it() {
    let origin = (2000, 800);
    let geometry = WindowGeometry::one_to_one((1600, 1200), (1600, 1200));
    let mut mapper = PointerMapper::new(3);
    for (screen, expected) in [
        ((2000, 800), (0.0, 0.0)),
        ((2000 + 1599, 800 + 1199), (1599.0, 1199.0)),
        // Under the title bar band, which is part of the client area.
        ((2000 + 800, 800 + 70), (800.0, 70.0)),
    ] {
        let input = touch_input(WM_POINTERDOWN, &touch_info(5, DOWN, screen), origin)
            .expect("a finger down is a contact");
        let event = mapper.handle(input, geometry).events[0];
        assert_eq!((event.x, event.y), expected, "a finger at {screen:?}");
    }
}

/// The setup the offset was reported on: a main monitor at 100% at the origin, and a second
/// monitor at 125% above it and to the right, whose top is above the main monitor's
/// (negative screen coordinates). The Settings task keeps the main monitor's density, so its
/// window on the second monitor shows each task pixel at 164 dpi over 130 dpi.
#[test]
fn fr4_on_a_monitor_of_another_scale_above_the_main_one_a_click_lands_on_the_task_pixel_under_it() {
    use dsa_host_core::compositor::window_sizing::{
        density_for_dpi, scale_between, to_window_size,
    };
    let scale = scale_between(density_for_dpi(120), density_for_dpi(96));
    let task = (810, 1661);
    let geometry = WindowGeometry {
        client: to_window_size(task, scale),
        task,
        scale,
    };
    // The client area begins above the main monitor's top edge.
    let origin = (2900, -880);
    let mut mapper = PointerMapper::new(6);
    for (screen, expected) in [
        ((2900, -880), (0.0, 0.0)),
        // 820 by 1640 window pixels in: 650 by 1300 task pixels at 82:65.
        ((2900 + 820, -880 + 1640), (650.0, 1300.0)),
        ((2900 + 164, -880 + 82), (130.0, 65.0)),
    ] {
        let input = touch_input(WM_POINTERDOWN, &touch_info(9, DOWN, screen), origin)
            .expect("a finger down is a contact");
        let event = mapper.handle(input, geometry).events[0];
        assert_eq!((event.x, event.y), expected, "a finger at {screen:?}");
        let lifted = touch_input(WM_POINTERUP, &touch_info(9, LIFTED, screen), origin)
            .expect("a finger up is a contact");
        mapper.handle(lifted, geometry);
    }
    // The mouse's messages carry client coordinates; the same point lands on the same pixel.
    let mut mouse = PointerMapper::new(6);
    let down = mouse.handle(
        WindowInput::MouseButton {
            button: MouseButton::Left,
            down: true,
            position: message_point(lparam(820, 1640)),
        },
        geometry,
    );
    let pressed = down.events.last().expect("a press");
    assert_eq!((pressed.x, pressed.y), (650.0, 1300.0));
}

/// Ten fingers, a pinch, and every finger lifted: each finger keeps its own pointer from
/// the window's messages to the control channel, in the order the fingers touched.
#[test]
fn fr5_ten_fingers_and_a_pinch_reach_the_control_channel_finger_by_finger() {
    let origin = (300, 200);
    let geometry = WindowGeometry::one_to_one((800, 600), (800, 600));
    let mut mapper = PointerMapper::new(7);
    let mut sent = Vec::new();
    let mut feed = |message: u32, touch: POINTER_TOUCH_INFO| {
        let input = touch_input(message, &touch, origin).expect("a touching finger");
        for event in mapper.handle(input, geometry).events {
            sent.push(event.to_control());
        }
    };
    let ids: Vec<u32> = (0..10).map(|finger| 700 + finger * 3).collect();
    let at = |finger: usize, spread: i32| {
        (
            origin.0 + 400 + (finger as i32 - 5) * (30 + spread),
            origin.1 + 400,
        )
    };
    for (finger, &id) in ids.iter().enumerate() {
        feed(WM_POINTERDOWN, touch_info(id, DOWN, at(finger, 0)));
    }
    for step in 1..=3 {
        for (finger, &id) in ids.iter().enumerate() {
            feed(
                WM_POINTERUPDATE,
                touch_info(id, MOVING, at(finger, step * 5)),
            );
        }
    }
    for (finger, &id) in ids.iter().enumerate() {
        feed(WM_POINTERUP, touch_info(id, LIFTED, at(finger, 15)));
    }

    assert_eq!(sent.len(), 50);
    assert!(sent.iter().all(|event| event.task_id == 7
        && event.tool == control::PointerTool::Touch as i32
        && event.buttons == 0));
    let downs: Vec<u32> = sent[..10].iter().map(|event| event.pointer_id).collect();
    assert_eq!(downs, ids);
    assert!(sent[..10]
        .iter()
        .all(|event| event.action == control::PointerAction::Down as i32));
    assert!(sent[10..40]
        .iter()
        .all(|event| event.action == control::PointerAction::Move as i32));
    assert!(sent[40..]
        .iter()
        .all(|event| event.action == control::PointerAction::Up as i32));
    // The pinch spreads the fingers apart around the middle of the window.
    assert_eq!((sent[0].x, sent[0].y), (250.0, 400.0));
    assert_eq!((sent[39].x, sent[39].y), (400.0 + 4.0 * 45.0, 400.0));
    assert_eq!((sent[40].x, sent[40].y), (400.0 - 5.0 * 45.0, 400.0));
    // Without a measured pressure, a touching finger presses fully.
    assert!(sent[..40].iter().all(|event| event.pressure == 1.0));
}

#[test]
fn fr5_a_pens_pointer_message_brings_its_pressure_barrel_button_eraser_and_hover() {
    // SAFETY: plain data, for which all zero is a valid value.
    let mut pen: POINTER_PEN_INFO = unsafe { std::mem::zeroed() };
    pen.pointerInfo.pointerType = PT_PEN;
    pen.pointerInfo.pointerId = 9;
    pen.pointerInfo.ptPixelLocation = POINT { x: 140, y: 260 };
    pen.pointerInfo.pointerFlags = POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE;
    pen.penFlags = PEN_FLAG_BARREL;
    pen.penMask = PEN_MASK_PRESSURE;
    pen.pressure = 0;
    assert_eq!(
        pen_input(WM_POINTERUPDATE, &pen, (100, 200)),
        Some(WindowInput::Contact {
            tool: PointerTool::Pen,
            pointer_id: 9,
            phase: ContactPhase::Hover,
            position: (40.0, 60.0),
            pressure: Some(0.0),
            buttons: buttons::STYLUS_PRIMARY,
            tilt: (0.0, 0.0),
        })
    );
    pen.pointerInfo.pointerFlags = DOWN;
    pen.penFlags = PEN_FLAG_ERASER;
    pen.penMask = PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
    pen.pressure = 768;
    pen.tiltX = 30;
    pen.tiltY = -45;
    assert_eq!(
        pen_input(WM_POINTERDOWN, &pen, (100, 200)),
        Some(WindowInput::Contact {
            tool: PointerTool::Eraser,
            pointer_id: 9,
            phase: ContactPhase::Down,
            position: (40.0, 60.0),
            pressure: Some(0.75),
            buttons: 0,
            tilt: (30.0, -45.0),
        })
    );
}

#[test]
fn fr5_a_pens_tilt_is_read_only_on_the_axes_it_measures() {
    // SAFETY: plain data, for which all zero is a valid value.
    let mut pen: POINTER_PEN_INFO = unsafe { std::mem::zeroed() };
    pen.tiltX = 40;
    pen.tiltY = -20;
    assert_eq!(pen_tilt(&pen), (0.0, 0.0));
    pen.penMask = PEN_MASK_TILT_X;
    assert_eq!(pen_tilt(&pen), (40.0, 0.0));
    pen.penMask = PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
    assert_eq!(pen_tilt(&pen), (40.0, -20.0));
    pen.tiltX = 300;
    assert_eq!(pen_tilt(&pen), (90.0, -20.0));
}

//! How a task window names the keys of its key messages, before the platform-neutral rules
//! turn them into the guest's key events.
#![cfg(windows)]

use dsa_host_core::keys::keycode;
use dsa_host_windows::app_window::keys::{guest_key, scan_and_extended};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

#[test]
fn fr16_letters_digits_and_punctuation_type_through_their_key_codes() {
    assert_eq!(guest_key(VK_A, 0x1e, false), Some(keycode::A));
    assert_eq!(guest_key(VK_Z, 0x2c, false), Some(keycode::Z));
    assert_eq!(guest_key(VK_0, 0x0b, false), Some(keycode::NUM_0));
    assert_eq!(guest_key(VK_9, 0x0a, false), Some(keycode::NUM_9));
    assert_eq!(guest_key(VK_SPACE, 0x39, false), Some(keycode::SPACE));
    for (vk, code) in [
        (VK_OEM_1, keycode::SEMICOLON),
        (VK_OEM_PLUS, keycode::EQUALS),
        (VK_OEM_COMMA, keycode::COMMA),
        (VK_OEM_MINUS, keycode::MINUS),
        (VK_OEM_PERIOD, keycode::PERIOD),
        (VK_OEM_2, keycode::SLASH),
        (VK_OEM_3, keycode::GRAVE),
        (VK_OEM_4, keycode::LEFT_BRACKET),
        (VK_OEM_5, keycode::BACKSLASH),
        (VK_OEM_6, keycode::RIGHT_BRACKET),
        (VK_OEM_7, keycode::APOSTROPHE),
    ] {
        assert_eq!(guest_key(vk, 0, false), Some(code), "virtual key {vk:#x}");
    }
}

#[test]
fn fr16_editing_and_navigation_keys_have_their_key_codes() {
    for (vk, code) in [
        (VK_UP, keycode::DPAD_UP),
        (VK_DOWN, keycode::DPAD_DOWN),
        (VK_LEFT, keycode::DPAD_LEFT),
        (VK_RIGHT, keycode::DPAD_RIGHT),
        (VK_BACK, keycode::DEL),
        (VK_DELETE, keycode::FORWARD_DEL),
        (VK_TAB, keycode::TAB),
        (VK_ESCAPE, keycode::ESCAPE),
        (VK_HOME, keycode::MOVE_HOME),
        (VK_END, keycode::MOVE_END),
        (VK_PRIOR, keycode::PAGE_UP),
        (VK_NEXT, keycode::PAGE_DOWN),
        (VK_INSERT, keycode::INSERT),
        (VK_APPS, keycode::MENU),
    ] {
        // The same with or without the extended flag: the arrows beside the keypad and the
        // keypad's own with Num Lock off move the cursor alike.
        assert_eq!(guest_key(vk, 0, true), Some(code), "virtual key {vk:#x}");
        assert_eq!(guest_key(vk, 0, false), Some(code), "virtual key {vk:#x}");
    }
}

#[test]
fn fr16_function_keys_and_the_keypad_have_their_key_codes() {
    assert_eq!(guest_key(VK_F1, 0x3b, false), Some(keycode::F1));
    assert_eq!(guest_key(VK_F12, 0x58, false), Some(keycode::F12));
    // The guest has no key codes past F12.
    assert_eq!(guest_key(VK_F13, 0x64, false), None);
    assert_eq!(guest_key(VK_NUMPAD0, 0x52, false), Some(keycode::NUMPAD_0));
    assert_eq!(guest_key(VK_NUMPAD9, 0x49, false), Some(keycode::NUMPAD_9));
    assert_eq!(
        guest_key(VK_DIVIDE, 0x35, true),
        Some(keycode::NUMPAD_DIVIDE)
    );
    assert_eq!(
        guest_key(VK_MULTIPLY, 0x37, false),
        Some(keycode::NUMPAD_MULTIPLY)
    );
    assert_eq!(
        guest_key(VK_SUBTRACT, 0x4a, false),
        Some(keycode::NUMPAD_SUBTRACT)
    );
    assert_eq!(guest_key(VK_ADD, 0x4e, false), Some(keycode::NUMPAD_ADD));
    assert_eq!(
        guest_key(VK_DECIMAL, 0x53, false),
        Some(keycode::NUMPAD_DOT)
    );
    assert_eq!(guest_key(VK_RETURN, 0x1c, false), Some(keycode::ENTER));
    assert_eq!(
        guest_key(VK_RETURN, 0x1c, true),
        Some(keycode::NUMPAD_ENTER)
    );
}

#[test]
fn fr16_left_and_right_modifiers_are_told_apart() {
    assert_eq!(guest_key(VK_SHIFT, 0x2a, false), Some(keycode::SHIFT_LEFT));
    assert_eq!(guest_key(VK_SHIFT, 0x36, false), Some(keycode::SHIFT_RIGHT));
    assert_eq!(guest_key(VK_CONTROL, 0x1d, false), Some(keycode::CTRL_LEFT));
    assert_eq!(guest_key(VK_CONTROL, 0x1d, true), Some(keycode::CTRL_RIGHT));
    assert_eq!(guest_key(VK_MENU, 0x38, false), Some(keycode::ALT_LEFT));
    assert_eq!(guest_key(VK_MENU, 0x38, true), Some(keycode::ALT_RIGHT));
    assert_eq!(guest_key(VK_LWIN, 0x5b, true), Some(keycode::META_LEFT));
    assert_eq!(guest_key(VK_RWIN, 0x5c, true), Some(keycode::META_RIGHT));
    assert_eq!(
        guest_key(VK_RSHIFT, 0x36, false),
        Some(keycode::SHIFT_RIGHT)
    );
    assert_eq!(guest_key(VK_CAPITAL, 0x3a, false), Some(keycode::CAPS_LOCK));
}

#[test]
fn fr16_a_key_the_input_method_composes_with_is_not_a_guest_key() {
    assert_eq!(guest_key(VK_PROCESSKEY, 0x1e, false), None);
}

#[test]
fn fr16_the_scan_code_and_extended_flag_come_from_the_lparam() {
    assert_eq!(scan_and_extended(0x011d_0001), (0x1d, true));
    assert_eq!(scan_and_extended(0xc036_0001_u32 as isize), (0x36, false));
}

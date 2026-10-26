//! Key input in a task window: the window's key messages, as the platform-neutral input that
//! `KeyMapper` turns into the guest's key events.
//!
//! This is the only part of key input that knows Win32. It names each key by the guest's key
//! code (from the virtual key, with the extended-key flag and the scan code telling the left
//! and right modifiers, the keypad's Enter and the main Enter apart), reads the locks, and
//! hands each event to a channel; the rules (what is held, repeats, modifiers) are the
//! mapper's.
//!
//! Every key with a guest key code goes to the guest, the ones that type text included, so
//! typing works through key events. Windows still turns them into WM_CHAR, which this module
//! leaves alone. A key the input method is composing with arrives as VK_PROCESSKEY; that key
//! belongs to the input method, so it is left to Windows and never reaches the guest.

use std::cell::RefCell;
use std::sync::mpsc::Sender;

use dsa_host_core::input::InputEvent;
use dsa_host_core::keys::{keycode, meta, KeyInput, KeyMapper};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_0, VK_9, VK_A, VK_ADD, VK_APPS, VK_BACK, VK_BROWSER_BACK,
    VK_CANCEL, VK_CAPITAL, VK_CLEAR, VK_CONTROL, VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F1, VK_F12, VK_F4, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_MENU,
    VK_MULTIPLY, VK_NEXT, VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD9, VK_OEM_1, VK_OEM_2, VK_OEM_3,
    VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS,
    VK_PAUSE, VK_PRIOR, VK_PROCESSKEY, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT,
    VK_RWIN, VK_SCROLL, VK_SHIFT, VK_SNAPSHOT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP,
    VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP, VK_Z,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMessageTime, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// The scan code of the right Shift key, which shares its virtual key with the left one.
const RIGHT_SHIFT_SCAN: u32 = 0x36;

/// What a key message's `lparam` says about the key: its scan code and whether it is an
/// extended key (bit 24), such as the right Ctrl, the keypad's Enter or an arrow key outside
/// the keypad.
pub fn scan_and_extended(lparam: LPARAM) -> (u32, bool) {
    let bits = lparam as usize;
    (((bits >> 16) & 0xff) as u32, bits & (1 << 24) != 0)
}

/// The guest's key code for a key, from its virtual key, scan code and extended flag. `None`
/// for a key the guest has no code for, which then stays with Windows.
pub fn guest_key(vk: VIRTUAL_KEY, scan: u32, extended: bool) -> Option<u32> {
    let code = match vk {
        VK_A..=VK_Z => keycode::A + u32::from(vk - VK_A),
        VK_0..=VK_9 => keycode::NUM_0 + u32::from(vk - VK_0),
        VK_NUMPAD0..=VK_NUMPAD9 => keycode::NUMPAD_0 + u32::from(vk - VK_NUMPAD0),
        VK_F1..=VK_F12 => keycode::F1 + u32::from(vk - VK_F1),
        VK_SHIFT if scan == RIGHT_SHIFT_SCAN => keycode::SHIFT_RIGHT,
        VK_SHIFT | VK_LSHIFT => keycode::SHIFT_LEFT,
        VK_RSHIFT => keycode::SHIFT_RIGHT,
        VK_CONTROL if extended => keycode::CTRL_RIGHT,
        VK_CONTROL | VK_LCONTROL => keycode::CTRL_LEFT,
        VK_RCONTROL => keycode::CTRL_RIGHT,
        VK_MENU if extended => keycode::ALT_RIGHT,
        VK_MENU | VK_LMENU => keycode::ALT_LEFT,
        VK_RMENU => keycode::ALT_RIGHT,
        VK_LWIN => keycode::META_LEFT,
        VK_RWIN => keycode::META_RIGHT,
        VK_RETURN if extended => keycode::NUMPAD_ENTER,
        VK_RETURN => keycode::ENTER,
        VK_BACK => keycode::DEL,
        VK_DELETE => keycode::FORWARD_DEL,
        VK_TAB => keycode::TAB,
        VK_ESCAPE => keycode::ESCAPE,
        VK_SPACE => keycode::SPACE,
        VK_UP => keycode::DPAD_UP,
        VK_DOWN => keycode::DPAD_DOWN,
        VK_LEFT => keycode::DPAD_LEFT,
        VK_RIGHT => keycode::DPAD_RIGHT,
        VK_HOME => keycode::MOVE_HOME,
        VK_END => keycode::MOVE_END,
        VK_PRIOR => keycode::PAGE_UP,
        VK_NEXT => keycode::PAGE_DOWN,
        VK_INSERT => keycode::INSERT,
        // The keypad's 5 with Num Lock off does nothing on the guest either.
        VK_CLEAR => return None,
        VK_CAPITAL => keycode::CAPS_LOCK,
        VK_NUMLOCK => keycode::NUM_LOCK,
        VK_SCROLL => keycode::SCROLL_LOCK,
        VK_SNAPSHOT => keycode::SYSRQ,
        VK_PAUSE | VK_CANCEL => keycode::BREAK,
        VK_APPS => keycode::MENU,
        VK_MULTIPLY => keycode::NUMPAD_MULTIPLY,
        VK_ADD => keycode::NUMPAD_ADD,
        VK_SUBTRACT => keycode::NUMPAD_SUBTRACT,
        VK_DECIMAL => keycode::NUMPAD_DOT,
        VK_DIVIDE => keycode::NUMPAD_DIVIDE,
        VK_OEM_1 => keycode::SEMICOLON,
        VK_OEM_PLUS => keycode::EQUALS,
        VK_OEM_COMMA => keycode::COMMA,
        VK_OEM_MINUS => keycode::MINUS,
        VK_OEM_PERIOD => keycode::PERIOD,
        VK_OEM_2 => keycode::SLASH,
        VK_OEM_3 => keycode::GRAVE,
        VK_OEM_4 => keycode::LEFT_BRACKET,
        VK_OEM_5 => keycode::BACKSLASH,
        VK_OEM_6 => keycode::RIGHT_BRACKET,
        VK_OEM_7 => keycode::APOSTROPHE,
        VK_VOLUME_MUTE => keycode::VOLUME_MUTE,
        VK_VOLUME_DOWN => keycode::VOLUME_DOWN,
        VK_VOLUME_UP => keycode::VOLUME_UP,
        VK_MEDIA_NEXT_TRACK => keycode::MEDIA_NEXT,
        VK_MEDIA_PREV_TRACK => keycode::MEDIA_PREVIOUS,
        VK_MEDIA_STOP => keycode::MEDIA_STOP,
        VK_MEDIA_PLAY_PAUSE => keycode::MEDIA_PLAY_PAUSE,
        VK_BROWSER_BACK => keycode::BACK,
        _ => return None,
    };
    Some(code)
}

/// The locks on now, as the guest's meta state bits.
unsafe fn locks() -> u32 {
    let on = |vk: VIRTUAL_KEY| GetKeyState(i32::from(vk)) & 1 != 0;
    let mut bits = 0;
    if on(VK_CAPITAL) {
        bits |= meta::CAPS_LOCK_ON;
    }
    if on(VK_NUMLOCK) {
        bits |= meta::NUM_LOCK_ON;
    }
    if on(VK_SCROLL) {
        bits |= meta::SCROLL_LOCK_ON;
    }
    bits
}

/// One task window's key input.
pub struct KeyWindow {
    mapper: RefCell<KeyMapper>,
    events: Sender<InputEvent>,
}

impl KeyWindow {
    pub fn new(task_id: i32, events: Sender<InputEvent>) -> Self {
        KeyWindow {
            mapper: RefCell::new(KeyMapper::new(task_id)),
            events,
        }
    }

    /// Handles a key message, or the loss of the keyboard focus. `None` for any other
    /// message, or one that Windows must also see.
    ///
    /// # Safety
    ///
    /// Called on the window's own thread, from its window procedure.
    pub unsafe fn handle(
        &self,
        _hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let vk = wparam as VIRTUAL_KEY;
                if vk == VK_PROCESSKEY {
                    return None;
                }
                // Alt+F4 closes the window, as in every other program; the guest never sees
                // the F4.
                if vk == VK_F4 && matches!(message, WM_SYSKEYDOWN | WM_SYSKEYUP) {
                    return None;
                }
                let (scan, extended) = scan_and_extended(lparam);
                let key_code = guest_key(vk, scan, extended)?;
                let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                let input = KeyInput {
                    key_code,
                    down,
                    locks: locks(),
                    time_ms: GetMessageTime() as u32,
                };
                let event = match self.mapper.try_borrow_mut() {
                    Ok(mut mapper) => mapper.handle(input),
                    Err(_) => return Some(0),
                };
                if let Some(event) = event {
                    // The sender ends only when the window host stops; input then has
                    // nowhere to go.
                    let _ = self.events.send(InputEvent::Key(event));
                }
                // Handled, so Alt alone or F10 does not open the window's menu, which would
                // take the keys that follow.
                Some(0)
            }
            // Alt with a key the guest took: without a menu to match it, Windows would only
            // beep. Alt+Space still opens the window's menu.
            WM_SYSCHAR if wparam != usize::from(b' ') => Some(0),
            WM_KILLFOCUS => {
                self.release_all();
                None
            }
            _ => None,
        }
    }

    /// Releases every key still held, as the window loses the keyboard focus.
    pub fn release_all(&self) {
        let released = match self.mapper.try_borrow_mut() {
            // SAFETY: GetMessageTime only reads this thread's message state.
            Ok(mut mapper) => mapper.release_all(unsafe { GetMessageTime() } as u32),
            Err(_) => return,
        };
        for event in released {
            let _ = self.events.send(InputEvent::Key(event));
        }
    }
}

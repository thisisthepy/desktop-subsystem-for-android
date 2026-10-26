//! Key input: the keys pressed and released in a task's window, as the guest task receives
//! them.
//!
//! A platform's window turns its own key messages into [`KeyInput`], naming each key by the
//! guest's key code ([`keycode`]), and hands them to the window's [`KeyMapper`]. The mapper
//! keeps which keys are held, so it can count a held key's repeats, work out the modifiers in
//! effect ([`meta`]), drop a release the window never saw pressed, and release every held key
//! when the window loses the keyboard focus. Nothing here names a window system, so the rules
//! run in tests and in whichever process owns the window.
//!
//! Every key travels as a key event, including those that type text. Text input through the
//! host's input method takes its characters on a path of its own; a window whose input method
//! has claimed a key simply does not hand it to the mapper.

use dsa_protocol::control;
use dsa_protocol::host_api::types as wire;

/// The guest's key codes (`android.view.KeyEvent.KEYCODE_*`). The host maps its keys to
/// these, and they pass through to the guest unchanged.
pub mod keycode {
    pub const BACK: u32 = 4;
    pub const NUM_0: u32 = 7;
    pub const NUM_9: u32 = 16;
    pub const DPAD_UP: u32 = 19;
    pub const DPAD_DOWN: u32 = 20;
    pub const DPAD_LEFT: u32 = 21;
    pub const DPAD_RIGHT: u32 = 22;
    pub const VOLUME_UP: u32 = 24;
    pub const VOLUME_DOWN: u32 = 25;
    pub const A: u32 = 29;
    pub const Z: u32 = 54;
    pub const COMMA: u32 = 55;
    pub const PERIOD: u32 = 56;
    pub const ALT_LEFT: u32 = 57;
    pub const ALT_RIGHT: u32 = 58;
    pub const SHIFT_LEFT: u32 = 59;
    pub const SHIFT_RIGHT: u32 = 60;
    pub const TAB: u32 = 61;
    pub const SPACE: u32 = 62;
    pub const ENTER: u32 = 66;
    /// Backspace.
    pub const DEL: u32 = 67;
    pub const GRAVE: u32 = 68;
    pub const MINUS: u32 = 69;
    pub const EQUALS: u32 = 70;
    pub const LEFT_BRACKET: u32 = 71;
    pub const RIGHT_BRACKET: u32 = 72;
    pub const BACKSLASH: u32 = 73;
    pub const SEMICOLON: u32 = 74;
    pub const APOSTROPHE: u32 = 75;
    pub const SLASH: u32 = 76;
    pub const MENU: u32 = 82;
    pub const MEDIA_PLAY_PAUSE: u32 = 85;
    pub const MEDIA_STOP: u32 = 86;
    pub const MEDIA_NEXT: u32 = 87;
    pub const MEDIA_PREVIOUS: u32 = 88;
    pub const PAGE_UP: u32 = 92;
    pub const PAGE_DOWN: u32 = 93;
    pub const ESCAPE: u32 = 111;
    /// Delete, which removes the character after the cursor.
    pub const FORWARD_DEL: u32 = 112;
    pub const CTRL_LEFT: u32 = 113;
    pub const CTRL_RIGHT: u32 = 114;
    pub const CAPS_LOCK: u32 = 115;
    pub const SCROLL_LOCK: u32 = 116;
    pub const META_LEFT: u32 = 117;
    pub const META_RIGHT: u32 = 118;
    pub const SYSRQ: u32 = 120;
    pub const BREAK: u32 = 121;
    pub const MOVE_HOME: u32 = 122;
    pub const MOVE_END: u32 = 123;
    pub const INSERT: u32 = 124;
    pub const F1: u32 = 131;
    pub const F12: u32 = 142;
    pub const NUM_LOCK: u32 = 143;
    pub const NUMPAD_0: u32 = 144;
    pub const NUMPAD_9: u32 = 153;
    pub const NUMPAD_DIVIDE: u32 = 154;
    pub const NUMPAD_MULTIPLY: u32 = 155;
    pub const NUMPAD_SUBTRACT: u32 = 156;
    pub const NUMPAD_ADD: u32 = 157;
    pub const NUMPAD_DOT: u32 = 158;
    pub const NUMPAD_ENTER: u32 = 160;
    pub const VOLUME_MUTE: u32 = 164;

    /// The highest key code the guest defines today, with room for the ones it adds.
    pub const MAX: u32 = 1023;

    /// The key code of a letter, `'a'` to `'z'` (either case).
    pub fn letter(letter: char) -> Option<u32> {
        let lower = letter.to_ascii_lowercase();
        lower
            .is_ascii_lowercase()
            .then(|| A + (lower as u32 - 'a' as u32))
    }

    /// The key code of a digit on the main keys, 0 to 9.
    pub fn digit(digit: u32) -> Option<u32> {
        (digit <= 9).then_some(NUM_0 + digit)
    }

    /// The key code of a digit on the numeric keypad, 0 to 9.
    pub fn numpad_digit(digit: u32) -> Option<u32> {
        (digit <= 9).then_some(NUMPAD_0 + digit)
    }

    /// The key code of a function key, F1 to F12. The guest has no key codes beyond F12.
    pub fn function(number: u32) -> Option<u32> {
        (1..=12).contains(&number).then(|| F1 + number - 1)
    }
}

/// The guest's meta state bits (`android.view.KeyEvent.META_*`): the modifiers held and the
/// locks on. A side's bit always comes with its modifier's bit.
pub mod meta {
    pub const SHIFT_ON: u32 = 0x1;
    pub const ALT_ON: u32 = 0x2;
    pub const ALT_LEFT_ON: u32 = 0x10;
    pub const ALT_RIGHT_ON: u32 = 0x20;
    pub const SHIFT_LEFT_ON: u32 = 0x40;
    pub const SHIFT_RIGHT_ON: u32 = 0x80;
    pub const CTRL_ON: u32 = 0x1000;
    pub const CTRL_LEFT_ON: u32 = 0x2000;
    pub const CTRL_RIGHT_ON: u32 = 0x4000;
    pub const META_ON: u32 = 0x1_0000;
    pub const META_LEFT_ON: u32 = 0x2_0000;
    pub const META_RIGHT_ON: u32 = 0x4_0000;
    pub const CAPS_LOCK_ON: u32 = 0x10_0000;
    pub const NUM_LOCK_ON: u32 = 0x20_0000;
    pub const SCROLL_LOCK_ON: u32 = 0x40_0000;

    /// The lock bits, which the window reports rather than the mapper working out.
    pub const LOCKS: u32 = CAPS_LOCK_ON | NUM_LOCK_ON | SCROLL_LOCK_ON;

    /// The bits a modifier key sets while it is held; 0 for any other key.
    pub fn of_modifier(key_code: u32) -> u32 {
        use super::keycode;
        match key_code {
            keycode::SHIFT_LEFT => SHIFT_ON | SHIFT_LEFT_ON,
            keycode::SHIFT_RIGHT => SHIFT_ON | SHIFT_RIGHT_ON,
            keycode::CTRL_LEFT => CTRL_ON | CTRL_LEFT_ON,
            keycode::CTRL_RIGHT => CTRL_ON | CTRL_RIGHT_ON,
            keycode::ALT_LEFT => ALT_ON | ALT_LEFT_ON,
            keycode::ALT_RIGHT => ALT_ON | ALT_RIGHT_ON,
            keycode::META_LEFT => META_ON | META_LEFT_ON,
            keycode::META_RIGHT => META_ON | META_RIGHT_ON,
            _ => 0,
        }
    }
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    /// The key went down, or is repeated while held ([`KeyEvent::repeat_count`] above 0).
    Down,
    /// The key came up.
    Up,
}

/// One key event for a guest task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub task_id: i32,
    pub action: KeyAction,
    /// The guest's key code ([`keycode`]).
    pub key_code: u32,
    /// How many times the key has been repeated since it went down: 0 for the press itself.
    pub repeat_count: u32,
    /// The modifiers and locks in effect after this event ([`meta`]).
    pub meta_state: u32,
    /// When the window saw the event, in milliseconds of the host's clock, which wraps.
    pub event_time_ms: u32,
    /// An up the user did not make: the window lost the keyboard focus while the key was held.
    pub canceled: bool,
}

impl KeyEvent {
    /// The event as the control channel carries it to the guest.
    pub fn to_control(&self) -> control::KeyEvent {
        control::KeyEvent {
            task_id: self.task_id,
            action: match self.action {
                KeyAction::Down => control::KeyAction::Down,
                KeyAction::Up => control::KeyAction::Up,
            } as i32,
            key_code: self.key_code,
            repeat_count: self.repeat_count,
            meta_state: self.meta_state,
            event_time_ms: self.event_time_ms,
            canceled: self.canceled,
        }
    }

    /// The event as the local API carries it to the service.
    pub fn to_wire(&self) -> wire::KeyEvent {
        wire::KeyEvent {
            task_id: self.task_id,
            action: match self.action {
                KeyAction::Down => wire::KeyAction::Down,
                KeyAction::Up => wire::KeyAction::Up,
            } as i32,
            key_code: self.key_code,
            repeat_count: self.repeat_count,
            meta_state: self.meta_state,
            event_time_ms: self.event_time_ms,
            canceled: self.canceled,
        }
    }

    /// The event the local API carried, or a reason it makes no sense: an action this side
    /// does not know, or a key code the guest cannot have.
    pub fn from_wire(event: &wire::KeyEvent) -> Result<KeyEvent, String> {
        let action = match wire::KeyAction::try_from(event.action) {
            Ok(wire::KeyAction::Down) => KeyAction::Down,
            Ok(wire::KeyAction::Up) => KeyAction::Up,
            Ok(wire::KeyAction::Unspecified) | Err(_) => {
                return Err(format!("unknown key action {}", event.action))
            }
        };
        if event.key_code == 0 || event.key_code > keycode::MAX {
            return Err(format!(
                "key code {} is not one the guest has (1 to {})",
                event.key_code,
                keycode::MAX
            ));
        }
        Ok(KeyEvent {
            task_id: event.task_id,
            action,
            key_code: event.key_code,
            repeat_count: event.repeat_count,
            meta_state: event.meta_state,
            event_time_ms: event.event_time_ms,
            canceled: event.canceled,
        })
    }
}

/// One key message of a window, already named by the guest's key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    pub key_code: u32,
    /// Whether the key went down (or the window system repeats it) rather than up.
    pub down: bool,
    /// The locks on after this message ([`meta::LOCKS`] bits); other bits are ignored.
    pub locks: u32,
    /// When the window saw it, in milliseconds of the host's clock.
    pub time_ms: u32,
}

/// One task window's keys: which are held, and since when they repeat.
#[derive(Debug)]
pub struct KeyMapper {
    task_id: i32,
    /// The keys held, in the order they went down, with how often each has repeated.
    held: Vec<(u32, u32)>,
    /// The locks the window last reported.
    locks: u32,
}

impl KeyMapper {
    pub fn new(task_id: i32) -> KeyMapper {
        KeyMapper {
            task_id,
            held: Vec::new(),
            locks: 0,
        }
    }

    /// The keys held now, in the order they went down.
    pub fn held(&self) -> Vec<u32> {
        self.held.iter().map(|(key, _)| *key).collect()
    }

    /// The modifier bits of the keys held, with the locks.
    fn meta_state(&self) -> u32 {
        self.held
            .iter()
            .fold(self.locks, |bits, (key, _)| bits | meta::of_modifier(*key))
    }

    /// The event for one key message: a press, a repeat of a key already held, or a release.
    /// `None` for a release of a key the window never saw pressed (pressed in another window
    /// before the focus moved here), which the guest has no use for.
    pub fn handle(&mut self, input: KeyInput) -> Option<KeyEvent> {
        self.locks = input.locks & meta::LOCKS;
        let at = self.held.iter().position(|(key, _)| *key == input.key_code);
        let (action, repeat_count) = match (input.down, at) {
            (true, Some(at)) => {
                self.held[at].1 = self.held[at].1.saturating_add(1);
                (KeyAction::Down, self.held[at].1)
            }
            (true, None) => {
                self.held.push((input.key_code, 0));
                (KeyAction::Down, 0)
            }
            (false, Some(at)) => {
                self.held.remove(at);
                (KeyAction::Up, 0)
            }
            (false, None) => return None,
        };
        Some(KeyEvent {
            task_id: self.task_id,
            action,
            key_code: input.key_code,
            repeat_count,
            meta_state: self.meta_state(),
            event_time_ms: input.time_ms,
            canceled: false,
        })
    }

    /// The window lost the keyboard focus: a canceled up for every key still held, the last
    /// pressed first, so no key stays down in the guest. The window system will not tell this
    /// window when they really come up.
    pub fn release_all(&mut self, time_ms: u32) -> Vec<KeyEvent> {
        let mut events = Vec::with_capacity(self.held.len());
        while let Some((key_code, _)) = self.held.pop() {
            events.push(KeyEvent {
                task_id: self.task_id,
                action: KeyAction::Up,
                key_code,
                repeat_count: 0,
                meta_state: self.meta_state(),
                event_time_ms: time_ms,
                canceled: true,
            });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(mapper: &mut KeyMapper, key_code: u32, down: bool) -> Option<KeyEvent> {
        mapper.handle(KeyInput {
            key_code,
            down,
            locks: 0,
            time_ms: 1000,
        })
    }

    #[test]
    fn fr16_a_key_press_is_a_down_then_an_up_for_its_task() {
        let mut mapper = KeyMapper::new(7);
        let down = press(&mut mapper, keycode::ENTER, true).unwrap();
        assert_eq!(
            (down.task_id, down.action, down.key_code, down.repeat_count),
            (7, KeyAction::Down, keycode::ENTER, 0)
        );
        let up = press(&mut mapper, keycode::ENTER, false).unwrap();
        assert_eq!((up.action, up.canceled), (KeyAction::Up, false));
        assert!(mapper.held().is_empty());
    }

    #[test]
    fn fr16_a_held_key_repeats_with_a_growing_count() {
        let mut mapper = KeyMapper::new(1);
        let counts: Vec<u32> = (0..4)
            .map(|_| press(&mut mapper, keycode::DEL, true).unwrap().repeat_count)
            .collect();
        assert_eq!(counts, vec![0, 1, 2, 3]);
        assert_eq!(
            press(&mut mapper, keycode::DEL, false)
                .unwrap()
                .repeat_count,
            0
        );
        // Pressed again, it starts over.
        assert_eq!(
            press(&mut mapper, keycode::DEL, true).unwrap().repeat_count,
            0
        );
    }

    #[test]
    fn fr16_ctrl_a_carries_the_ctrl_modifier_and_its_side() {
        let mut mapper = KeyMapper::new(1);
        let ctrl = press(&mut mapper, keycode::CTRL_LEFT, true).unwrap();
        // A modifier's own down already counts it.
        assert_eq!(ctrl.meta_state, meta::CTRL_ON | meta::CTRL_LEFT_ON);
        let a = press(&mut mapper, keycode::A, true).unwrap();
        assert_eq!(a.meta_state, meta::CTRL_ON | meta::CTRL_LEFT_ON);
        press(&mut mapper, keycode::A, false);
        let release = press(&mut mapper, keycode::CTRL_LEFT, false).unwrap();
        assert_eq!(release.meta_state, 0);
    }

    #[test]
    fn fr16_left_and_right_modifiers_are_told_apart() {
        let mut mapper = KeyMapper::new(1);
        press(&mut mapper, keycode::SHIFT_RIGHT, true);
        press(&mut mapper, keycode::ALT_RIGHT, true);
        let event = press(&mut mapper, keycode::META_LEFT, true).unwrap();
        assert_eq!(
            event.meta_state,
            meta::SHIFT_ON
                | meta::SHIFT_RIGHT_ON
                | meta::ALT_ON
                | meta::ALT_RIGHT_ON
                | meta::META_ON
                | meta::META_LEFT_ON
        );
        // With both shifts down, releasing one leaves Shift on.
        press(&mut mapper, keycode::SHIFT_LEFT, true);
        let event = press(&mut mapper, keycode::SHIFT_RIGHT, false).unwrap();
        assert_eq!(
            event.meta_state & (meta::SHIFT_ON | meta::SHIFT_LEFT_ON | meta::SHIFT_RIGHT_ON),
            meta::SHIFT_ON | meta::SHIFT_LEFT_ON
        );
    }

    #[test]
    fn fr16_the_locks_the_window_reports_go_with_every_event() {
        let mut mapper = KeyMapper::new(1);
        let event = mapper
            .handle(KeyInput {
                key_code: keycode::A,
                down: true,
                locks: meta::CAPS_LOCK_ON | meta::NUM_LOCK_ON | meta::CTRL_ON,
                time_ms: 5,
            })
            .unwrap();
        // Only lock bits come from the window; Ctrl is not held.
        assert_eq!(event.meta_state, meta::CAPS_LOCK_ON | meta::NUM_LOCK_ON);
        assert_eq!(event.event_time_ms, 5);
    }

    #[test]
    fn fr16_a_release_the_window_never_saw_pressed_is_dropped() {
        let mut mapper = KeyMapper::new(1);
        assert_eq!(press(&mut mapper, keycode::TAB, false), None);
    }

    #[test]
    fn fr16_losing_the_focus_releases_every_held_key_as_canceled() {
        let mut mapper = KeyMapper::new(3);
        press(&mut mapper, keycode::SHIFT_LEFT, true);
        press(&mut mapper, keycode::DPAD_RIGHT, true);
        let released = mapper.release_all(2000);
        let summary: Vec<(u32, KeyAction, bool, u32)> = released
            .iter()
            .map(|event| {
                (
                    event.key_code,
                    event.action,
                    event.canceled,
                    event.meta_state,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    keycode::DPAD_RIGHT,
                    KeyAction::Up,
                    true,
                    meta::SHIFT_ON | meta::SHIFT_LEFT_ON
                ),
                (keycode::SHIFT_LEFT, KeyAction::Up, true, 0),
            ]
        );
        assert!(released.iter().all(|event| event.task_id == 3));
        assert!(mapper.held().is_empty());
        assert!(mapper.release_all(2001).is_empty());
    }

    #[test]
    fn fr16_key_codes_name_letters_digits_and_function_keys() {
        assert_eq!(keycode::letter('a'), Some(keycode::A));
        assert_eq!(keycode::letter('Z'), Some(keycode::Z));
        assert_eq!(keycode::letter('1'), None);
        assert_eq!(keycode::digit(9), Some(keycode::NUM_9));
        assert_eq!(keycode::numpad_digit(0), Some(keycode::NUMPAD_0));
        assert_eq!(keycode::function(12), Some(keycode::F12));
        assert_eq!(keycode::function(13), None);
    }

    #[test]
    fn fr16_events_cross_the_local_api_unchanged() {
        let event = KeyEvent {
            task_id: 9,
            action: KeyAction::Up,
            key_code: keycode::FORWARD_DEL,
            repeat_count: 2,
            meta_state: meta::CTRL_ON | meta::CTRL_RIGHT_ON,
            event_time_ms: u32::MAX,
            canceled: true,
        };
        assert_eq!(KeyEvent::from_wire(&event.to_wire()), Ok(event));
        let control = event.to_control();
        assert_eq!(control.action, control::KeyAction::Up as i32);
        assert_eq!(control.key_code, keycode::FORWARD_DEL);
        assert_eq!(control.meta_state, meta::CTRL_ON | meta::CTRL_RIGHT_ON);
        assert!(control.canceled);
    }

    #[test]
    fn nfr5_a_key_event_that_makes_no_sense_is_refused() {
        let good = KeyEvent {
            task_id: 1,
            action: KeyAction::Down,
            key_code: keycode::A,
            repeat_count: 0,
            meta_state: 0,
            event_time_ms: 0,
            canceled: false,
        };
        let mut unknown = good.to_wire();
        unknown.action = 9;
        assert!(KeyEvent::from_wire(&unknown).is_err());
        let mut no_key = good.to_wire();
        no_key.key_code = 0;
        assert!(KeyEvent::from_wire(&no_key).is_err());
        let mut huge = good.to_wire();
        huge.key_code = 70_000;
        assert!(KeyEvent::from_wire(&huge).is_err());
    }
}

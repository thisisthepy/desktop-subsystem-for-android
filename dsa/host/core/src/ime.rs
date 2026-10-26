//! Text input: what the host's input method composes and commits in a task's window, as the
//! guest's editor receives it.
//!
//! The host is the shell, so text is composed on the host, by the input method the user
//! already has, and the guest has no keyboard of its own. A platform's window takes the input
//! method's composition and the characters its keys type, and hands them to the window's
//! [`TextInput`]. That turns them into [`ImeEvent`]s: the composing text with its cursor,
//! which the agent puts into the focused editor as its composing region, and the committed
//! text, which takes the composition's place. Nothing here names a window system, so the
//! rules run in tests and in whichever process owns the window.
//!
//! A key that types a character travels as text, not as a key event, so a layout other than
//! US types what its keys show; the window gives the guest that key's press neither as a key
//! event nor twice. Keys that type nothing (arrows, Enter, Backspace, shortcuts with Ctrl)
//! stay key events (see [`crate::keys`]).

use dsa_protocol::control;
use dsa_protocol::host_api::types as wire;

/// The longest text one message carries, in UTF-16 code units. A composition is a few
/// characters; a commit longer than this is what a paste through the input method would be,
/// and is cut, on a character boundary, rather than sent as a frame the guest refuses.
pub const MAX_TEXT_UNITS: usize = 4096;

/// One change to the text of a task's focused editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    /// The whole composition is now `text`, with the cursor `cursor` UTF-16 code units from
    /// its start. An empty `text` removes the composition.
    Composition {
        task_id: i32,
        text: String,
        cursor: u32,
    },
    /// `text` is typed, in place of the composition if there is one, which ends.
    Commit { task_id: i32, text: String },
}

/// How many UTF-16 code units `text` takes.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `text` cut to at most [`MAX_TEXT_UNITS`] UTF-16 code units, on a character boundary.
fn bounded(text: &str) -> String {
    let mut units = 0;
    text.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= MAX_TEXT_UNITS
        })
        .collect()
}

fn check_text(text: &str) -> Result<(), String> {
    let units = utf16_len(text);
    if units > MAX_TEXT_UNITS {
        return Err(format!(
            "the text is {units} UTF-16 code units long, more than the {MAX_TEXT_UNITS} one \
             message may carry"
        ));
    }
    Ok(())
}

impl ImeEvent {
    /// The task whose editor the event is for.
    pub fn task_id(&self) -> i32 {
        match self {
            ImeEvent::Composition { task_id, .. } | ImeEvent::Commit { task_id, .. } => *task_id,
        }
    }

    /// The event as the control channel carries it to the guest.
    pub fn to_control(&self) -> control::envelope::Body {
        match self.clone() {
            ImeEvent::Composition {
                task_id,
                text,
                cursor,
            } => control::envelope::Body::ImeComposition(control::ImeComposition {
                task_id,
                text,
                cursor,
            }),
            ImeEvent::Commit { task_id, text } => {
                control::envelope::Body::ImeCommit(control::ImeCommit { task_id, text })
            }
        }
    }

    /// The event as the local API carries it to the service.
    pub fn to_wire(&self) -> wire::envelope::Body {
        match self.clone() {
            ImeEvent::Composition {
                task_id,
                text,
                cursor,
            } => wire::envelope::Body::ImeComposition(wire::ImeComposition {
                task_id,
                text,
                cursor,
            }),
            ImeEvent::Commit { task_id, text } => {
                wire::envelope::Body::ImeCommit(wire::ImeCommit { task_id, text })
            }
        }
    }

    /// The composition the local API carried, or a reason it makes no sense: text longer
    /// than a message may carry, or a cursor past its end.
    pub fn from_wire_composition(composition: &wire::ImeComposition) -> Result<ImeEvent, String> {
        check_text(&composition.text)?;
        let units = utf16_len(&composition.text);
        if composition.cursor as usize > units {
            return Err(format!(
                "the cursor is at {}, past the end of a composition {units} UTF-16 code units \
                 long",
                composition.cursor
            ));
        }
        Ok(ImeEvent::Composition {
            task_id: composition.task_id,
            text: composition.text.clone(),
            cursor: composition.cursor,
        })
    }

    /// The commit the local API carried, or a reason it makes no sense: no text, or more
    /// than a message may carry.
    pub fn from_wire_commit(commit: &wire::ImeCommit) -> Result<ImeEvent, String> {
        if commit.text.is_empty() {
            return Err("the commit carries no text".into());
        }
        check_text(&commit.text)?;
        Ok(ImeEvent::Commit {
            task_id: commit.task_id,
            text: commit.text.clone(),
        })
    }
}

/// Whether `c` is a character an editor shows, rather than a control a key event carries
/// (Enter's carriage return, Tab, Backspace, Escape, and Ctrl with a letter).
fn is_typed(c: char) -> bool {
    !c.is_control()
}

/// One task window's text input: what its input method composes and commits, and the
/// characters its keys type.
#[derive(Debug)]
pub struct TextInput {
    task_id: i32,
    /// The composition the guest was last told of, with its cursor; empty when there is none.
    composing: (String, u32),
    /// The first half of a character beyond the Basic Multilingual Plane, which a window
    /// system that hands over one UTF-16 code unit at a time sends before the second.
    high_surrogate: Option<u16>,
}

impl TextInput {
    pub fn new(task_id: i32) -> TextInput {
        TextInput {
            task_id,
            composing: (String::new(), 0),
            high_surrogate: None,
        }
    }

    /// Whether the guest's editor holds a composition the window started.
    pub fn is_composing(&self) -> bool {
        !self.composing.0.is_empty()
    }

    /// The input method's composition is now `text`, with the cursor `cursor` UTF-16 code
    /// units from its start (past the end means at the end). `None` when the guest already
    /// has exactly that, or when it is empty and there was none.
    pub fn composition(&mut self, text: &str, cursor: u32) -> Option<ImeEvent> {
        let text = bounded(text);
        let cursor = cursor.min(utf16_len(&text) as u32);
        if self.composing.0 == text && (text.is_empty() || self.composing.1 == cursor) {
            return None;
        }
        self.composing = (text.clone(), cursor);
        Some(ImeEvent::Composition {
            task_id: self.task_id,
            text,
            cursor,
        })
    }

    /// The input method committed `text`. An empty commit only ends the composition, which
    /// it removes.
    pub fn commit(&mut self, text: &str) -> Option<ImeEvent> {
        if text.is_empty() {
            return self.end();
        }
        self.composing = (String::new(), 0);
        Some(ImeEvent::Commit {
            task_id: self.task_id,
            text: bounded(text),
        })
    }

    /// The input method ended its composition without committing it: the composition goes.
    pub fn end(&mut self) -> Option<ImeEvent> {
        self.composition("", 0)
    }

    /// A key typed `unit`, one UTF-16 code unit, without the input method composing it. A
    /// control character is dropped: the key that typed it went to the guest as a key event.
    /// The halves of a character beyond the Basic Multilingual Plane come one after the
    /// other, and the character is typed with the second.
    pub fn character(&mut self, unit: u16) -> Option<ImeEvent> {
        let high = self.high_surrogate.take();
        let c = match unit {
            0xd800..=0xdbff => {
                self.high_surrogate = Some(unit);
                return None;
            }
            0xdc00..=0xdfff => {
                let high = high?;
                char::decode_utf16([high, unit]).next()?.ok()?
            }
            _ => char::from_u32(u32::from(unit))?,
        };
        if !is_typed(c) {
            return None;
        }
        self.commit(c.encode_utf8(&mut [0; 4]))
    }
}

/// Where the cursor of the editor focused in a task is, in the task's own pixels from its top
/// left corner, which its window shows one to one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ImeCursor {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr6_each_change_to_the_composition_replaces_it_whole_with_its_cursor() {
        let mut input = TextInput::new(7);
        // Typing 한 jamo by jamo: ㅎ, 하, 한.
        let steps: Vec<ImeEvent> = ["\u{314e}", "\u{d558}", "\u{d55c}"]
            .iter()
            .filter_map(|text| input.composition(text, 1))
            .collect();
        assert_eq!(
            steps,
            vec![
                ImeEvent::Composition {
                    task_id: 7,
                    text: "\u{314e}".into(),
                    cursor: 1
                },
                ImeEvent::Composition {
                    task_id: 7,
                    text: "\u{d558}".into(),
                    cursor: 1
                },
                ImeEvent::Composition {
                    task_id: 7,
                    text: "\u{d55c}".into(),
                    cursor: 1
                },
            ]
        );
        assert!(input.is_composing());
    }

    #[test]
    fn fr6_backspace_through_the_jamo_shrinks_then_removes_the_composition() {
        let mut input = TextInput::new(7);
        input.composition("\u{d55c}", 1);
        assert_eq!(
            input.composition("\u{d558}", 1),
            Some(ImeEvent::Composition {
                task_id: 7,
                text: "\u{d558}".into(),
                cursor: 1
            })
        );
        input.composition("\u{314e}", 1);
        assert_eq!(
            input.composition("", 0),
            Some(ImeEvent::Composition {
                task_id: 7,
                text: String::new(),
                cursor: 0
            })
        );
        assert!(!input.is_composing());
        // Nothing to remove a second time.
        assert_eq!(input.composition("", 0), None);
        assert_eq!(input.end(), None);
    }

    #[test]
    fn fr6_the_same_composition_twice_is_sent_once() {
        let mut input = TextInput::new(1);
        assert!(input.composition("\u{d55c}", 1).is_some());
        assert_eq!(input.composition("\u{d55c}", 1), None);
        // The cursor moving within it is news.
        assert!(input.composition("\u{d55c}", 0).is_some());
    }

    #[test]
    fn fr6_a_cursor_past_the_end_of_the_composition_is_at_its_end() {
        let mut input = TextInput::new(1);
        assert_eq!(
            input.composition("ab", 9),
            Some(ImeEvent::Composition {
                task_id: 1,
                text: "ab".into(),
                cursor: 2
            })
        );
    }

    #[test]
    fn fr6_a_commit_takes_the_place_of_the_composition_which_ends() {
        let mut input = TextInput::new(3);
        input.composition("\u{d55c}", 1);
        assert_eq!(
            input.commit("\u{d55c}"),
            Some(ImeEvent::Commit {
                task_id: 3,
                text: "\u{d55c}".into()
            })
        );
        assert!(!input.is_composing());
        // The next syllable starts a new composition.
        assert!(input.composition("\u{3131}", 1).is_some());
    }

    #[test]
    fn fr6_a_composition_ended_without_a_commit_is_removed() {
        let mut input = TextInput::new(3);
        input.composition("\u{d55c}", 1);
        assert_eq!(
            input.end(),
            Some(ImeEvent::Composition {
                task_id: 3,
                text: String::new(),
                cursor: 0
            })
        );
        assert_eq!(input.commit(""), None);
    }

    #[test]
    fn fr6_a_typed_character_is_committed_and_a_control_character_is_left_to_its_key() {
        let mut input = TextInput::new(2);
        assert_eq!(
            input.character(u16::from(b'a')),
            Some(ImeEvent::Commit {
                task_id: 2,
                text: "a".into()
            })
        );
        // A German layout's ß, a French layout's é: whatever the layout typed.
        assert_eq!(
            input.character(0xdf),
            Some(ImeEvent::Commit {
                task_id: 2,
                text: "\u{df}".into()
            })
        );
        // Enter, Tab, Backspace, Escape, Ctrl+A and Delete went as key events.
        for control in [0x0d, 0x09, 0x08, 0x1b, 0x01, 0x7f] {
            assert_eq!(input.character(control), None, "{control:#x}");
        }
    }

    #[test]
    fn fr6_a_character_beyond_the_basic_plane_is_typed_from_its_two_halves() {
        let mut input = TextInput::new(2);
        // U+1F600, as two WM_CHAR code units.
        assert_eq!(input.character(0xd83d), None);
        assert_eq!(
            input.character(0xde00),
            Some(ImeEvent::Commit {
                task_id: 2,
                text: "\u{1f600}".into()
            })
        );
        // A second half alone is no character.
        assert_eq!(input.character(0xde00), None);
        // A first half followed by something else is dropped, and the other is typed.
        assert_eq!(input.character(0xd83d), None);
        assert_eq!(
            input.character(u16::from(b'x')),
            Some(ImeEvent::Commit {
                task_id: 2,
                text: "x".into()
            })
        );
    }

    #[test]
    fn fr6_text_longer_than_a_message_is_cut_on_a_character_boundary() {
        let mut input = TextInput::new(1);
        let long = "\u{1f600}".repeat(MAX_TEXT_UNITS);
        let Some(ImeEvent::Commit { text, .. }) = input.commit(&long) else {
            panic!("no commit");
        };
        assert_eq!(utf16_len(&text), MAX_TEXT_UNITS);
        assert!(text.chars().all(|c| c == '\u{1f600}'));
    }

    #[test]
    fn fr6_the_local_api_carries_text_input_unchanged() {
        let events = [
            ImeEvent::Composition {
                task_id: 4,
                text: "\u{d55c}".into(),
                cursor: 1,
            },
            ImeEvent::Commit {
                task_id: 4,
                text: "\u{d55c}\u{ae00}".into(),
            },
        ];
        for event in events {
            let back = match event.to_wire() {
                wire::envelope::Body::ImeComposition(c) => ImeEvent::from_wire_composition(&c),
                wire::envelope::Body::ImeCommit(c) => ImeEvent::from_wire_commit(&c),
                other => panic!("{other:?}"),
            };
            assert_eq!(back, Ok(event));
        }
    }

    #[test]
    fn fr6_text_input_that_makes_no_sense_is_refused() {
        assert!(ImeEvent::from_wire_composition(&wire::ImeComposition {
            task_id: 1,
            text: "ab".into(),
            cursor: 3,
        })
        .is_err());
        assert!(ImeEvent::from_wire_commit(&wire::ImeCommit {
            task_id: 1,
            text: String::new(),
        })
        .is_err());
        assert!(ImeEvent::from_wire_commit(&wire::ImeCommit {
            task_id: 1,
            text: "a".repeat(MAX_TEXT_UNITS + 1),
        })
        .is_err());
        // A removal is a composition with no text.
        assert!(ImeEvent::from_wire_composition(&wire::ImeComposition {
            task_id: 1,
            text: String::new(),
            cursor: 0,
        })
        .is_ok());
    }
}

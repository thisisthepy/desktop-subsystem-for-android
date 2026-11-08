//! An input method's text (text-input-v3) as compositions and commits for the guest's
//! editor.
//!
//! The protocol sends a batch of events and then `done`: the text to delete around the
//! cursor, the text to commit, and the new preedit (the composition) with its cursor in
//! bytes. Only `done` applies the batch, in that order. The guest's editor receives the
//! composition whole each time, with its cursor in UTF-16 code units, as the Windows input
//! method's compositions reach it, so Hangul, kana and pinyin compose in the guest exactly as
//! they do there.

//! The agent describes the focused editor ([`ImeEditor`]), and the window passes it on as
//! text-input-v3 state: the surrounding text without the composition, with the cursor and
//! the anchor in bytes ([`surrounding`]), and the content hint and purpose of its kind
//! ([`content_type`]). The input method reads the text before the cursor to predict and
//! reconverts by deleting around the cursor and composing anew; those deletions are counted
//! against the surrounding text the window sent, so they reach the guest in UTF-16 code
//! units exactly.

use dsa_host_core::ime::{EditorText, ImeEditor, ImeEvent, InputKind, TextInput};

/// The most surrounding text text-input-v3 carries, in bytes: it must stay below 4000.
pub const MAX_SURROUNDING_BYTES: usize = 3999;

/// The surrounding text of the focused editor as text-input-v3 takes it: without the
/// composition, which the input method holds as its preedit, and with the cursor (the
/// selection's active end) and the anchor as byte offsets into `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surrounding {
    pub text: String,
    pub cursor: i32,
    pub anchor: i32,
}

/// The surrounding text of `editor`, cut to [`MAX_SURROUNDING_BYTES`] around the selection.
/// `None` for an editor that takes no text, or a selection too long to carry with its text.
pub fn surrounding(editor: &ImeEditor) -> Option<Surrounding> {
    if !editor.kind.takes_text() {
        return None;
    }
    let text = EditorText::of(editor);
    let len = text.len();
    let local = |at: u32| at.saturating_sub(editor.text_start).min(len);
    let (mut anchor, mut cursor) = (local(editor.selection.0), local(editor.selection.1));
    let mut units = text.units.clone();
    if let Some((from, to)) = text.composition {
        units.drain(from as usize..to as usize);
        let moved = |at: u32| {
            if at >= to {
                at - (to - from)
            } else {
                at.min(from)
            }
        };
        anchor = moved(anchor);
        cursor = moved(cursor);
    }
    let (low, high) = (anchor.min(cursor) as usize, anchor.max(cursor) as usize);
    let before = String::from_utf16_lossy(&units[..low]);
    let selected = String::from_utf16_lossy(&units[low..high]);
    let after = String::from_utf16_lossy(&units[high..]);
    if selected.len() > MAX_SURROUNDING_BYTES {
        return None;
    }
    // What is left goes half before the selection and the rest after it, in whole
    // characters.
    let room = MAX_SURROUNDING_BYTES - selected.len();
    let mut kept_before = 0;
    let before_start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            kept_before += c.len_utf8();
            kept_before <= room / 2
        })
        .last()
        .map_or(before.len(), |(at, _)| at);
    let before = &before[before_start..];
    let room = room - before.len();
    let mut kept_after = 0;
    let after: String = after
        .chars()
        .take_while(|c| {
            kept_after += c.len_utf8();
            kept_after <= room
        })
        .collect();
    let low_byte = before.len() as i32;
    let high_byte = low_byte + selected.len() as i32;
    let (anchor, cursor) = if anchor <= cursor {
        (low_byte, high_byte)
    } else {
        (high_byte, low_byte)
    };
    Some(Surrounding {
        text: format!("{before}{selected}{after}"),
        cursor,
        anchor,
    })
}

/// text-input-v3's content hints, as its protocol numbers them. They are numbers here so the
/// rules build and run on every host; the seat turns them into the protocol's own types.
pub mod hint {
    pub const COMPLETION: u32 = 0x1;
    pub const SPELLCHECK: u32 = 0x2;
    pub const AUTO_CAPITALIZATION: u32 = 0x4;
    pub const HIDDEN_TEXT: u32 = 0x40;
    pub const SENSITIVE_DATA: u32 = 0x80;
}

/// text-input-v3's content purposes, as its protocol numbers them.
pub mod purpose {
    pub const NORMAL: u32 = 0;
    pub const NUMBER: u32 = 3;
    pub const PHONE: u32 = 4;
    pub const URL: u32 = 5;
    pub const EMAIL: u32 = 6;
    pub const NAME: u32 = 7;
    pub const PASSWORD: u32 = 8;
    pub const PIN: u32 = 9;
    pub const DATE: u32 = 10;
    pub const TIME: u32 = 11;
    pub const TERMINAL: u32 = 13;
}

/// The content hint and purpose of a kind of text, which the input method picks its layout,
/// its prediction and its learning by.
pub fn content_type(kind: InputKind) -> (u32, u32) {
    let secret = hint::HIDDEN_TEXT | hint::SENSITIVE_DATA;
    match kind {
        InputKind::None => (0, purpose::TERMINAL),
        InputKind::Text => (hint::COMPLETION | hint::SPELLCHECK, purpose::NORMAL),
        InputKind::Search => (hint::COMPLETION, purpose::NORMAL),
        InputKind::Password => (secret, purpose::PASSWORD),
        InputKind::NumberPassword => (secret, purpose::PIN),
        InputKind::Number => (0, purpose::NUMBER),
        InputKind::Phone => (0, purpose::PHONE),
        InputKind::Email => (0, purpose::EMAIL),
        InputKind::Url => (0, purpose::URL),
        InputKind::PersonName => (hint::AUTO_CAPITALIZATION, purpose::NAME),
        InputKind::Date => (0, purpose::DATE),
        InputKind::Time => (0, purpose::TIME),
    }
}

/// The UTF-16 length of `bytes` bytes of `text` ending at byte `end` (before) or starting
/// there (after); `None` when they do not fall on character boundaries of it.
fn units_around(text: &str, end: usize, bytes: usize, before: bool) -> Option<u32> {
    let (from, to) = if before {
        (end.checked_sub(bytes)?, end)
    } else {
        (end, end.checked_add(bytes)?)
    };
    let part = text.get(from..to)?;
    Some(part.encode_utf16().count() as u32)
}

/// One batch of text-input-v3 events, waiting for its `done`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Pending {
    preedit: Option<(String, i32)>,
    commit: Option<String>,
    delete: Option<(u32, u32)>,
}

/// The input method's state for one task window.
#[derive(Debug)]
pub struct TextInputV3 {
    task_id: i32,
    input: TextInput,
    /// The text typed last through this window, to measure deletions against.
    recent: String,
    pending: Pending,
    /// What the guest last said of its focused editor.
    editor: Option<ImeEditor>,
    /// The surrounding text last sent to the input method, which its deletions count in.
    sent: Option<Surrounding>,
}

/// The UTF-16 length of the first `bytes` bytes of `text`; the whole text when `bytes` is
/// negative (the protocol's "hide the cursor") or past its end, or not on a character
/// boundary.
pub fn utf16_cursor(text: &str, bytes: i32) -> u32 {
    let end = match usize::try_from(bytes) {
        Ok(bytes) if bytes <= text.len() && text.is_char_boundary(bytes) => bytes,
        _ => text.len(),
    };
    text[..end].encode_utf16().count() as u32
}

/// The UTF-16 length of the last `bytes` UTF-8 bytes of `recent`; `bytes` itself when they
/// do not end on a character boundary of it or reach past it.
pub fn recent_units(recent: &str, bytes: u32) -> u32 {
    let bytes = bytes as usize;
    match recent.len().checked_sub(bytes) {
        Some(start) if recent.is_char_boundary(start) => {
            recent[start..].encode_utf16().count() as u32
        }
        _ => bytes as u32,
    }
}

impl TextInputV3 {
    pub fn new(task_id: i32) -> TextInputV3 {
        TextInputV3 {
            task_id,
            input: TextInput::new(task_id),
            recent: String::new(),
            pending: Pending::default(),
            editor: None,
            sent: None,
        }
    }

    /// `preedit_string`: the composition is `text` (none when `None`), with the cursor
    /// starting `cursor_begin` bytes in.
    pub fn preedit(&mut self, text: Option<String>, cursor_begin: i32) {
        self.pending.preedit = text.map(|text| (text, cursor_begin));
    }

    /// `commit_string`: `text` is committed.
    pub fn commit(&mut self, text: Option<String>) {
        self.pending.commit = text;
    }

    /// The guest described its focused editor.
    pub fn describe(&mut self, editor: ImeEditor) {
        self.editor = Some(editor);
    }

    /// What the guest last said of its focused editor.
    pub fn editor(&self) -> Option<&ImeEditor> {
        self.editor.as_ref()
    }

    /// The surrounding text to send the input method now, remembered so its deletions are
    /// counted in it.
    pub fn surrounding(&mut self) -> Option<Surrounding> {
        self.sent = self.editor.as_ref().and_then(surrounding);
        self.sent.clone()
    }

    /// `delete_surrounding_text`: `before` and `after` UTF-8 bytes around the cursor go. The
    /// guest counts UTF-16 code units: the bytes are counted in the surrounding text sent
    /// last, from the selection's ends. Without it, the bytes before the cursor are counted
    /// against what this window typed last, as an input method deletes what it has just
    /// typed; anything else passes on as it is, which is exact for ASCII.
    pub fn delete_surrounding(&mut self, before: u32, after: u32) {
        let counted = self.sent.as_ref().and_then(|sent| {
            let low = sent.cursor.min(sent.anchor) as usize;
            let high = sent.cursor.max(sent.anchor) as usize;
            Some((
                units_around(&sent.text, low, before as usize, true)?,
                units_around(&sent.text, high, after as usize, false)?,
            ))
        });
        let (before, after) =
            counted.unwrap_or_else(|| (recent_units(&self.recent, before), after));
        self.pending.delete = Some((before, after));
    }

    /// `done`: applies the batch, and says what the guest's editor receives, in order.
    pub fn done(&mut self) -> Vec<ImeEvent> {
        let batch = std::mem::take(&mut self.pending);
        let mut events = Vec::new();
        if let Some((before, after)) = batch.delete.filter(|(b, a)| *b > 0 || *a > 0) {
            events.push(ImeEvent::DeleteSurrounding {
                task_id: self.task_id,
                before,
                after,
            });
        }
        if let Some((before, _)) = batch.delete {
            let keep = self.recent.chars().count().saturating_sub(before as usize);
            self.recent = self.recent.chars().take(keep).collect();
        }
        if let Some(text) = batch.commit.filter(|text| !text.is_empty()) {
            self.remember(&text);
            events.extend(self.input.commit(&text));
        }
        match batch.preedit.filter(|(text, _)| !text.is_empty()) {
            Some((text, cursor)) => {
                let cursor = utf16_cursor(&text, cursor);
                events.extend(self.input.composition(&text, cursor));
            }
            None => events.extend(self.input.end()),
        }
        events
    }

    /// The window lost the keyboard, or the input method was turned off: a composition in
    /// progress goes, as on Windows.
    pub fn leave(&mut self) -> Vec<ImeEvent> {
        self.pending = Pending::default();
        self.input.end().into_iter().collect()
    }

    /// A key typed `text` with no input method composing it.
    pub fn typed(&mut self, text: &str) -> Vec<ImeEvent> {
        self.remember(text);
        self.input.commit(text).into_iter().collect()
    }

    fn remember(&mut self, text: &str) {
        const KEEP: usize = 256;
        self.recent.push_str(text);
        let extra = self.recent.chars().count().saturating_sub(KEEP);
        if extra > 0 {
            self.recent = self.recent.chars().skip(extra).collect();
        }
    }

    /// Whether the guest's editor holds a composition.
    pub fn is_composing(&self) -> bool {
        self.input.is_composing()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composition(text: &str, cursor: u32) -> ImeEvent {
        ImeEvent::Composition {
            task_id: 4,
            text: text.into(),
            cursor,
        }
    }

    fn commit(text: &str) -> ImeEvent {
        ImeEvent::Commit {
            task_id: 4,
            text: text.into(),
        }
    }

    #[test]
    fn fr6_hangul_composes_jamo_by_jamo_and_commits_on_the_next_syllable() {
        let mut ime = TextInputV3::new(4);
        ime.preedit(Some("\u{314e}".into()), 3);
        assert_eq!(ime.done(), vec![composition("\u{314e}", 1)]);
        ime.preedit(Some("\u{d55c}".into()), 3);
        assert_eq!(ime.done(), vec![composition("\u{d55c}", 1)]);
        // Typing the next syllable's first jamo commits 한 and starts a new composition.
        ime.commit(Some("\u{d55c}".into()));
        ime.preedit(Some("\u{3131}".into()), 3);
        assert_eq!(
            ime.done(),
            vec![commit("\u{d55c}"), composition("\u{3131}", 1)]
        );
        assert!(ime.is_composing());
    }

    #[test]
    fn fr6_a_commit_without_a_preedit_ends_the_composition() {
        let mut ime = TextInputV3::new(4);
        ime.preedit(Some("ni".into()), 2);
        ime.done();
        ime.commit(Some("\u{4f60}".into()));
        assert_eq!(ime.done(), vec![commit("\u{4f60}")]);
        assert!(!ime.is_composing());
        assert_eq!(ime.done(), vec![], "nothing left to end");
    }

    #[test]
    fn fr6_an_empty_preedit_clears_the_composition() {
        let mut ime = TextInputV3::new(4);
        ime.preedit(Some("ka".into()), 2);
        ime.done();
        ime.preedit(None, 0);
        assert_eq!(ime.done(), vec![composition("", 0)]);
    }

    #[test]
    fn fr6_the_cursor_is_counted_in_utf16_code_units() {
        assert_eq!(utf16_cursor("\u{d55c}a", 3), 1);
        assert_eq!(utf16_cursor("\u{1f600}x", 4), 2, "an emoji is two units");
        assert_eq!(
            utf16_cursor("abc", -1),
            3,
            "a hidden cursor stands at the end"
        );
        assert_eq!(
            utf16_cursor("\u{d55c}", 1),
            1,
            "inside a character means the end"
        );
    }

    #[test]
    fn fr6_an_input_method_deletes_around_the_cursor_before_it_commits() {
        let mut ime = TextInputV3::new(4);
        ime.typed("a\u{d55c}");
        // The input method takes back the three bytes of 한: one UTF-16 code unit.
        ime.delete_surrounding(3, 0);
        ime.commit(Some("\u{d55c}".into()));
        assert_eq!(
            ime.done(),
            vec![
                ImeEvent::DeleteSurrounding {
                    task_id: 4,
                    before: 1,
                    after: 0
                },
                commit("\u{d55c}")
            ]
        );
    }

    #[test]
    fn fr6_losing_the_keyboard_drops_the_composition() {
        let mut ime = TextInputV3::new(4);
        ime.preedit(Some("ka".into()), 2);
        ime.done();
        assert_eq!(ime.leave(), vec![composition("", 0)]);
    }
    fn editor(kind: InputKind, text: &str, start: u32, selection: (u32, u32)) -> ImeEditor {
        ImeEditor {
            kind,
            text: text.into(),
            text_start: start,
            selection,
            composition: None,
        }
    }

    #[test]
    fn fr6_the_surrounding_text_leaves_out_the_composition_and_counts_in_bytes() {
        // "한글 ab", with "ab" being composed and the cursor after it.
        let described = ImeEditor {
            composition: Some((103, 105)),
            ..editor(InputKind::Text, "\u{d55c}\u{ae00} ab", 100, (105, 105))
        };
        assert_eq!(
            surrounding(&described),
            Some(Surrounding {
                text: "\u{d55c}\u{ae00} ".into(),
                cursor: 7,
                anchor: 7,
            })
        );
        // Selected backwards over 글: the cursor is at its start.
        let selected = editor(InputKind::Text, "\u{d55c}\u{ae00}", 0, (2, 1));
        assert_eq!(
            surrounding(&selected),
            Some(Surrounding {
                text: "\u{d55c}\u{ae00}".into(),
                cursor: 3,
                anchor: 6,
            })
        );
        assert_eq!(surrounding(&ImeEditor::none()), None);
    }

    #[test]
    fn fr6_the_surrounding_text_stays_below_what_text_input_v3_carries() {
        let long = "\u{d55c}".repeat(1300);
        let described = editor(InputKind::Text, &long, 0, (650, 650));
        let sent = surrounding(&described).unwrap();
        assert!(sent.text.len() <= MAX_SURROUNDING_BYTES);
        assert_eq!(sent.cursor, sent.anchor);
        assert!(sent.text.is_char_boundary(sent.cursor as usize));
        assert!(sent.cursor > 0 && (sent.cursor as usize) < sent.text.len());
    }

    #[test]
    fn fr6_each_input_kind_has_the_content_type_the_input_method_reads() {
        assert_eq!(
            content_type(InputKind::Password),
            (hint::HIDDEN_TEXT | hint::SENSITIVE_DATA, purpose::PASSWORD)
        );
        assert_eq!(content_type(InputKind::NumberPassword).1, purpose::PIN);
        assert_eq!(content_type(InputKind::Number).1, purpose::NUMBER);
        assert_eq!(content_type(InputKind::Phone).1, purpose::PHONE);
        assert_eq!(content_type(InputKind::Email).1, purpose::EMAIL);
        assert_eq!(content_type(InputKind::Url).1, purpose::URL);
        assert_eq!(content_type(InputKind::PersonName).1, purpose::NAME);
        assert_eq!(content_type(InputKind::Date).1, purpose::DATE);
        assert_eq!(content_type(InputKind::Time).1, purpose::TIME);
        assert_eq!(content_type(InputKind::Text).1, purpose::NORMAL);
        assert!(content_type(InputKind::Text).0 & hint::COMPLETION != 0);
    }

    #[test]
    fn fr6_a_deletion_is_counted_against_the_surrounding_text_the_window_sent() {
        let mut ime = TextInputV3::new(4);
        // An emoji is four bytes and two UTF-16 code units; 한 is three bytes and one unit.
        ime.describe(editor(InputKind::Text, "a\u{1f600}\u{d55c}", 0, (3, 3)));
        ime.surrounding();
        ime.delete_surrounding(4, 3);
        assert_eq!(
            ime.done(),
            vec![ImeEvent::DeleteSurrounding {
                task_id: 4,
                before: 2,
                after: 1
            }]
        );
    }
}

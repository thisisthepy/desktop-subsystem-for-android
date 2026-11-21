//! The document a task window's input method reads and edits: the text of the guest's focused
//! editor around its selection, as the text services framework sees it, and the events that
//! make the guest's editor follow each edit.
//!
//! The text services framework works on a document it can read and change at any offset: it
//! reads the text before the cursor to predict and to reconvert, sets the selection, starts a
//! composition over text already typed, and replaces any part of it. The guest's editor
//! changes only through its `InputConnection`, whose edits replace the composition, or the
//! selection when there is none. [`TextStore`] keeps both: the document the framework edits,
//! with the composition the framework has, and the guest's editor as the events sent so far
//! leave it ([`EditorText`]). After each of the framework's edits, [`TextStore::flush`] sends
//! the fewest events that make the guest's editor the document again: a commit of what the
//! composition finished, a composition of what it shows now, and the selection.
//!
//! The agent describes the editor again after every change ([`ImeEditor`]). A description of
//! a state the host's own events pass through on their way is stale and changes nothing, even
//! when it comes several edits late, and so is one of such a state whose editor lost its
//! composing span (input restarted on it); one the host's events never made (the user clicked
//! elsewhere in the editor, the app changed its text, another editor took the focus) becomes
//! the document, and the framework is told what changed.
//!
//! Offsets are UTF-16 code units, as both the framework and Android count them. Nothing here
//! calls Windows, so every rule is tested without an input method.

use std::collections::VecDeque;

use dsa_host_core::ime::{
    bounded, EditorText, ImeEditor, ImeEvent, InputKind, TextInput, EDITOR_REACH_UNITS,
    MAX_EDITOR_UNITS,
};
use windows::Win32::UI::TextServices::{
    InputScope, IS_DATE_FULLDATE, IS_DEFAULT, IS_EMAIL_SMTPEMAILADDRESS, IS_NUMBER,
    IS_NUMERIC_PASSWORD, IS_PASSWORD, IS_PERSONALNAME_FULLNAME, IS_SEARCH,
    IS_TELEPHONE_FULLTELEPHONENUMBER, IS_TIME_FULLTIME, IS_URL,
};

/// A change to the document's text, as the framework is told of it: what ran from `start` to
/// `old_end` now runs from `start` to `new_end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextChange {
    pub start: u32,
    pub old_end: u32,
    pub new_end: u32,
}

/// What the framework must be told after the guest described its editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Described {
    /// The document's text changed.
    pub text: Option<TextChange>,
    /// The selection moved.
    pub selection: bool,
    /// The editor takes another kind of text, so the input scope changed.
    pub kind: bool,
    /// The framework was composing when the guest's editor changed under it, so the
    /// composition must end: it no longer stands where the framework thinks.
    pub ends_composition: bool,
}

/// An offset past the end of the document, or a range that ends before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange;

/// How many of the states the host's events pass through are remembered, to tell a stale
/// description from a change in the guest. The events of one edit are a handful; this covers
/// many edits in flight at once.
const MAX_EXPECTED: usize = 64;

/// The most text the document keeps when the guest never describes its editor (an agent that
/// does not), so typing for hours does not grow it without end. What lies further before the
/// selection is dropped from the document; the guest's editor keeps it.
const MAX_DOCUMENT_UNITS: u32 = 2 * MAX_EDITOR_UNITS as u32;

/// The input scopes the touch keyboard and the input method read for a kind of text: the
/// keyboard's layout, and whether to compose, predict and learn.
pub fn input_scopes(kind: InputKind) -> &'static [InputScope] {
    match kind {
        InputKind::None | InputKind::Text => &[IS_DEFAULT],
        InputKind::Password => &[IS_PASSWORD],
        InputKind::Number => &[IS_NUMBER],
        InputKind::NumberPassword => &[IS_NUMERIC_PASSWORD],
        InputKind::Phone => &[IS_TELEPHONE_FULLTELEPHONENUMBER],
        InputKind::Email => &[IS_EMAIL_SMTPEMAILADDRESS],
        InputKind::Url => &[IS_URL],
        InputKind::PersonName => &[IS_PERSONALNAME_FULLNAME],
        InputKind::Date => &[IS_DATE_FULLDATE],
        InputKind::Time => &[IS_TIME_FULLTIME],
        InputKind::Search => &[IS_SEARCH],
    }
}

/// Where the two texts differ: from `prefix` to `old_end` in `old`, and from `prefix` to
/// `new_end` in `new`. `None` when they are the same. Never splits a surrogate pair, so what
/// is sent is always whole characters.
fn difference(old: &[u16], new: &[u16]) -> Option<(u32, u32, u32)> {
    dsa_host_core::text_edit::difference(
        old,
        new,
        dsa_host_core::text_edit::is_high_surrogate,
        dsa_host_core::text_edit::is_low_surrogate,
    )
    .map(|(prefix, old_end, new_end)| (prefix as u32, old_end as u32, new_end as u32))
}

fn text_of(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// One task window's document.
#[derive(Debug)]
pub struct TextStore {
    task_id: i32,
    kind: InputKind,
    /// The document the framework reads and edits, with the framework's composition.
    document: EditorText,
    /// Whether the selection's active end is its start: the user selected backwards.
    backwards: bool,
    /// The guest's editor once every event sent has reached it.
    guest: EditorText,
    /// The states the guest's editor passes through on the way to `guest`, oldest first.
    expected: VecDeque<EditorText>,
    /// The characters typed without the input method.
    typed: TextInput,
    /// What the guest last said of its editor.
    described: Option<ImeEditor>,
    /// Whether the guest described its editor as the host's events left it but without the
    /// composing span, so the next events set the span again first.
    reassert: bool,
}

impl TextStore {
    pub fn new(task_id: i32) -> TextStore {
        TextStore {
            task_id,
            kind: InputKind::Text,
            document: EditorText::default(),
            backwards: false,
            guest: EditorText::default(),
            expected: VecDeque::new(),
            typed: TextInput::new(task_id),
            described: None,
            reassert: false,
        }
    }

    /// The kind of text the focused editor takes.
    pub fn kind(&self) -> InputKind {
        self.kind
    }

    /// What the guest last said of its editor.
    pub fn described(&self) -> Option<&ImeEditor> {
        self.described.as_ref()
    }

    /// How long the document is.
    pub fn len(&self) -> u32 {
        self.document.len()
    }

    pub fn is_empty(&self) -> bool {
        self.document.is_empty()
    }

    /// The document's text from `start` to `end`, cut to the document.
    pub fn text(&self, start: u32, end: u32) -> &[u16] {
        let end = end.min(self.len());
        let start = start.min(end);
        &self.document.units[start as usize..end as usize]
    }

    /// The selection, start before end, and whether its active end is its start.
    pub fn selection(&self) -> (u32, u32, bool) {
        let (start, end) = self.document.selection;
        (start, end, self.backwards)
    }

    /// The framework's composition, if it is composing.
    pub fn composition(&self) -> Option<(u32, u32)> {
        self.document.composition
    }

    pub fn is_composing(&self) -> bool {
        self.document.composition.is_some()
    }

    fn check(&self, start: u32, end: u32) -> Result<(), OutOfRange> {
        if start > end || end > self.len() {
            return Err(OutOfRange);
        }
        Ok(())
    }

    /// The framework selects from `start` to `end`.
    pub fn set_selection(
        &mut self,
        start: u32,
        end: u32,
        backwards: bool,
    ) -> Result<(), OutOfRange> {
        self.check(start, end)?;
        self.document.selection = (start, end);
        self.backwards = backwards && start != end;
        Ok(())
    }

    /// The framework replaces the text from `start` to `end` with `text`, which it then
    /// selects. A composition the change falls within grows or shrinks with it.
    pub fn replace(
        &mut self,
        start: u32,
        end: u32,
        text: &[u16],
    ) -> Result<TextChange, OutOfRange> {
        self.check(start, end)?;
        let added = text.len() as u32;
        self.document
            .units
            .splice(start as usize..end as usize, text.iter().copied());
        let moved = |at: u32| -> u32 {
            if at >= end {
                at - (end - start) + added
            } else if at > start {
                start
            } else {
                at
            }
        };
        self.document.composition = self.document.composition.map(|(from, to)| {
            if start >= from && end <= to {
                // Within the composition: it holds the new text.
                (from, to - (end - start) + added)
            } else {
                (moved(from), moved(to).max(moved(from)))
            }
        });
        self.document.selection = (start, start + added);
        self.backwards = false;
        Ok(TextChange {
            start,
            old_end: end,
            new_end: start + added,
        })
    }

    /// The framework types `text` in place of the selection, as dictation, handwriting and
    /// the emoji panel do.
    pub fn insert_at_selection(&mut self, text: &[u16]) -> TextChange {
        let (start, end) = self.document.selection;
        // The selection always lies within the document.
        self.replace(start, end, text).unwrap_or(TextChange {
            start,
            old_end: end,
            new_end: end,
        })
    }

    /// The input method composed `text` outside the document, through IMM32, with its cursor
    /// `cursor` UTF-16 code units from the start of `text`: it takes the place of the
    /// composition, or of the selection when there is none, and is the composition now. An
    /// empty `text` removes the composition. What changed, for the framework to be told.
    pub fn compose_outside(&mut self, text: &[u16], cursor: u32) -> TextChange {
        let (from, to) = self.document.composition.unwrap_or(self.document.selection);
        let change = self.replace(from, to, text).unwrap_or(TextChange {
            start: from,
            old_end: from,
            new_end: from,
        });
        let len = text.len() as u32;
        self.document.composition = (len > 0).then_some((from, from + len));
        let at = from + cursor.min(len);
        self.document.selection = (at, at);
        change
    }

    /// The input method committed `text` outside the document, through IMM32: it takes the
    /// place of the composition, or of the selection, and the composition ends. What changed,
    /// for the framework to be told.
    pub fn commit_outside(&mut self, text: &[u16]) -> TextChange {
        let (from, to) = self.document.composition.unwrap_or(self.document.selection);
        let change = self.replace(from, to, text).unwrap_or(TextChange {
            start: from,
            old_end: from,
            new_end: from,
        });
        self.document.composition = None;
        let at = from + text.len() as u32;
        self.document.selection = (at, at);
        change
    }

    /// The framework's composition now runs from `start` to `end`, or it has none.
    pub fn set_composition(&mut self, composition: Option<(u32, u32)>) {
        let len = self.len();
        self.document.composition = composition.map(|(from, to)| {
            let (from, to) = (from.min(len), to.min(len));
            (from.min(to), from.max(to))
        });
    }

    /// Sends `event` to the guest: what its editor becomes, and the state it passes through.
    fn send(&mut self, event: ImeEvent, sent: &mut Vec<ImeEvent>) {
        self.guest.apply(&event);
        self.expected.push_back(self.guest.clone());
        while self.expected.len() > MAX_EXPECTED {
            self.expected.pop_front();
        }
        sent.push(event);
    }

    fn absolute(&self, at: u32) -> u32 {
        self.guest.start.saturating_add(at)
    }

    /// Makes the guest's editor replace `from..to` next: its composition, or the cursor at
    /// `from` with no composition when the range is empty.
    fn aim(&mut self, from: u32, to: u32, sent: &mut Vec<ImeEvent>) {
        if to > from {
            if self.guest.composition != Some((from, to)) {
                let event = ImeEvent::SetComposingRegion {
                    task_id: self.task_id,
                    start: self.absolute(from),
                    end: self.absolute(to),
                };
                self.send(event, sent);
            }
            return;
        }
        if let Some((start, end)) = self.guest.composition {
            // The composition ends with its text as it is.
            let unchanged = self.guest.units.get(start as usize..end as usize)
                == self.document.units.get(start as usize..end as usize);
            let event = if unchanged && end <= from {
                ImeEvent::Commit {
                    task_id: self.task_id,
                    text: text_of(&self.guest.units[start as usize..end as usize]),
                }
            } else {
                ImeEvent::SetComposingRegion {
                    task_id: self.task_id,
                    start: self.absolute(from),
                    end: self.absolute(from),
                }
            };
            self.send(event, sent);
        }
        self.select(from, from, sent);
    }

    fn select(&mut self, start: u32, end: u32, sent: &mut Vec<ImeEvent>) {
        if self.guest.selection != (start, end) {
            let event = ImeEvent::SetSelection {
                task_id: self.task_id,
                start: self.absolute(start),
                end: self.absolute(end),
            };
            self.send(event, sent);
        }
    }

    /// The events that make the guest's editor what the document is now, after an edit of
    /// the framework's. Empty when it already is.
    pub fn flush(&mut self) -> Vec<ImeEvent> {
        let mut sent = Vec::new();
        // A composition the framework has started but put nothing in yet shows nothing in
        // the guest's editor.
        let mut document = self.document.clone();
        document.composition = document.composition.filter(|(start, end)| start < end);
        if self.guest == document {
            return sent;
        }
        if std::mem::take(&mut self.reassert) {
            // The guest's editor may have lost its composing span: it is set again, so what
            // follows replaces the composition rather than typing beside it.
            if let Some((from, to)) = self.guest.composition {
                let event = ImeEvent::SetComposingRegion {
                    task_id: self.task_id,
                    start: self.absolute(from),
                    end: self.absolute(to),
                };
                self.send(event, &mut sent);
            }
        }
        let change = difference(&self.guest.units, &document.units);
        // A place in the document as the place in the guest's text it stands for: the
        // changed part maps to its own ends.
        let to_guest = |at: u32, end: bool| match change {
            None => at,
            Some((prefix, _, _)) if at <= prefix => at,
            Some((_, old_end, new_end)) if at >= new_end => at - new_end + old_end,
            Some((prefix, old_end, _)) => {
                if end {
                    old_end
                } else {
                    prefix
                }
            }
        };
        let to_document = |at: u32| match change {
            Some((_, old_end, new_end)) if at >= old_end => at - old_end + new_end,
            _ => at,
        };
        // The part of the guest's text that is written again: what changed, with any
        // composition there was or is now.
        let mut region: Option<(u32, u32)> = None;
        let mut include = |from: u32, to: u32| {
            region = Some(match region {
                Some((a, b)) => (a.min(from), b.max(to)),
                None => (from, to),
            });
        };
        if let Some((prefix, old_end, _)) = change {
            include(prefix, old_end);
        }
        if let Some((from, to)) = self.guest.composition {
            include(from, to);
        }
        if let Some((from, to)) = document.composition {
            include(to_guest(from, false), to_guest(to, true));
        }
        if let Some((from, to)) = region {
            let written = to_document(to);
            match document.composition {
                Some((start, end)) => {
                    self.aim(from, to, &mut sent);
                    let mut around = document.units[from as usize..start as usize].to_vec();
                    around.extend_from_slice(&document.units[end as usize..written as usize]);
                    if !around.is_empty() {
                        let event = ImeEvent::Commit {
                            task_id: self.task_id,
                            text: bounded(&text_of(&around)),
                        };
                        self.send(event, &mut sent);
                        self.select(start, start, &mut sent);
                    }
                    let composing = &document.units[start as usize..end as usize];
                    let nothing_to_remove = self.guest.composition.is_none()
                        && self.guest.selection.0 == self.guest.selection.1;
                    if !(composing.is_empty() && nothing_to_remove) {
                        let cursor = document.selection.1.clamp(start, end) - start;
                        let event = ImeEvent::Composition {
                            task_id: self.task_id,
                            text: bounded(&text_of(composing)),
                            cursor,
                        };
                        self.send(event, &mut sent);
                    }
                }
                None => {
                    let new = document.units[from as usize..written as usize].to_vec();
                    if to > from || !new.is_empty() {
                        self.aim(from, to, &mut sent);
                        let event = if new.is_empty() {
                            ImeEvent::Composition {
                                task_id: self.task_id,
                                text: String::new(),
                                cursor: 0,
                            }
                        } else {
                            ImeEvent::Commit {
                                task_id: self.task_id,
                                text: bounded(&text_of(&new)),
                            }
                        };
                        self.send(event, &mut sent);
                    }
                }
            }
        }
        let (start, end) = document.selection;
        self.select(start, end, &mut sent);
        if self.guest.composition != document.composition {
            let (start, end) = document
                .composition
                .unwrap_or((document.selection.1, document.selection.1));
            let event = ImeEvent::SetComposingRegion {
                task_id: self.task_id,
                start: self.absolute(start),
                end: self.absolute(end),
            };
            self.send(event, &mut sent);
        }
        // Only text longer than a message carries leaves the guest's editor short of the
        // document; the framework keeps its document, and the next edit sends the rest.
        sent
    }

    /// A key typed `unit` without the input method. The events to send, and what changed in
    /// the document for the framework to be told.
    pub fn typed(&mut self, unit: u16) -> (Vec<ImeEvent>, Option<TextChange>) {
        let Some(event) = self.typed.character(unit) else {
            return (Vec::new(), None);
        };
        if !self.kind.takes_text() {
            // It goes as the keys that type it, and no editor holds it.
            return (vec![event], None);
        }
        let before = self.document.units.clone();
        self.document.apply(&event);
        self.backwards = false;
        let mut sent = Vec::new();
        self.send(event, &mut sent);
        self.guest = self.document.clone();
        let change =
            difference(&before, &self.document.units).map(|(start, old_end, new_end)| TextChange {
                start,
                old_end,
                new_end,
            });
        (sent, change)
    }

    /// The guest described its editor. What the framework must be told of it.
    pub fn described_as(&mut self, editor: ImeEditor) -> Described {
        let mut told = Described {
            kind: editor.kind != self.kind,
            ..Described::default()
        };
        self.kind = editor.kind;
        let seen = EditorText::of(&editor);
        self.described = Some(editor);
        if seen.agrees_with(&self.guest) {
            // Where the host's own events lead, perhaps through a window that moved. While
            // the framework composes, its document stays as it is.
            if seen == self.guest || self.is_composing() {
                return told;
            }
        } else if self.expected.iter().any(|state| seen.agrees_with(state)) {
            // A state the host's events pass through: the rest is on its way. Descriptions
            // come late, several edits behind, and any of them may come after a later one
            // was already seen, so every state still remembered counts.
            return told;
        } else if seen.echoes(&self.guest) {
            // The guest's editor holds what the host's events made, without its composing
            // span: input restarted on it, or its app set the same text again. That is the
            // host's own text, so the composition goes on. The agent makes the text the
            // composition again as input restarts, but whether it could is not known here,
            // so the next edit says where the composition is before it replaces it.
            self.reassert = self.guest.composition.is_some();
            return told;
        } else if self.expected.iter().any(|state| seen.echoes(state)) {
            // The same, for a state the host's events passed through on the way.
            return told;
        }
        told.text = if seen.start == self.document.start {
            difference(&self.document.units, &seen.units).map(|(start, old_end, new_end)| {
                TextChange {
                    start,
                    old_end,
                    new_end,
                }
            })
        } else {
            Some(TextChange {
                start: 0,
                old_end: self.len(),
                new_end: seen.len(),
            })
        };
        told.selection =
            seen.start != self.document.start || seen.selection != self.document.selection;
        told.ends_composition = self.is_composing() && (told.text.is_some() || told.selection);
        // A composition is the framework's own: one the guest's editor still shows ends with
        // its text as typed at the framework's next edit.
        self.document = EditorText {
            composition: None,
            ..seen.clone()
        };
        self.backwards = editor_backwards(self.described.as_ref());
        self.guest = seen;
        self.expected.clear();
        self.reassert = false;
        told
    }

    /// Drops text far before the selection from a document that has grown past
    /// [`MAX_DOCUMENT_UNITS`], which only one the guest never describes does. The change, for
    /// the framework to be told.
    pub fn trim(&mut self) -> Option<TextChange> {
        if self.len() <= MAX_DOCUMENT_UNITS || self.is_composing() || self.guest != self.document {
            return None;
        }
        let cut = self
            .document
            .selection
            .0
            .saturating_sub(2 * EDITOR_REACH_UNITS);
        if cut == 0 {
            return None;
        }
        let shift = |text: &mut EditorText| {
            text.units.drain(..cut as usize);
            text.start = text.start.saturating_add(cut);
            text.selection = (text.selection.0 - cut, text.selection.1 - cut);
        };
        shift(&mut self.document);
        shift(&mut self.guest);
        self.expected.clear();
        Some(TextChange {
            start: 0,
            old_end: cut,
            new_end: 0,
        })
    }
}

/// Whether the guest's editor was selected backwards: its active end comes first.
fn editor_backwards(editor: Option<&ImeEditor>) -> bool {
    editor.is_some_and(|editor| editor.selection.1 < editor.selection.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: i32 = 7;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn compose(text: &str, cursor: u32) -> ImeEvent {
        ImeEvent::Composition {
            task_id: TASK,
            text: text.into(),
            cursor,
        }
    }

    fn commit(text: &str) -> ImeEvent {
        ImeEvent::Commit {
            task_id: TASK,
            text: text.into(),
        }
    }

    fn select(start: u32, end: u32) -> ImeEvent {
        ImeEvent::SetSelection {
            task_id: TASK,
            start,
            end,
        }
    }

    fn region(start: u32, end: u32) -> ImeEvent {
        ImeEvent::SetComposingRegion {
            task_id: TASK,
            start,
            end,
        }
    }

    fn described(text: &str, start: u32, selection: (u32, u32)) -> ImeEditor {
        ImeEditor {
            kind: InputKind::Text,
            text: text.into(),
            text_start: start,
            selection,
            composition: None,
        }
    }

    /// A guest editor that applies what the store sends, as the agent's input method does.
    struct Guest {
        text: EditorText,
    }

    impl Guest {
        fn new() -> Guest {
            Guest {
                text: EditorText::default(),
            }
        }

        fn take(&mut self, events: &[ImeEvent]) {
            for event in events {
                self.text.apply(event);
            }
        }

        fn shows(&self) -> String {
            String::from_utf16_lossy(&self.text.units)
        }
    }

    /// One edit of the framework's: a composition that is now `text` over `range`.
    fn composing(store: &mut TextStore, range: (u32, u32), text: &str, then: (u32, u32)) {
        store.replace(range.0, range.1, &units(text)).unwrap();
        let end = range.0 + units(text).len() as u32;
        store.set_composition(Some((then.0, then.1.max(end).min(end))));
        store.set_selection(end, end, false).unwrap();
    }

    #[test]
    fn fr6_korean_composes_in_the_guest_and_commits_each_syllable_as_the_next_begins() {
        let mut store = TextStore::new(TASK);
        let mut guest = Guest::new();
        let mut sent = Vec::new();
        // 한: ㅎ, 하, 한, each replacing the composition.
        store.set_composition(Some((0, 0)));
        for (i, jamo) in ["\u{314e}", "\u{d558}", "\u{d55c}"].iter().enumerate() {
            let range = if i == 0 { (0, 0) } else { (0, 1) };
            composing(&mut store, range, jamo, (0, 1));
            sent.extend(store.flush());
        }
        assert_eq!(
            sent,
            vec![
                compose("\u{314e}", 1),
                compose("\u{d558}", 1),
                compose("\u{d55c}", 1)
            ]
        );
        guest.take(&sent);
        // 한 committed as ㄱ begins, in one edit.
        store.set_composition(None);
        composing(&mut store, (1, 1), "\u{3131}", (1, 2));
        let sent = store.flush();
        assert_eq!(sent, vec![commit("\u{d55c}"), compose("\u{3131}", 1)]);
        guest.take(&sent);
        // And 글 ends with the composition.
        composing(&mut store, (1, 2), "\u{ae00}", (1, 2));
        store.set_composition(None);
        let sent = store.flush();
        assert_eq!(sent, vec![commit("\u{ae00}")]);
        guest.take(&sent);
        assert_eq!(guest.shows(), "\u{d55c}\u{ae00}");
        assert_eq!(guest.text, store.guest);
        assert!(store.flush().is_empty(), "nothing more to say");
    }

    #[test]
    fn fr6_a_canceled_composition_is_removed_from_the_guest() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("ab", 0, (2, 2)));
        let mut guest = Guest {
            text: EditorText::of(&described("ab", 0, (2, 2))),
        };
        store.set_composition(Some((2, 2)));
        composing(&mut store, (2, 2), "\u{314e}", (2, 3));
        guest.take(&store.flush());
        // Escape: the input method removes the composition's text and ends it.
        store.replace(2, 3, &[]).unwrap();
        store.set_composition(None);
        let sent = store.flush();
        assert_eq!(sent, vec![compose("", 0)]);
        guest.take(&sent);
        assert_eq!(guest.shows(), "ab");
        assert_eq!(guest.text.composition, None);
        assert_eq!(guest.text.selection, (2, 2));
    }

    #[test]
    fn fr6_dictation_and_the_emoji_panel_type_through_the_document() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("say ", 10, (14, 14)));
        // Dictation types a phrase in place of the selection, without composing.
        let change = store.insert_at_selection(&units("hello"));
        assert_eq!(
            change,
            TextChange {
                start: 4,
                old_end: 4,
                new_end: 9
            }
        );
        store.set_selection(9, 9, false).unwrap();
        assert_eq!(store.flush(), vec![commit("hello")]);
        // The emoji panel types one character beyond the Basic Multilingual Plane, two units.
        store.insert_at_selection(&units("\u{1f600}"));
        store.set_selection(11, 11, false).unwrap();
        assert_eq!(store.flush(), vec![commit("\u{1f600}")]);
        assert_eq!(store.selection(), (11, 11, false));
    }

    #[test]
    fn fr6_replacing_one_emoji_with_another_never_splits_its_surrogate_pair() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("a\u{1f600}", 0, (3, 3)));
        // U+1F600 and U+1F601 share their first half.
        store.set_selection(1, 3, false).unwrap();
        store.insert_at_selection(&units("\u{1f601}"));
        store.set_selection(3, 3, false).unwrap();
        let sent = store.flush();
        assert_eq!(sent, vec![region(1, 3), commit("\u{1f601}")]);
        for event in &sent {
            if let ImeEvent::Commit { text, .. } = event {
                assert!(!text.contains('\u{fffd}'));
            }
        }
    }

    #[test]
    fn fr6_the_document_is_the_text_and_selection_the_guest_reports() {
        let mut store = TextStore::new(TASK);
        let told = store.described_as(ImeEditor {
            kind: InputKind::Text,
            text: "\u{d55c}\u{ae00} text".into(),
            text_start: 100,
            selection: (107, 102),
            composition: None,
        });
        assert_eq!(
            told.text,
            Some(TextChange {
                start: 0,
                old_end: 0,
                new_end: 7
            })
        );
        assert!(told.selection);
        assert_eq!(store.text(0, 2), &units("\u{d55c}\u{ae00}")[..]);
        assert_eq!(store.selection(), (2, 7, true), "selected backwards");
        // The same description again changes nothing.
        let again = store.described_as(ImeEditor {
            kind: InputKind::Text,
            text: "\u{d55c}\u{ae00} text".into(),
            text_start: 100,
            selection: (107, 102),
            composition: None,
        });
        assert_eq!(again, Described::default());
    }

    #[test]
    fn fr6_reconverting_a_word_makes_it_the_guests_composition_and_replaces_it() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("\u{d55c}\u{ae00}", 50, (52, 52)));
        let mut guest = Guest {
            text: EditorText::of(&described("\u{d55c}\u{ae00}", 50, (52, 52))),
        };
        // The input method selects 한, starts a composition over it and converts it to 漢.
        store.set_selection(0, 1, false).unwrap();
        store.set_composition(Some((0, 1)));
        store.replace(0, 1, &units("\u{6f22}")).unwrap();
        store.set_selection(1, 1, false).unwrap();
        let sent = store.flush();
        assert_eq!(sent, vec![region(50, 51), compose("\u{6f22}", 1)]);
        guest.take(&sent);
        // The conversion is committed.
        store.set_composition(None);
        let sent = store.flush();
        assert_eq!(sent, vec![commit("\u{6f22}")]);
        guest.take(&sent);
        assert_eq!(guest.shows(), "\u{6f22}\u{ae00}");
        assert_eq!(guest.text.selection, (1, 1));
    }

    #[test]
    fn fr6_the_input_method_moves_the_selection_in_the_guest() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("hello", 0, (5, 5)));
        store.set_selection(1, 4, true).unwrap();
        assert_eq!(store.flush(), vec![select(1, 4)]);
        assert_eq!(store.selection(), (1, 4, true));
        assert!(store.flush().is_empty());
    }

    #[test]
    fn fr6_a_stale_description_is_ignored_and_a_change_in_the_guest_is_taken() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("", 0, (0, 0)));
        store.set_composition(Some((0, 0)));
        composing(&mut store, (0, 0), "\u{314e}", (0, 1));
        store.flush();
        composing(&mut store, (0, 1), "\u{d558}", (0, 1));
        store.flush();
        store.set_composition(None);
        store.flush();
        // The guest's description after ㅎ arrives late: the host's events passed through
        // it, so nothing changes.
        let stale = ImeEditor {
            composition: Some((0, 1)),
            ..described("\u{314e}", 0, (1, 1))
        };
        assert_eq!(store.described_as(stale), Described::default());
        assert_eq!(store.text(0, 9), &units("\u{d558}")[..]);
        // The user clicks before the syllable in the guest: that is news.
        let told = store.described_as(described("\u{d558}", 0, (0, 0)));
        assert!(told.selection);
        assert_eq!(told.text, None);
        assert_eq!(store.selection(), (0, 0, false));
        // The app changes its text: the document follows.
        let told = store.described_as(described("x\u{d558}", 0, (1, 1)));
        assert_eq!(
            told.text,
            Some(TextChange {
                start: 0,
                old_end: 0,
                new_end: 1
            })
        );
        assert!(store.flush().is_empty(), "nothing goes back to the guest");
    }

    #[test]
    fn fr6_a_change_in_the_guest_while_composing_ends_the_composition() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("ab", 0, (2, 2)));
        store.set_composition(Some((2, 2)));
        composing(&mut store, (2, 2), "\u{314e}", (2, 3));
        store.flush();
        let told = store.described_as(described("ab", 0, (0, 0)));
        assert!(told.ends_composition);
    }

    #[test]
    fn fr6_characters_typed_without_the_input_method_enter_the_document() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("", 0, (0, 0)));
        let (sent, change) = store.typed(u16::from(b'a'));
        assert_eq!(sent, vec![commit("a")]);
        assert_eq!(
            change,
            Some(TextChange {
                start: 0,
                old_end: 0,
                new_end: 1
            })
        );
        // The halves of U+1F600, then a control character, which its key event carried.
        assert!(store.typed(0xd83d).0.is_empty());
        assert_eq!(store.typed(0xde00).0, vec![commit("\u{1f600}")]);
        assert_eq!(store.typed(0x0d), (vec![], None));
        assert_eq!(store.text(0, 9), &units("a\u{1f600}")[..]);
        assert_eq!(store.selection(), (3, 3, false));
        // Where no editor takes text, the character still goes, as keys, and no document
        // holds it.
        store.described_as(ImeEditor::none());
        assert_eq!(store.typed(u16::from(b'w')), (vec![commit("w")], None));
        assert!(store.is_empty());
    }

    #[test]
    fn fr6_each_input_kind_has_the_input_scope_the_touch_keyboard_reads() {
        let cases = [
            (InputKind::Text, IS_DEFAULT),
            (InputKind::None, IS_DEFAULT),
            (InputKind::Password, IS_PASSWORD),
            (InputKind::Number, IS_NUMBER),
            (InputKind::NumberPassword, IS_NUMERIC_PASSWORD),
            (InputKind::Phone, IS_TELEPHONE_FULLTELEPHONENUMBER),
            (InputKind::Email, IS_EMAIL_SMTPEMAILADDRESS),
            (InputKind::Url, IS_URL),
            (InputKind::PersonName, IS_PERSONALNAME_FULLNAME),
            (InputKind::Date, IS_DATE_FULLDATE),
            (InputKind::Time, IS_TIME_FULLTIME),
            (InputKind::Search, IS_SEARCH),
        ];
        for (kind, scope) in cases {
            assert_eq!(input_scopes(kind), &[scope], "{kind:?}");
        }
        let mut store = TextStore::new(TASK);
        assert_eq!(store.kind(), InputKind::Text, "before the guest says");
        let told = store.described_as(ImeEditor {
            kind: InputKind::Email,
            ..described("", 0, (0, 0))
        });
        assert!(told.kind);
        assert_eq!(store.kind(), InputKind::Email);
    }

    #[test]
    fn fr6_an_edit_outside_the_document_is_refused() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("ab", 0, (2, 2)));
        assert_eq!(store.replace(1, 3, &[]), Err(OutOfRange));
        assert_eq!(store.replace(2, 1, &[]), Err(OutOfRange));
        assert_eq!(store.set_selection(0, 3, false), Err(OutOfRange));
        assert_eq!(store.text(1, 99), &units("b")[..]);
    }

    #[test]
    fn fr6_a_composition_the_input_method_gives_through_imm32_reaches_the_guest_in_order() {
        // An input method that composes outside the document (the window's text services
        // document is not the focused one), typing 한글 and a space.
        let mut store = TextStore::new(TASK);
        store.described_as(described("", 0, (0, 0)));
        let mut guest = Guest {
            text: EditorText::of(&described("", 0, (0, 0))),
        };
        let mut sent = Vec::new();
        // G, K, S: ㅎ, 하, 한, each shown as it is typed.
        for jamo in ["\u{314e}", "\u{d558}", "\u{d55c}"] {
            store.compose_outside(&units(jamo), 1);
            let events = store.flush();
            guest.take(&events);
            assert_eq!(guest.text.composition, Some((0, 1)), "{jamo} is composing");
            sent.extend(events);
        }
        assert_eq!(guest.shows(), "\u{d55c}");
        // R: 한 is committed and ㄱ begins, in one message.
        store.commit_outside(&units("\u{d55c}"));
        store.compose_outside(&units("\u{3131}"), 1);
        let events = store.flush();
        assert_eq!(events, vec![commit("\u{d55c}"), compose("\u{3131}", 1)]);
        guest.take(&events);
        sent.extend(events);
        // M, F: 그, 글. Space: 글 is committed, and the space is typed after it.
        for jamo in ["\u{adf8}", "\u{ae00}"] {
            store.compose_outside(&units(jamo), 1);
            let events = store.flush();
            guest.take(&events);
            sent.extend(events);
        }
        store.commit_outside(&units("\u{ae00}"));
        store.commit_outside(&units(" "));
        let events = store.flush();
        guest.take(&events);
        sent.extend(events);
        assert_eq!(
            sent,
            vec![
                compose("\u{314e}", 1),
                compose("\u{d558}", 1),
                compose("\u{d55c}", 1),
                commit("\u{d55c}"),
                compose("\u{3131}", 1),
                compose("\u{adf8}", 1),
                compose("\u{ae00}", 1),
                commit("\u{ae00} "),
            ]
        );
        assert_eq!(guest.shows(), "\u{d55c}\u{ae00} ");
        assert_eq!(guest.text.composition, None);
        assert_eq!(guest.text, store.guest);
    }

    #[test]
    fn fr6_a_composition_canceled_through_imm32_is_removed_from_the_guest() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("ab", 0, (2, 2)));
        let mut guest = Guest {
            text: EditorText::of(&described("ab", 0, (2, 2))),
        };
        store.compose_outside(&units("\u{d55c}"), 1);
        guest.take(&store.flush());
        assert_eq!(guest.shows(), "ab\u{d55c}");
        // Escape: the input method empties its composition.
        store.compose_outside(&[], 0);
        let sent = store.flush();
        assert_eq!(sent, vec![compose("", 0)]);
        guest.take(&sent);
        assert_eq!(guest.shows(), "ab");
        assert_eq!(guest.text.composition, None);
        assert_eq!(guest.text.selection, (2, 2));
        assert!(!store.is_composing());
    }

    #[test]
    fn fr6_a_click_elsewhere_while_composing_keeps_the_syllable_and_ends_its_composition() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("ab", 0, (2, 2)));
        let mut guest = Guest {
            text: EditorText::of(&described("ab", 0, (2, 2))),
        };
        store.set_composition(Some((2, 2)));
        composing(&mut store, (2, 2), "\u{d55c}", (2, 3));
        guest.take(&store.flush());
        // The user clicks before "ab" in the guest; its editor still shows the composing
        // syllable as its composing region.
        let clicked = ImeEditor {
            composition: Some((2, 3)),
            ..described("ab\u{d55c}", 0, (0, 0))
        };
        let told = store.described_as(clicked);
        assert!(told.ends_composition);
        // The input method's composition ends, and the guest's ends with it, its text kept
        // and the cursor where the user clicked.
        store.set_composition(None);
        let sent = store.flush();
        guest.take(&sent);
        assert_eq!(guest.shows(), "ab\u{d55c}");
        assert_eq!(guest.text.composition, None);
        assert_eq!(guest.text.selection, (0, 0));
        assert!(store.flush().is_empty());
    }

    /// The guest's editor as the agent describes it.
    fn description(text: &EditorText) -> ImeEditor {
        ImeEditor {
            kind: InputKind::Text,
            text: String::from_utf16_lossy(&text.units),
            text_start: text.start,
            selection: (text.start + text.selection.0, text.start + text.selection.1),
            composition: text
                .composition
                .map(|(from, to)| (text.start + from, text.start + to)),
        }
    }

    /// A guest editor that applies each event as it arrives and describes itself after each,
    /// as the agent does, and whose descriptions reach the host `lag` descriptions late: the
    /// user types faster than the guest answers.
    struct LateGuest {
        guest: Guest,
        lag: usize,
        waiting: VecDeque<ImeEditor>,
        told: Vec<Described>,
    }

    impl LateGuest {
        fn new(lag: usize) -> LateGuest {
            LateGuest {
                guest: Guest::new(),
                lag,
                waiting: VecDeque::new(),
                told: Vec::new(),
            }
        }

        /// The events of one edit reach the guest, and the descriptions due by now the host.
        fn take(&mut self, store: &mut TextStore, events: &[ImeEvent]) {
            for event in events {
                self.guest.text.apply(event);
                self.waiting.push_back(description(&self.guest.text));
            }
            self.deliver(store, self.lag);
        }

        /// Something changes the guest's editor that the host did not send, described at once.
        fn change(&mut self, store: &mut TextStore, change: impl FnOnce(&mut EditorText)) {
            change(&mut self.guest.text);
            self.waiting.push_back(description(&self.guest.text));
            self.deliver(store, self.lag);
        }

        fn deliver(&mut self, store: &mut TextStore, keep: usize) {
            while self.waiting.len() > keep {
                let editor = self.waiting.pop_front().unwrap();
                self.told.push(store.described_as(editor));
            }
        }

        /// Every description still on its way reaches the host.
        fn settle(&mut self, store: &mut TextStore) {
            self.deliver(store, 0);
        }

        /// Whether the host was told nothing of any description: each was its own echo.
        fn all_echoes(&self) -> bool {
            self.told.iter().all(|told| *told == Described::default())
        }
    }

    /// What a Korean input method does as each key is typed: what it commits, if anything, and
    /// what it composes next, if anything.
    type Steps = &'static [(Option<&'static str>, Option<&'static str>)];

    /// How one step reaches the document: as the framework's edits, or through IMM32.
    type Step = fn(&mut TextStore, (Option<&str>, Option<&str>));

    /// 한글, jamo by jamo: ㅎ ㅏ ㄴ ㄱ ㅡ ㄹ, then Space.
    const HANGUL: Steps = &[
        (None, Some("ㅎ")),
        (None, Some("하")),
        (None, Some("한")),
        (Some("한"), Some("ㄱ")),
        (None, Some("그")),
        (None, Some("글")),
        (Some("글"), None),
    ];

    /// 안녕하세요, jamo by jamo, then Enter.
    const ANNYEONG: Steps = &[
        (None, Some("ㅇ")),
        (None, Some("아")),
        (None, Some("안")),
        (Some("안"), Some("ㄴ")),
        (None, Some("녀")),
        (None, Some("녕")),
        (Some("녕"), Some("ㅎ")),
        (None, Some("하")),
        (None, Some("핫")),
        (Some("하"), Some("세")),
        (None, Some("셍")),
        (Some("세"), Some("요")),
        (Some("요"), None),
    ];

    /// One step typed through the window's document: the framework edits its composition.
    fn step_in_document(store: &mut TextStore, step: (Option<&str>, Option<&str>)) {
        let (result, composition) = step;
        let cursor = store.selection().1;
        let (mut at, mut end) = store.composition().unwrap_or((cursor, cursor));
        if let Some(result) = result {
            store.replace(at, end, &units(result)).unwrap();
            at += units(result).len() as u32;
            end = at;
            store.set_composition(None);
        }
        match composition {
            Some(text) => {
                store.set_composition(Some((at, end)));
                composing(store, (at, end), text, (at, at + units(text).len() as u32));
            }
            None => {
                store.set_composition(None);
                store.set_selection(at, at, false).unwrap();
            }
        }
    }

    /// One step typed through IMM32, as the window hands each message to the document.
    fn step_through_imm32(store: &mut TextStore, step: (Option<&str>, Option<&str>)) {
        let (result, composition) = step;
        if let Some(result) = result {
            store.commit_outside(&units(result));
        }
        if let Some(text) = composition {
            let text = units(text);
            store.compose_outside(&text, text.len() as u32);
        }
    }

    fn type_late(steps: Steps, lag: usize, step: Step) -> (TextStore, LateGuest) {
        let mut store = TextStore::new(TASK);
        store.described_as(described("", 0, (0, 0)));
        let mut guest = LateGuest::new(lag);
        for &one in steps {
            step(&mut store, one);
            let events = store.flush();
            guest.take(&mut store, &events);
        }
        guest.settle(&mut store);
        (store, guest)
    }

    #[test]
    fn fr6_fast_korean_typing_whose_echoes_come_late_never_ends_a_composition_early() {
        for (steps, typed) in [(HANGUL, "한글"), (ANNYEONG, "안녕하세요")] {
            for lag in 0..8 {
                for (path, step) in [
                    ("the document", step_in_document as Step),
                    ("IMM32", step_through_imm32 as Step),
                ] {
                    let (store, guest) = type_late(steps, lag, step);
                    assert!(
                        guest.all_echoes(),
                        "{typed} through {path}, {lag} descriptions late: {:?}",
                        guest.told
                    );
                    assert_eq!(
                        guest.guest.shows(),
                        typed,
                        "{typed} through {path}, lag {lag}"
                    );
                    assert_eq!(store.text(0, 99), &units(typed)[..]);
                    assert_eq!(guest.guest.text.composition, None);
                }
            }
        }
    }

    /// One step typed through the window's document in separate edits, as an input method
    /// that commits a syllable and starts the next composition in edit sessions of their own
    /// does: each edit is sent before the next one is made.
    fn step_in_sessions(
        store: &mut TextStore,
        step: (Option<&str>, Option<&str>),
    ) -> Vec<ImeEvent> {
        let (result, composition) = step;
        let mut sent = Vec::new();
        let cursor = store.selection().1;
        let (mut at, mut end) = store.composition().unwrap_or((cursor, cursor));
        if let Some(result) = result {
            store.replace(at, end, &units(result)).unwrap();
            at += units(result).len() as u32;
            end = at;
            store.set_composition(None);
            store.set_selection(at, at, false).unwrap();
            sent.extend(store.flush());
            if composition.is_some() {
                // The next composition begins, empty, in an edit of its own.
                store.set_composition(Some((at, at)));
                sent.extend(store.flush());
            }
        }
        match composition {
            Some(text) => {
                store.set_composition(Some((at, end)));
                composing(store, (at, end), text, (at, at + units(text).len() as u32));
            }
            None => {
                store.set_composition(None);
                store.set_selection(at, at, false).unwrap();
            }
        }
        sent.extend(store.flush());
        sent
    }

    #[test]
    fn fr6_a_compose_field_that_echoes_the_commit_late_keeps_each_next_syllable_composing() {
        // A Jetpack Compose field (the Play Store's search box) reports its states as its
        // frames come: every state the host's events made, or only the last of a burst, each
        // up to several descriptions late, the commit of a syllable after the next one began.
        for (steps, typed) in [(HANGUL, "한글"), (ANNYEONG, "안녕하세요")] {
            for lag in 0..6 {
                for every_state in [true, false] {
                    let mut store = TextStore::new(TASK);
                    store.described_as(described("", 0, (0, 0)));
                    let mut guest = LateGuest::new(lag);
                    for &one in steps {
                        let events = step_in_sessions(&mut store, one);
                        if every_state {
                            guest.take(&mut store, &events);
                        } else {
                            guest.guest.take(&events);
                            let last = description(&guest.guest.text);
                            guest.waiting.push_back(last);
                            guest.deliver(&mut store, lag);
                        }
                    }
                    guest.settle(&mut store);
                    assert!(
                        guest.all_echoes(),
                        "{typed}, {lag} descriptions late, every state {every_state}: {:?}",
                        guest.told
                    );
                    assert_eq!(guest.guest.shows(), typed, "{typed}, lag {lag}");
                    assert_eq!(store.text(0, 99), &units(typed)[..]);
                }
            }
        }
    }

    #[test]
    fn fr6_a_compose_field_restarting_mid_syllable_with_the_commit_still_on_its_way_goes_on() {
        for lag in 0..4 {
            let mut store = TextStore::new(TASK);
            store.described_as(described("", 0, (0, 0)));
            let mut guest = LateGuest::new(lag);
            // 안, then ㄴ: 안 is committed and ㄴ composes.
            for &one in &ANNYEONG[..4] {
                let events = step_in_sessions(&mut store, one);
                guest.take(&mut store, &events);
            }
            // The field restarts input while the echoes of the commit of 안 are still on their
            // way. The agent makes ㄴ the composition again and describes the editor so, once
            // more and late like the rest.
            guest.change(&mut store, |_| {});
            for &one in &ANNYEONG[4..] {
                let events = step_in_sessions(&mut store, one);
                guest.take(&mut store, &events);
            }
            guest.settle(&mut store);
            assert!(guest.all_echoes(), "lag {lag}: {:?}", guest.told);
            assert_eq!(guest.guest.shows(), "안녕하세요", "lag {lag}");
        }
    }

    #[test]
    fn fr6_a_composition_whose_editor_restarted_input_goes_on_in_the_same_syllable() {
        for (path, step) in [
            ("the document", step_in_document as Step),
            ("IMM32", step_through_imm32 as Step),
        ] {
            let mut store = TextStore::new(TASK);
            store.described_as(described("a", 0, (1, 1)));
            let mut guest = LateGuest::new(0);
            guest.guest.text = EditorText::of(&described("a", 0, (1, 1)));
            for &one in &HANGUL[..2] {
                step(&mut store, one);
                let events = store.flush();
                guest.take(&mut store, &events);
            }
            // Input restarts on the editor: the framework finishes its composition, which
            // keeps 하 and loses the span, and the agent describes it so.
            guest.change(&mut store, |text| text.composition = None);
            assert!(guest.all_echoes(), "through {path}: {:?}", guest.told);
            // ㄴ: 한 takes the place of 하, not a place beside it.
            step(&mut store, HANGUL[2]);
            let events = store.flush();
            assert_eq!(events.first(), Some(&region(1, 2)), "through {path}");
            guest.take(&mut store, &events);
            assert_eq!(guest.guest.shows(), "a한", "through {path}");
            assert_eq!(guest.guest.text.composition, Some((1, 2)));
            // The rest of 한글 follows as ever.
            for &one in &HANGUL[3..] {
                step(&mut store, one);
                let events = store.flush();
                guest.take(&mut store, &events);
            }
            guest.settle(&mut store);
            assert!(guest.all_echoes(), "through {path}: {:?}", guest.told);
            assert_eq!(guest.guest.shows(), "a한글", "through {path}");
        }
    }

    #[test]
    fn fr6_a_click_elsewhere_ends_the_composition_even_with_echoes_still_on_their_way() {
        for lag in 0..4 {
            let mut store = TextStore::new(TASK);
            store.described_as(described("ab", 0, (2, 2)));
            let mut guest = LateGuest::new(lag);
            guest.guest.text = EditorText::of(&described("ab", 0, (2, 2)));
            for &one in &HANGUL[..3] {
                step_through_imm32(&mut store, one);
                let events = store.flush();
                guest.take(&mut store, &events);
            }
            // The user clicks before "ab" while the echoes of 한 are still on their way.
            guest.change(&mut store, |text| text.selection = (0, 0));
            guest.settle(&mut store);
            let ends: Vec<bool> = guest.told.iter().map(|t| t.ends_composition).collect();
            assert_eq!(
                ends.iter().filter(|end| **end).count(),
                1,
                "lag {lag}: {ends:?}"
            );
            assert_eq!(
                ends.last(),
                Some(&true),
                "the click, and only the click, ends it"
            );
        }
    }

    #[test]
    fn fr6_a_change_the_host_never_made_ends_the_composition_while_echoes_do_not() {
        let mut store = TextStore::new(TASK);
        store.described_as(described("", 0, (0, 0)));
        let mut guest = LateGuest::new(2);
        for &one in &ANNYEONG[..4] {
            step_in_document(&mut store, one);
            let events = store.flush();
            guest.take(&mut store, &events);
        }
        assert!(guest.all_echoes(), "{:?}", guest.told);
        // The app clears its search field while 안ㄴ is being typed.
        guest.change(&mut store, |text| *text = EditorText::default());
        guest.settle(&mut store);
        assert!(guest.told.last().unwrap().ends_composition);
        assert!(guest.told[..guest.told.len() - 1]
            .iter()
            .all(|told| !told.ends_composition));
        // Another editor takes the focus while a composition is being typed.
        let mut store = TextStore::new(TASK);
        store.described_as(described("", 0, (0, 0)));
        step_through_imm32(&mut store, ANNYEONG[0]);
        store.flush();
        let told = store.described_as(ImeEditor {
            kind: InputKind::Search,
            ..described("", 0, (0, 0))
        });
        assert!(told.kind);
        assert!(told.ends_composition);
    }

    #[test]
    fn fr6_a_document_the_guest_never_describes_stays_bounded() {
        let mut store = TextStore::new(TASK);
        for _ in 0..MAX_DOCUMENT_UNITS + 1 {
            store.typed(u16::from(b'a'));
        }
        let change = store.trim().expect("trimmed");
        assert_eq!(change.start, 0);
        assert!(store.len() <= 2 * EDITOR_REACH_UNITS);
        assert_eq!(store.selection().0, store.len());
        // What is sent after it still lands at the cursor.
        store.insert_at_selection(&units("b"));
        let end = store.len();
        store.set_selection(end, end, false).unwrap();
        assert_eq!(store.flush(), vec![commit("b")]);
    }
}

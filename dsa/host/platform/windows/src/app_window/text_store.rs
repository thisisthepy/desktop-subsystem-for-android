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
//! a state the host's own events pass through on their way is stale and changes nothing; one
//! the host's events never made (the user clicked elsewhere in the editor, or the app changed
//! its text) becomes the document, and the framework is told what changed.
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
    if old == new {
        return None;
    }
    let mut prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix > 0 && is_high_surrogate(old[prefix - 1]) {
        prefix -= 1;
    }
    let most = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(most)
        .take_while(|(a, b)| a == b)
        .count();
    if suffix > 0 && is_low_surrogate(old[old.len() - suffix]) {
        suffix -= 1;
    }
    Some((
        prefix as u32,
        (old.len() - suffix) as u32,
        (new.len() - suffix) as u32,
    ))
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xdc00..=0xdfff).contains(&unit)
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
            // A state the host's events pass through: the rest is on its way.
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

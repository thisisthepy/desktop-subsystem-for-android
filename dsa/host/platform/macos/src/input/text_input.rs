//! The input method on macOS: what `NSTextInputClient` is told, as the platform-neutral
//! text input's compositions and commits, which reach the agent's `InputConnection`.
//!
//! A key goes first to the input method (`interpretKeyEvents:`). If it composes or types
//! text, the text goes to the guest and the key does not; if it is a command (Return, an
//! arrow, delete) or nothing at all, the key goes to the guest as a key event, so the guest
//! never sees a character twice.
//!
//! The client's document is the guest's focused editor around its selection, as the agent
//! describes it ([`ImeEditor`]) and as each event sent since changes it ([`EditorText`]).
//! Ranges are the editor's own offsets in UTF-16 code units, as `NSRange` counts them, so the
//! input method reads the text before the cursor (`attributedSubstringForProposedRange:`),
//! the selection and the composition where they stand in the editor, and its replacement
//! ranges (a reconversion, a correction) reach the guest as the selection or the composing
//! region they name. A description of a state the host's own events pass through is late and
//! changes nothing; any other is the guest's editor as it now is.

use std::collections::VecDeque;

use dsa_host_core::ime::{utf16_len, EditorText, ImeEditor, ImeEvent, InputKind, TextInput};

/// How many of the states the events sent pass through are remembered, to tell a late
/// description from a change in the guest.
const MAX_EXPECTED: usize = 32;

/// The state `NSTextInputClient` reports back to the input method.
#[derive(Debug)]
pub struct TextInputClient {
    task_id: i32,
    text: TextInput,
    /// The marked (composing) text; empty when there is none.
    marked: String,
    /// Its length in UTF-16 code units.
    marked_units: usize,
    /// The selection within the marked text, as location and length in UTF-16 code units.
    selected: (usize, usize),
    /// During a key: whether the input method typed or composed text for it.
    took_key: bool,
    /// During a key: whether there was a composition when it began.
    composing_before: bool,
    /// The guest's editor as the host knows it.
    document: EditorText,
    /// The states the guest's editor passes through on the way to `document`, oldest first.
    expected: VecDeque<EditorText>,
    /// What the guest last said of its editor.
    described: Option<ImeEditor>,
}

impl TextInputClient {
    pub fn new(task_id: i32) -> TextInputClient {
        TextInputClient {
            task_id,
            text: TextInput::new(task_id),
            marked: String::new(),
            marked_units: 0,
            selected: (0, 0),
            took_key: false,
            composing_before: false,
            document: EditorText::default(),
            expected: VecDeque::new(),
            described: None,
        }
    }

    /// The kind of text the focused editor takes; ordinary text until the guest says.
    pub fn kind(&self) -> InputKind {
        self.described
            .as_ref()
            .map_or(InputKind::Text, |editor| editor.kind)
    }

    /// The guest described its editor. Whether the document changed, so the input method's
    /// idea of where characters are must be refreshed.
    pub fn describe(&mut self, editor: ImeEditor) -> bool {
        if self.described.as_ref() == Some(&editor) {
            return false;
        }
        let seen = EditorText::of(&editor);
        self.described = Some(editor);
        if self.has_marked_text() {
            // The input method's composition stands until it ends it.
            return false;
        }
        if seen == self.document {
            return false;
        }
        let agrees = seen.agrees_with(&self.document);
        if !agrees && self.expected.iter().any(|state| seen.agrees_with(state)) {
            // A state the host's events pass through: the rest is on its way.
            return false;
        }
        if !agrees {
            self.expected.clear();
        }
        self.document = seen;
        true
    }

    /// Where the document begins in the editor's text.
    fn start(&self) -> usize {
        self.document.start as usize
    }

    /// `hasMarkedText`.
    pub fn has_marked_text(&self) -> bool {
        self.marked_units > 0
    }

    /// Where the composition stands in the document, in the editor's offsets.
    fn marked_start(&self) -> usize {
        let local = self
            .document
            .composition
            .map_or(self.document.selection.0, |(start, _)| start);
        self.start() + local as usize
    }

    /// `markedRange`: `None` is `{NSNotFound, 0}`.
    pub fn marked_range(&self) -> Option<(usize, usize)> {
        self.has_marked_text()
            .then(|| (self.marked_start(), self.marked_units))
    }

    /// `selectedRange`: the input method's selection within the composition while it
    /// composes, and the guest editor's selection otherwise.
    pub fn selected_range(&self) -> (usize, usize) {
        if self.has_marked_text() {
            return (self.marked_start() + self.selected.0, self.selected.1);
        }
        let (start, end) = self.document.selection;
        (self.start() + start as usize, (end - start) as usize)
    }

    /// `attributedSubstringForProposedRange:actualRange:`: the editor's text in `range`, as
    /// far as the document reaches, with the range it covers. `None` outside the document.
    pub fn substring(&self, range: (usize, usize)) -> Option<(String, (usize, usize))> {
        let begin = self.start();
        let end = begin + self.document.units.len();
        let from = range.0.max(begin);
        let to = range.0.saturating_add(range.1).min(end);
        if from > to || from > end {
            return None;
        }
        let units = &self.document.units[from - begin..to - begin];
        Some((String::from_utf16_lossy(units), (from, to - from)))
    }

    /// `firstRectForCharacterRange:actualRange:`: the part of `range` the rectangle (the
    /// guest's cursor) stands for: `range` itself, cut to the document.
    pub fn rect_range(&self, range: (usize, usize)) -> (usize, usize) {
        self.substring(range).map_or(range, |(_, actual)| actual)
    }

    /// Records events as sent: the document follows each, and remembers what it passed
    /// through.
    fn sent(&mut self, events: Vec<ImeEvent>) -> Vec<ImeEvent> {
        for event in &events {
            self.document.apply(event);
            self.expected.push_back(self.document.clone());
        }
        while self.expected.len() > MAX_EXPECTED {
            self.expected.pop_front();
        }
        events
    }

    /// Makes the guest's editor replace `range` (editor offsets) next: its composing region,
    /// or the cursor there when it is empty.
    fn aim(&self, range: (usize, usize), events: &mut Vec<ImeEvent>) {
        let start = range.0 as u32;
        let end = range.0.saturating_add(range.1) as u32;
        events.push(if end > start {
            ImeEvent::SetComposingRegion {
                task_id: self.task_id,
                start,
                end,
            }
        } else {
            ImeEvent::SetSelection {
                task_id: self.task_id,
                start,
                end: start,
            }
        });
    }

    /// A key is about to go to the input method.
    pub fn begin_key(&mut self) {
        self.took_key = false;
        self.composing_before = self.has_marked_text();
    }

    /// The input method is done with the key: whether the key itself must go to the guest.
    pub fn end_key(&mut self) -> bool {
        let send = !self.took_key && !self.composing_before && !self.has_marked_text();
        self.took_key = false;
        self.composing_before = false;
        send
    }

    /// `setMarkedText:selectedRange:replacementRange:` with the marked text `text` and its
    /// selection at `selected` (location, length) in UTF-16 code units.
    pub fn set_marked_text(&mut self, text: &str, selected: (usize, usize)) -> Option<ImeEvent> {
        self.set_marked_text_in(text, selected, None).pop()
    }

    /// `setMarkedText:selectedRange:replacementRange:` with a replacement range: the input
    /// method composes over text already in the editor, as when it reconverts a word.
    pub fn set_marked_text_in(
        &mut self,
        text: &str,
        selected: (usize, usize),
        replacement: Option<(usize, usize)>,
    ) -> Vec<ImeEvent> {
        self.took_key = true;
        let mut events = Vec::new();
        if let Some(range) = replacement.filter(|_| !self.has_marked_text()) {
            self.aim(range, &mut events);
        }
        let units = utf16_len(text);
        let location = selected.0.min(units);
        let end = location.saturating_add(selected.1).min(units);
        self.marked = text.to_string();
        self.marked_units = units;
        self.selected = (location, end - location);
        // The guest's composition has one cursor: at the end of the input method's selection,
        // which is where the next character goes.
        events.extend(self.text.composition(text, end as u32));
        self.sent(events)
    }

    /// `unmarkText`: the input method ends its composition, keeping the text it composed.
    pub fn unmark_text(&mut self) -> Option<ImeEvent> {
        let marked = std::mem::take(&mut self.marked);
        self.marked_units = 0;
        self.selected = (0, 0);
        if self.text.is_composing() {
            // The composed text stays in the editor as typed text.
            let events = self.text.commit(&marked).into_iter().collect();
            return self.sent(events).pop();
        }
        None
    }

    /// `insertText:replacementRange:`: `text` is typed, in place of the composition if there
    /// is one.
    pub fn insert_text(&mut self, text: &str) -> Option<ImeEvent> {
        self.insert_text_in(text, None).pop()
    }

    /// `insertText:replacementRange:` with a replacement range: `text` takes the place of the
    /// editor's text there, as a correction or a dictated phrase does.
    pub fn insert_text_in(
        &mut self,
        text: &str,
        replacement: Option<(usize, usize)>,
    ) -> Vec<ImeEvent> {
        self.took_key = true;
        let mut events = Vec::new();
        if let Some(range) = replacement.filter(|_| !self.has_marked_text()) {
            self.aim(range, &mut events);
        }
        self.marked.clear();
        self.marked_units = 0;
        self.selected = (0, 0);
        events.extend(self.text.commit(text));
        self.sent(events)
    }

    /// The window lost the keyboard: a composition left open is ended and removed, as when
    /// the user switched away mid-word.
    pub fn focus_lost(&mut self) -> Option<ImeEvent> {
        self.marked.clear();
        self.marked_units = 0;
        self.selected = (0, 0);
        let events = self.text.end().into_iter().collect();
        self.sent(events).pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composition(text: &str, cursor: u32) -> Option<ImeEvent> {
        Some(ImeEvent::Composition {
            task_id: 7,
            text: text.into(),
            cursor,
        })
    }

    fn commit(text: &str) -> Option<ImeEvent> {
        Some(ImeEvent::Commit {
            task_id: 7,
            text: text.into(),
        })
    }

    fn editor(text: &str, start: u32, selection: (u32, u32)) -> ImeEditor {
        ImeEditor {
            kind: InputKind::Text,
            text: text.into(),
            text_start: start,
            selection,
            composition: None,
        }
    }

    #[test]
    fn fr6_a_plain_key_goes_to_the_guest_as_a_key() {
        let mut client = TextInputClient::new(7);
        client.begin_key();
        // Return: the input method calls doCommandBySelector:, not insertText:.
        assert!(client.end_key());
    }

    #[test]
    fn fr6_a_typed_character_goes_as_text_and_not_as_its_key() {
        let mut client = TextInputClient::new(7);
        client.begin_key();
        assert_eq!(client.insert_text("a"), commit("a"));
        assert!(!client.end_key());
    }

    #[test]
    fn fr6_a_hangul_composition_is_sent_whole_and_committed() {
        let mut client = TextInputClient::new(7);
        client.begin_key();
        assert_eq!(client.set_marked_text("ㅎ", (1, 0)), composition("ㅎ", 1));
        assert!(!client.end_key());
        assert!(client.has_marked_text());
        assert_eq!(client.marked_range(), Some((0, 1)));

        client.begin_key();
        assert_eq!(client.set_marked_text("하", (1, 0)), composition("하", 1));
        assert!(!client.end_key());

        // Space commits the syllable; the space itself is typed after it.
        client.begin_key();
        assert_eq!(client.insert_text("하"), commit("하"));
        assert_eq!(client.insert_text(" "), commit(" "));
        assert!(!client.end_key());
        assert!(!client.has_marked_text());
    }

    #[test]
    fn fr6_a_key_that_ends_a_composition_is_the_input_methods_not_the_guests() {
        let mut client = TextInputClient::new(7);
        client.begin_key();
        client.set_marked_text("か", (1, 0));
        client.end_key();
        // Backspace through the last character: the composition goes, and the key with it.
        client.begin_key();
        assert_eq!(client.set_marked_text("", (0, 0)), composition("", 0));
        assert!(!client.end_key());
    }

    #[test]
    fn fr6_the_cursor_is_at_the_end_of_the_input_methods_selection() {
        let mut client = TextInputClient::new(7);
        assert_eq!(
            client.set_marked_text("にほんご", (0, 2)),
            composition("にほんご", 2)
        );
        assert_eq!(client.selected_range(), (0, 2));
        // A selection past the text is kept inside it.
        assert_eq!(client.set_marked_text("に", (5, 3)), composition("に", 1));
    }

    #[test]
    fn fr6_unmarking_keeps_the_composed_text() {
        let mut client = TextInputClient::new(7);
        client.set_marked_text("漢", (1, 0));
        assert_eq!(client.unmark_text(), commit("漢"));
        assert!(!client.has_marked_text());
        assert_eq!(client.unmark_text(), None);
    }

    #[test]
    fn fr6_losing_the_keyboard_removes_an_open_composition() {
        let mut client = TextInputClient::new(7);
        client.set_marked_text("ㄱ", (1, 0));
        assert_eq!(client.focus_lost(), composition("", 0));
        assert_eq!(client.focus_lost(), None);
    }

    #[test]
    fn fr6_the_input_method_reads_the_editors_text_selection_and_composition_where_they_stand() {
        let mut client = TextInputClient::new(7);
        assert!(client.describe(editor("\u{d55c}\u{ae00} ", 100, (103, 103))));
        assert_eq!(client.selected_range(), (103, 0));
        assert_eq!(
            client.substring((100, 2)),
            Some(("\u{d55c}\u{ae00}".into(), (100, 2)))
        );
        // A range reaching past the document is cut to it.
        assert_eq!(client.substring((102, 50)), Some((" ".into(), (102, 1))));
        assert_eq!(client.substring((500, 1)), None);
        assert_eq!(client.rect_range((101, 9)), (101, 2));
        // Composing after the space: the composition stands at the cursor.
        client.set_marked_text("\u{3131}", (1, 0));
        assert_eq!(client.marked_range(), Some((103, 1)));
        assert_eq!(client.selected_range(), (104, 0));
        assert_eq!(
            client.substring((100, 4)),
            Some(("\u{d55c}\u{ae00} \u{3131}".into(), (100, 4)))
        );
    }

    #[test]
    fn fr6_reconverting_a_word_makes_it_the_guests_composition_and_replaces_it() {
        let mut client = TextInputClient::new(7);
        client.describe(editor("\u{d55c}\u{ae00}", 50, (52, 52)));
        let events = client.set_marked_text_in("\u{6f22}", (1, 0), Some((50, 1)));
        assert_eq!(
            events,
            vec![
                ImeEvent::SetComposingRegion {
                    task_id: 7,
                    start: 50,
                    end: 51
                },
                composition("\u{6f22}", 1).unwrap(),
            ]
        );
        assert_eq!(client.unmark_text(), commit("\u{6f22}"));
        assert_eq!(
            client.substring((50, 2)),
            Some(("\u{6f22}\u{ae00}".into(), (50, 2)))
        );
    }

    #[test]
    fn fr6_a_correction_replaces_the_text_it_names() {
        let mut client = TextInputClient::new(7);
        client.describe(editor("teh ", 0, (4, 4)));
        let events = client.insert_text_in("the", Some((0, 3)));
        assert_eq!(
            events,
            vec![
                ImeEvent::SetComposingRegion {
                    task_id: 7,
                    start: 0,
                    end: 3
                },
                commit("the").unwrap(),
            ]
        );
        assert_eq!(client.substring((0, 4)), Some(("the ".into(), (0, 4))));
        // An empty replacement range types at that place.
        let events = client.insert_text_in("!", Some((4, 0)));
        assert_eq!(
            events[0],
            ImeEvent::SetSelection {
                task_id: 7,
                start: 4,
                end: 4
            }
        );
    }

    #[test]
    fn fr6_a_late_description_is_ignored_and_a_change_in_the_guest_is_taken() {
        let mut client = TextInputClient::new(7);
        client.describe(editor("", 0, (0, 0)));
        client.set_marked_text("\u{314e}", (1, 0));
        client.insert_text("\u{d558}");
        // The guest's description of the composition arrives after the commit.
        let late = ImeEditor {
            composition: Some((0, 1)),
            ..editor("\u{314e}", 0, (1, 1))
        };
        assert!(!client.describe(late));
        assert_eq!(client.substring((0, 9)), Some(("\u{d558}".into(), (0, 1))));
        // The user clicks before the syllable: that is news.
        assert!(client.describe(editor("\u{d558}", 0, (0, 0))));
        assert_eq!(client.selected_range(), (0, 0));
        // While the input method composes, its composition stands.
        client.set_marked_text("a", (1, 0));
        assert!(!client.describe(editor("x", 0, (1, 1))));
        assert_eq!(client.kind(), InputKind::Text);
    }
}

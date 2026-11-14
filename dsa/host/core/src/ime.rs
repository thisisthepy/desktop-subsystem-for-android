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
//!
//! The other way, the agent describes the focused editor ([`ImeEditor`]): the kind of text it
//! takes and its text around the selection. An input method that reads the document (the
//! text services framework on Windows) reconverts and predicts from it, and moves the
//! selection or makes earlier text its composition again ([`ImeEvent::SetSelection`],
//! [`ImeEvent::SetComposingRegion`]). [`EditorText`] is that text as the host knows it, and
//! changes with each event exactly as the agent's input method changes the editor, so a host
//! can tell its own edits coming back from a change the app or the user made in the guest.

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
    /// The input method deletes `before` UTF-16 code units before the cursor and `after`
    /// after it, as it does when it reconverts or corrects what was typed.
    DeleteSurrounding {
        task_id: i32,
        before: u32,
        after: u32,
    },
    /// The input method selects the editor's text from `start` to `end`, in UTF-16 code units
    /// from the start of the editor's text, or puts the cursor at `start` when they are
    /// equal. A composition stays as it is.
    SetSelection { task_id: i32, start: u32, end: u32 },
    /// The input method makes the editor's text from `start` to `end` its composition, as it
    /// does when it reconverts a word typed earlier; equal offsets end the composition and keep
    /// its text.
    SetComposingRegion { task_id: i32, start: u32, end: u32 },
}

/// The most text one deletion removes on either side of the cursor, in UTF-16 code units.
pub const MAX_DELETE_UNITS: u32 = 65536;

/// How many UTF-16 code units `text` takes.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `text` cut to at most [`MAX_TEXT_UNITS`] UTF-16 code units, on a character boundary.
pub fn bounded(text: &str) -> String {
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
            ImeEvent::Composition { task_id, .. }
            | ImeEvent::Commit { task_id, .. }
            | ImeEvent::DeleteSurrounding { task_id, .. }
            | ImeEvent::SetSelection { task_id, .. }
            | ImeEvent::SetComposingRegion { task_id, .. } => *task_id,
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
            ImeEvent::DeleteSurrounding {
                task_id,
                before,
                after,
            } => control::envelope::Body::ImeDeleteSurrounding(control::ImeDeleteSurrounding {
                task_id,
                before_length: before,
                after_length: after,
            }),
            ImeEvent::SetSelection {
                task_id,
                start,
                end,
            } => control::envelope::Body::ImeSetSelection(control::ImeSetSelection {
                task_id,
                start,
                end,
            }),
            ImeEvent::SetComposingRegion {
                task_id,
                start,
                end,
            } => control::envelope::Body::ImeSetComposingRegion(control::ImeSetComposingRegion {
                task_id,
                start,
                end,
            }),
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
            ImeEvent::DeleteSurrounding {
                task_id,
                before,
                after,
            } => wire::envelope::Body::ImeDeleteSurrounding(wire::ImeDeleteSurrounding {
                task_id,
                before_length: before,
                after_length: after,
            }),
            ImeEvent::SetSelection {
                task_id,
                start,
                end,
            } => wire::envelope::Body::ImeSetSelection(wire::ImeSetSelection {
                task_id,
                start,
                end,
            }),
            ImeEvent::SetComposingRegion {
                task_id,
                start,
                end,
            } => wire::envelope::Body::ImeSetComposingRegion(wire::ImeSetComposingRegion {
                task_id,
                start,
                end,
            }),
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

impl ImeEvent {
    /// The deletion the local API carried, or a reason it makes no sense: nothing to delete,
    /// or more than [`MAX_DELETE_UNITS`] on a side.
    pub fn from_wire_delete(delete: &wire::ImeDeleteSurrounding) -> Result<ImeEvent, String> {
        if delete.before_length == 0 && delete.after_length == 0 {
            return Err("the deletion removes nothing".into());
        }
        if delete.before_length > MAX_DELETE_UNITS || delete.after_length > MAX_DELETE_UNITS {
            return Err(format!(
                "the deletion removes more than {MAX_DELETE_UNITS} UTF-16 code units on a side"
            ));
        }
        Ok(ImeEvent::DeleteSurrounding {
            task_id: delete.task_id,
            before: delete.before_length,
            after: delete.after_length,
        })
    }
}

impl ImeEvent {
    /// The selection the local API carried, or a reason it makes no sense: an offset past
    /// what an Android editor can hold.
    pub fn from_wire_selection(selection: &wire::ImeSetSelection) -> Result<ImeEvent, String> {
        let (start, end) = check_range(selection.start, selection.end)?;
        Ok(ImeEvent::SetSelection {
            task_id: selection.task_id,
            start,
            end,
        })
    }

    /// The composing region the local API carried, or a reason it makes no sense: an offset
    /// past what an Android editor can hold, or a start after its end.
    pub fn from_wire_composing_region(
        region: &wire::ImeSetComposingRegion,
    ) -> Result<ImeEvent, String> {
        if region.start > region.end {
            return Err(format!(
                "the composing region starts at {}, after its end at {}",
                region.start, region.end
            ));
        }
        let (start, end) = check_range(region.start, region.end)?;
        Ok(ImeEvent::SetComposingRegion {
            task_id: region.task_id,
            start,
            end,
        })
    }
}

/// `start` and `end` as offsets into an Android editor, whose offsets are Java ints, or a
/// reason they are not.
fn check_range(start: u32, end: u32) -> Result<(u32, u32), String> {
    let most = i32::MAX as u32;
    if start > most || end > most {
        return Err(format!(
            "the offsets {start} and {end} lie past the {most} UTF-16 code units an editor can \
             hold"
        ));
    }
    Ok((start, end))
}

/// What kind of text the editor focused in a task takes, from the input type its app gave it.
/// The input method offers the keys for it (the touch keyboard's layout, say) and keeps
/// composition, prediction and learning out of a password.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum InputKind {
    /// No editor in the task takes text: the focus is on a view that reads keys (a game's) or
    /// on nothing. Typed characters still reach it, as the keys that type them.
    None,
    /// Ordinary text, and what an agent that does not say is taken to have.
    #[default]
    Text,
    /// A password, shown or hidden.
    Password,
    Number,
    /// A number that is a password, such as a PIN.
    NumberPassword,
    Phone,
    Email,
    Url,
    PersonName,
    Date,
    Time,
    /// Text the app searches for.
    Search,
}

impl InputKind {
    /// The kind from its number in any of the protocols, which number it the same way. An
    /// unspecified or unknown number is ordinary text, so an agent that does not say gets
    /// the input method as it always had it.
    pub fn from_number(number: i32) -> InputKind {
        match number {
            1 => InputKind::None,
            3 => InputKind::Password,
            4 => InputKind::Number,
            5 => InputKind::NumberPassword,
            6 => InputKind::Phone,
            7 => InputKind::Email,
            8 => InputKind::Url,
            9 => InputKind::PersonName,
            10 => InputKind::Date,
            11 => InputKind::Time,
            12 => InputKind::Search,
            _ => InputKind::Text,
        }
    }

    /// The kind's number in the protocols.
    pub fn number(self) -> i32 {
        match self {
            InputKind::None => 1,
            InputKind::Text => 2,
            InputKind::Password => 3,
            InputKind::Number => 4,
            InputKind::NumberPassword => 5,
            InputKind::Phone => 6,
            InputKind::Email => 7,
            InputKind::Url => 8,
            InputKind::PersonName => 9,
            InputKind::Date => 10,
            InputKind::Time => 11,
            InputKind::Search => 12,
        }
    }

    /// Whether an editor takes text, so the input method composes into it.
    pub fn takes_text(self) -> bool {
        self != InputKind::None
    }

    /// Whether what is typed is secret: nothing is composed, predicted or learned from it.
    pub fn is_secret(self) -> bool {
        matches!(self, InputKind::Password | InputKind::NumberPassword)
    }
}

/// The most text around the selection the agent describes, in UTF-16 code units.
pub const MAX_EDITOR_UNITS: usize = 4096;

/// The editor focused in a task, as the agent describes it to the host's input method.
/// Offsets are in UTF-16 code units from the start of the editor's whole text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ImeEditor {
    pub kind: InputKind,
    /// The editor's text around the selection.
    pub text: String,
    /// Where `text` starts in the editor's text.
    pub text_start: u32,
    /// The selection: its anchor and its active end, which come first when the user selected
    /// backwards. Equal for a cursor.
    pub selection: (u32, u32),
    /// The editor's composing region, if it has one.
    pub composition: Option<(u32, u32)>,
}

impl ImeEditor {
    /// No editor takes text in the task.
    pub fn none() -> ImeEditor {
        ImeEditor {
            kind: InputKind::None,
            ..ImeEditor::default()
        }
    }

    /// The editor as the agent described it, with its task, or a reason the description
    /// makes no sense: more text than one may carry, or a selection or composition outside
    /// the text.
    pub fn from_control(editor: &control::ImeEditor) -> Result<(i32, ImeEditor), String> {
        ImeEditor::checked(
            editor.kind,
            &editor.text,
            editor.text_start,
            (editor.selection_start, editor.selection_end),
            (editor.composition_start, editor.composition_end),
        )
        .map(|checked| (editor.task_id, checked))
    }

    /// The editor as the local API carries it.
    pub fn to_wire(&self, task_id: i32) -> wire::ImeEditor {
        let (composition_start, composition_end) = self
            .composition
            .unwrap_or((self.selection.1, self.selection.1));
        wire::ImeEditor {
            task_id,
            kind: self.kind.number(),
            text: self.text.clone(),
            text_start: self.text_start,
            selection_start: self.selection.0,
            selection_end: self.selection.1,
            composition_start,
            composition_end,
        }
    }

    /// The editor the local API carried, with its task, or a reason it makes no sense.
    pub fn from_wire(editor: &wire::ImeEditor) -> Result<(i32, ImeEditor), String> {
        ImeEditor::checked(
            editor.kind,
            &editor.text,
            editor.text_start,
            (editor.selection_start, editor.selection_end),
            (editor.composition_start, editor.composition_end),
        )
        .map(|checked| (editor.task_id, checked))
    }

    /// An editor from the fields every protocol carries it in, or a reason they make no
    /// sense.
    pub fn checked(
        kind: i32,
        text: &str,
        text_start: u32,
        selection: (u32, u32),
        composition: (u32, u32),
    ) -> Result<ImeEditor, String> {
        let units = utf16_len(text);
        if units > MAX_EDITOR_UNITS {
            return Err(format!(
                "the editor's text is {units} UTF-16 code units long, more than the \
                 {MAX_EDITOR_UNITS} one description may carry"
            ));
        }
        let end = u64::from(text_start) + units as u64;
        let within = |at: u32| u64::from(at) >= u64::from(text_start) && u64::from(at) <= end;
        if !within(selection.0) || !within(selection.1) {
            return Err(format!(
                "the selection from {} to {} lies outside the text, which runs from \
                 {text_start} to {end}",
                selection.0, selection.1
            ));
        }
        let (from, to) = (
            composition.0.min(composition.1),
            composition.0.max(composition.1),
        );
        if from != to && (!within(from) || !within(to)) {
            return Err(format!(
                "the composition from {from} to {to} lies outside the text, which runs from \
                 {text_start} to {end}"
            ));
        }
        Ok(ImeEditor {
            kind: InputKind::from_number(kind),
            text: if InputKind::from_number(kind).takes_text() {
                text.to_owned()
            } else {
                String::new()
            },
            text_start,
            selection,
            composition: (from != to).then_some((from, to)),
        })
    }
}

/// The text of the guest's editor around the selection, as the host knows it, in UTF-16 code
/// units, and how each [`ImeEvent`] changes it: as the agent's input method changes the
/// editor through its `InputConnection`, which replaces the composition, or the selection when
/// there is none.
///
/// Offsets inside are relative to `start`, the place in the editor's whole text where the
/// known text begins; the events carry the editor's own offsets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct EditorText {
    /// Where `units` begins in the editor's text.
    pub start: u32,
    pub units: Vec<u16>,
    /// The selection, start before end, within `units`.
    pub selection: (u32, u32),
    /// The composing region, start before end and never empty, within `units`.
    pub composition: Option<(u32, u32)>,
}

/// How far the text the agent describes reaches on each side of the selection at the least,
/// when the editor's text goes on that far. Two descriptions of the same text that both reach
/// this far on a side may end in different places there, because the agent reads a window
/// around the selection that moves with it; one that reaches less far ends where the text
/// does.
pub const EDITOR_REACH_UNITS: u32 = 512;

impl EditorText {
    /// The text an editor's description gives.
    pub fn of(editor: &ImeEditor) -> EditorText {
        let units: Vec<u16> = editor.text.encode_utf16().collect();
        let len = units.len() as u32;
        let local = |at: u32| at.saturating_sub(editor.text_start).min(len);
        let (a, b) = (local(editor.selection.0), local(editor.selection.1));
        EditorText {
            start: editor.text_start,
            units,
            selection: (a.min(b), a.max(b)),
            composition: editor
                .composition
                .map(|(from, to)| (local(from), local(to)))
                .filter(|(from, to)| from < to),
        }
    }

    pub fn len(&self) -> u32 {
        self.units.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// `at`, an offset in the editor's text, within this text.
    fn local(&self, at: u32) -> u32 {
        at.saturating_sub(self.start).min(self.len())
    }

    /// What an event replaces: the composition, or the selection when there is none.
    fn target(&self) -> (u32, u32) {
        self.composition.unwrap_or(self.selection)
    }

    /// Replaces `from..to` with `with`; every offset after it moves with the text.
    fn splice(&mut self, from: u32, to: u32, with: &[u16]) {
        let (from, to) = (from.min(self.len()), to.min(self.len()).max(from));
        self.units
            .splice(from as usize..to as usize, with.iter().copied());
        let grown = with.len() as i64 - i64::from(to - from);
        let moved = |at: u32| -> u32 {
            if at >= to {
                (i64::from(at) + grown) as u32
            } else if at > from {
                from
            } else {
                at
            }
        };
        self.selection = (moved(self.selection.0), moved(self.selection.1));
        self.composition = self
            .composition
            .map(|(a, b)| (moved(a), moved(b)))
            .filter(|(a, b)| a < b);
    }

    /// The text once the agent has applied `event` to the editor.
    pub fn apply(&mut self, event: &ImeEvent) {
        match event {
            ImeEvent::Composition { text, cursor, .. } => {
                let units: Vec<u16> = text.encode_utf16().collect();
                let (from, to) = self.target();
                self.splice(from, to, &units);
                let len = units.len() as u32;
                self.composition = (len > 0).then_some((from, from + len));
                let at = from + (*cursor).min(len);
                self.selection = (at, at);
            }
            ImeEvent::Commit { text, .. } => {
                let units: Vec<u16> = text.encode_utf16().collect();
                let (from, to) = self.target();
                self.splice(from, to, &units);
                self.composition = None;
                let at = from + units.len() as u32;
                self.selection = (at, at);
            }
            ImeEvent::DeleteSurrounding { before, after, .. } => {
                // The composition keeps what is left of it.
                let (start, end) = self.selection;
                let after_end = end.saturating_add(*after).min(self.len());
                self.splice(end, after_end, &[]);
                self.splice(start.saturating_sub(*before), start, &[]);
            }
            ImeEvent::SetSelection { start, end, .. } => {
                let (a, b) = (self.local(*start), self.local(*end));
                self.selection = (a.min(b), a.max(b));
            }
            ImeEvent::SetComposingRegion { start, end, .. } => {
                let (a, b) = (self.local(*start), self.local(*end));
                self.composition = (a < b).then_some((a.min(b), a.max(b)));
            }
        }
    }

    /// Whether `other` describes the same editor: the same selection and composition, and the
    /// same text wherever both reach. A side where either stops short of
    /// [`EDITOR_REACH_UNITS`] from the selection is where the editor's text ends, so both must
    /// end there.
    pub fn agrees_with(&self, other: &EditorText) -> bool {
        let absolute = |text: &EditorText, (a, b): (u32, u32)| {
            (
                u64::from(text.start) + u64::from(a),
                u64::from(text.start) + u64::from(b),
            )
        };
        if absolute(self, self.selection) != absolute(other, other.selection)
            || self.composition.map(|range| absolute(self, range))
                != other.composition.map(|range| absolute(other, range))
        {
            return false;
        }
        let begin = |text: &EditorText| u64::from(text.start);
        let end = |text: &EditorText| u64::from(text.start) + u64::from(text.len());
        let reaches_back = |text: &EditorText| text.selection.0 >= EDITOR_REACH_UNITS;
        let reaches_on = |text: &EditorText| text.len() - text.selection.1 >= EDITOR_REACH_UNITS;
        if begin(self) != begin(other) && !(reaches_back(self) && reaches_back(other)) {
            return false;
        }
        if end(self) != end(other) && !(reaches_on(self) && reaches_on(other)) {
            return false;
        }
        let from = begin(self).max(begin(other));
        let to = end(self).min(end(other));
        let part = |text: &EditorText| {
            let a = (from - begin(text)) as usize;
            let b = (to - begin(text)) as usize;
            text.units.get(a..b).map(<[u16]>::to_vec)
        };
        from <= to && part(self) == part(other)
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

/// Where the cursor of the editor focused in a task is, in pixels from the top left corner: the
/// task's own as the guest reports it, which the window manager turns into its window's
/// (the same, unless the window shows the task at another scale) before a window hears of it.
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
    fn fr6_a_deletion_around_the_cursor_crosses_the_local_api_and_reaches_the_guest() {
        let event = ImeEvent::DeleteSurrounding {
            task_id: 3,
            before: 2,
            after: 1,
        };
        let wire::envelope::Body::ImeDeleteSurrounding(delete) = event.to_wire() else {
            panic!("not a deletion");
        };
        assert_eq!(ImeEvent::from_wire_delete(&delete), Ok(event.clone()));
        assert!(matches!(
            event.to_control(),
            control::envelope::Body::ImeDeleteSurrounding(control::ImeDeleteSurrounding {
                task_id: 3,
                before_length: 2,
                after_length: 1,
            })
        ));
        let nothing = wire::ImeDeleteSurrounding {
            task_id: 3,
            before_length: 0,
            after_length: 0,
        };
        assert!(ImeEvent::from_wire_delete(&nothing).is_err());
    }

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

#[cfg(test)]
mod editor_tests {
    use super::*;

    fn editor(text: &str, start: u32, selection: (u32, u32)) -> ImeEditor {
        ImeEditor {
            kind: InputKind::Text,
            text: text.into(),
            text_start: start,
            selection,
            composition: None,
        }
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn fr6_an_editor_description_crosses_the_local_api_unchanged() {
        let described = ImeEditor {
            kind: InputKind::Email,
            text: "\u{d55c}a\u{1f600}".into(),
            text_start: 40,
            // Selected backwards: the active end comes first.
            selection: (44, 41),
            composition: Some((40, 41)),
        };
        assert_eq!(
            ImeEditor::from_wire(&described.to_wire(9)),
            Ok((9, described.clone()))
        );
        let none = ImeEditor::none();
        assert_eq!(ImeEditor::from_wire(&none.to_wire(9)), Ok((9, none)));
    }

    #[test]
    fn fr6_an_editor_description_that_makes_no_sense_is_refused() {
        // A selection past the end of the text.
        assert!(ImeEditor::checked(2, "ab", 10, (10, 13), (0, 0)).is_err());
        // A selection before its start.
        assert!(ImeEditor::checked(2, "ab", 10, (9, 9), (0, 0)).is_err());
        // A composition outside the text.
        assert!(ImeEditor::checked(2, "ab", 10, (11, 11), (3, 5)).is_err());
        // More text than one description carries.
        let long = "a".repeat(MAX_EDITOR_UNITS + 1);
        assert!(ImeEditor::checked(2, &long, 0, (0, 0), (0, 0)).is_err());
        // An empty composition is none, wherever it is.
        assert_eq!(
            ImeEditor::checked(2, "ab", 10, (11, 11), (0, 0)).map(|e| e.composition),
            Ok(None)
        );
    }

    #[test]
    fn fr6_an_unknown_input_kind_is_ordinary_text_and_every_kind_keeps_its_number() {
        assert_eq!(InputKind::from_number(0), InputKind::Text);
        assert_eq!(InputKind::from_number(99), InputKind::Text);
        for kind in [
            InputKind::None,
            InputKind::Text,
            InputKind::Password,
            InputKind::Number,
            InputKind::NumberPassword,
            InputKind::Phone,
            InputKind::Email,
            InputKind::Url,
            InputKind::PersonName,
            InputKind::Date,
            InputKind::Time,
            InputKind::Search,
        ] {
            assert_eq!(InputKind::from_number(kind.number()), kind);
        }
        assert!(InputKind::Password.is_secret() && InputKind::NumberPassword.is_secret());
        assert!(!InputKind::None.takes_text());
    }

    #[test]
    fn fr6_an_editor_that_takes_no_text_describes_none() {
        let described = ImeEditor::checked(1, "ignored", 0, (0, 0), (0, 0)).unwrap();
        assert_eq!(described.kind, InputKind::None);
        assert!(described.text.is_empty());
    }

    #[test]
    fn fr6_the_selection_and_the_composing_region_cross_the_local_api_and_reach_the_guest() {
        let select = ImeEvent::SetSelection {
            task_id: 3,
            start: 4,
            end: 2,
        };
        let wire::envelope::Body::ImeSetSelection(selection) = select.to_wire() else {
            panic!("not a selection");
        };
        assert_eq!(
            ImeEvent::from_wire_selection(&selection),
            Ok(select.clone())
        );
        assert!(matches!(
            select.to_control(),
            control::envelope::Body::ImeSetSelection(control::ImeSetSelection {
                task_id: 3,
                start: 4,
                end: 2,
            })
        ));
        let region = ImeEvent::SetComposingRegion {
            task_id: 3,
            start: 1,
            end: 2,
        };
        let wire::envelope::Body::ImeSetComposingRegion(wired) = region.to_wire() else {
            panic!("not a composing region");
        };
        assert_eq!(
            ImeEvent::from_wire_composing_region(&wired),
            Ok(region.clone())
        );
        assert!(matches!(
            region.to_control(),
            control::envelope::Body::ImeSetComposingRegion(control::ImeSetComposingRegion {
                task_id: 3,
                start: 1,
                end: 2,
            })
        ));
        // A region that ends before it starts, and an offset no editor holds.
        assert!(
            ImeEvent::from_wire_composing_region(&wire::ImeSetComposingRegion {
                task_id: 3,
                start: 2,
                end: 1,
            })
            .is_err()
        );
        assert!(ImeEvent::from_wire_selection(&wire::ImeSetSelection {
            task_id: 3,
            start: u32::MAX,
            end: 0,
        })
        .is_err());
    }

    #[test]
    fn fr6_the_editor_text_changes_with_each_event_as_the_agent_applies_it() {
        let mut text = EditorText::of(&editor("ac", 100, (101, 101)));
        let compose = |text: &str, cursor| ImeEvent::Composition {
            task_id: 1,
            text: text.into(),
            cursor,
        };
        // A composition goes in at the cursor and replaces itself.
        text.apply(&compose("\u{314e}", 1));
        text.apply(&compose("\u{d55c}", 1));
        assert_eq!(text.units, units("a\u{d55c}c"));
        assert_eq!(text.composition, Some((1, 2)));
        assert_eq!(text.selection, (2, 2));
        // A commit takes its place.
        text.apply(&ImeEvent::Commit {
            task_id: 1,
            text: "\u{d55c}".into(),
        });
        assert_eq!(text.composition, None);
        // Reconverting the syllable: it becomes the composition, which is replaced.
        text.apply(&ImeEvent::SetComposingRegion {
            task_id: 1,
            start: 101,
            end: 102,
        });
        text.apply(&compose("\u{6f22}", 1));
        assert_eq!(text.units, units("a\u{6f22}c"));
        text.apply(&ImeEvent::SetComposingRegion {
            task_id: 1,
            start: 102,
            end: 102,
        });
        assert_eq!(text.composition, None, "ended, its text kept");
        assert_eq!(text.units, units("a\u{6f22}c"));
        // A selection replaced by a commit, then a deletion before the cursor.
        text.apply(&ImeEvent::SetSelection {
            task_id: 1,
            start: 103,
            end: 100,
        });
        assert_eq!(text.selection, (0, 3));
        text.apply(&ImeEvent::Commit {
            task_id: 1,
            text: "xyz".into(),
        });
        text.apply(&ImeEvent::DeleteSurrounding {
            task_id: 1,
            before: 1,
            after: 0,
        });
        assert_eq!(text.units, units("xy"));
        assert_eq!(text.selection, (2, 2));
        // An empty composition removes what it showed.
        text.apply(&compose("q", 1));
        text.apply(&compose("", 0));
        assert_eq!(text.units, units("xy"));
        assert_eq!(text.composition, None);
    }

    #[test]
    fn fr6_two_descriptions_of_one_editor_agree_however_far_their_windows_reach() {
        let reach = EDITOR_REACH_UNITS as usize;
        let long = "a".repeat(reach * 3);
        let cursor = reach as u32 + 10;
        // The agent's window moved with the cursor in a long text.
        let early = EditorText::of(&editor(&long[..reach * 2 + 20], 0, (cursor, cursor)));
        let later = EditorText::of(&editor(&long[5..reach * 2 + 50], 5, (cursor, cursor)));
        assert!(early.agrees_with(&later));
        // Near the start of the text both windows end where it does, so text added there
        // differs.
        let short = EditorText::of(&editor("ab", 0, (2, 2)));
        let grown = EditorText::of(&editor("ab)", 0, (2, 2)));
        assert!(!short.agrees_with(&grown));
        // A different selection is a different editor.
        assert!(!short.agrees_with(&EditorText::of(&editor("ab", 0, (1, 1)))));
        assert!(short.agrees_with(&short.clone()));
    }
}

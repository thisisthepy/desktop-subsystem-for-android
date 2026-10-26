//! Text input in a task window: the system input method's composition and the characters the
//! keys type, as the platform-neutral input that `TextInput` turns into the guest's text.
//!
//! The window takes the input method through IMM32: the composition (`WM_IME_COMPOSITION`
//! with `GCS_COMPSTR` and `GCS_CURSORPOS`), what it commits (`GCS_RESULTSTR`), its end
//! (`WM_IME_ENDCOMPOSITION`), and every character typed without it (`WM_CHAR`). On current
//! Windows these messages come from the text services framework itself, which serves every
//! input method to an IMM32 window, the Microsoft Korean IME included, so the window sees the
//! same composition a text services client would.
//!
//! The guest's editor shows the composition itself, underlined where the text is, so the
//! window hides the input method's own composition window. The candidate list (Hanja, say)
//! is the input method's, and the window places it beside the cursor of the guest's editor,
//! which the guest reports in the task's pixels; those are the window's client pixels.
//!
//! A key that types a character goes as text alone: `KeyWindow` gives the guest no key event
//! for it (see [`super::keys`]). A control character (Enter, Tab, Backspace, Ctrl with a
//! letter) went as a key event, and `TextInput` drops it here.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;

use dsa_host_core::ime::{ImeCursor, ImeEvent, TextInput};
use dsa_host_core::input::InputEvent;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionWindow, CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT,
    COMPOSITIONFORM, CPS_COMPLETE, GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, HIMC,
    ISC_SHOWUICOMPOSITIONWINDOW, NI_COMPOSITIONSTR,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, WM_CHAR, WM_IME_CHAR, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KILLFOCUS,
};

/// What one `WM_IME_COMPOSITION` changed, read from the input method.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompositionChange {
    /// The text the input method committed, when it committed some.
    pub result: Option<String>,
    /// The composition now, with its cursor in UTF-16 code units, when it changed.
    pub composition: Option<(String, u32)>,
}

impl CompositionChange {
    /// Reads what a `WM_IME_COMPOSITION` whose `lparam` is `flags` changed, through `read`,
    /// which gives the input method's string of one kind (`GCS_RESULTSTR`, `GCS_COMPSTR`), and
    /// `cursor`, which gives the cursor in the composition. A message with no flags at all is
    /// a composition the input method canceled.
    pub fn read(
        flags: u32,
        read: impl Fn(u32) -> Option<Vec<u16>>,
        cursor: impl Fn() -> Option<u32>,
    ) -> CompositionChange {
        let text = |kind| read(kind).map(|units| String::from_utf16_lossy(&units));
        let result = if flags & GCS_RESULTSTR != 0 {
            text(GCS_RESULTSTR)
        } else {
            None
        };
        let composition = if flags & GCS_COMPSTR != 0 {
            text(GCS_COMPSTR).map(|composing| {
                let end = composing.encode_utf16().count() as u32;
                let at = if flags & GCS_CURSORPOS != 0 {
                    cursor().unwrap_or(end)
                } else {
                    end
                };
                (composing, at)
            })
        } else if flags == 0 {
            Some((String::new(), 0))
        } else {
            None
        };
        CompositionChange {
            result,
            composition,
        }
    }

    /// What the guest is told, in order: the commit first, then the composition that follows
    /// it (Korean commits a syllable and starts the next in one message).
    pub fn apply(self, input: &mut TextInput) -> Vec<ImeEvent> {
        let mut events = Vec::new();
        if let Some(result) = self.result {
            events.extend(input.commit(&result));
        }
        if let Some((text, cursor)) = self.composition {
            events.extend(input.composition(&text, cursor));
        }
        events
    }
}

/// Where the input method's candidate list goes for a cursor: below it, and never over it.
pub fn candidate_form(cursor: &ImeCursor) -> CANDIDATEFORM {
    CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_EXCLUDE,
        ptCurrentPos: POINT {
            x: cursor.left,
            y: cursor.bottom,
        },
        rcArea: RECT {
            left: cursor.left,
            top: cursor.top,
            right: cursor.right,
            bottom: cursor.bottom,
        },
    }
}

/// Where the input method's composition window would go for a cursor, for an input method
/// that draws one even when asked not to: at the cursor.
pub fn composition_form(cursor: &ImeCursor) -> COMPOSITIONFORM {
    COMPOSITIONFORM {
        dwStyle: CFS_POINT,
        ptCurrentPos: POINT {
            x: cursor.left,
            y: cursor.top,
        },
        rcArea: RECT::default(),
    }
}

/// The input context of a window, released when dropped.
struct Context {
    hwnd: HWND,
    himc: HIMC,
}

impl Context {
    /// # Safety
    ///
    /// Called on the window's own thread.
    unsafe fn of(hwnd: HWND) -> Option<Context> {
        let himc = ImmGetContext(hwnd);
        (!himc.is_null()).then_some(Context { hwnd, himc })
    }

    /// The input method's string of one kind, in UTF-16 code units.
    unsafe fn string(&self, kind: u32) -> Option<Vec<u16>> {
        let bytes = ImmGetCompositionStringW(self.himc, kind, std::ptr::null_mut(), 0);
        if bytes < 0 {
            return None;
        }
        let mut units = vec![0u16; bytes as usize / 2];
        if !units.is_empty() {
            let read =
                ImmGetCompositionStringW(self.himc, kind, units.as_mut_ptr().cast(), bytes as u32);
            if read < 0 {
                return None;
            }
            units.truncate(read as usize / 2);
        }
        Some(units)
    }

    /// The cursor in the composition, in UTF-16 code units.
    unsafe fn cursor(&self) -> Option<u32> {
        let at = ImmGetCompositionStringW(self.himc, GCS_CURSORPOS, std::ptr::null_mut(), 0);
        (at >= 0).then_some(at as u32)
    }

    unsafe fn place(&self, cursor: &ImeCursor) {
        ImmSetCandidateWindow(self.himc, &candidate_form(cursor));
        ImmSetCompositionWindow(self.himc, &composition_form(cursor));
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the context this window was given on its own thread.
        unsafe { ImmReleaseContext(self.hwnd, self.himc) };
    }
}

/// One task window's text input.
pub struct ImeWindow {
    text: RefCell<TextInput>,
    events: Sender<InputEvent>,
    /// Where the guest last said the cursor of the task's focused editor is.
    cursor: Cell<Option<ImeCursor>>,
}

impl ImeWindow {
    pub fn new(task_id: i32, events: Sender<InputEvent>) -> Self {
        ImeWindow {
            text: RefCell::new(TextInput::new(task_id)),
            events,
            cursor: Cell::new(None),
        }
    }

    /// Where the guest last said the cursor of the task's focused editor is.
    pub fn cursor(&self) -> Option<ImeCursor> {
        self.cursor.get()
    }

    /// The guest says the cursor of the task's focused editor is at `cursor`: the input
    /// method's windows go beside it from now on.
    ///
    /// # Safety
    ///
    /// Called on the window's own thread.
    pub unsafe fn cursor_moved(&self, hwnd: HWND, cursor: ImeCursor) {
        self.cursor.set(Some(cursor));
        if let Some(context) = Context::of(hwnd) {
            context.place(&cursor);
        }
    }

    fn send(&self, events: impl IntoIterator<Item = ImeEvent>) {
        for event in events {
            // The sender ends only when the window host stops; input then has nowhere to go.
            let _ = self.events.send(InputEvent::Ime(event));
        }
    }

    fn with_text<T>(&self, work: impl FnOnce(&mut TextInput) -> T) -> Option<T> {
        self.text
            .try_borrow_mut()
            .ok()
            .map(|mut text| work(&mut text))
    }

    /// Handles an input method message or a typed character. `None` for any other message,
    /// or one that Windows must also see.
    ///
    /// # Safety
    ///
    /// Called on the window's own thread, from its window procedure.
    pub unsafe fn handle(
        &self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            // The guest's editor shows the composition; the input method keeps its candidate
            // list and the rest of its windows.
            WM_IME_SETCONTEXT => {
                let shown = (lparam as u32 & !ISC_SHOWUICOMPOSITIONWINDOW) as LPARAM;
                Some(DefWindowProcW(hwnd, message, wparam, shown))
            }
            // Not passed on, so Windows opens no composition window of its own.
            WM_IME_STARTCOMPOSITION => {
                if let (Some(cursor), Some(context)) = (self.cursor.get(), Context::of(hwnd)) {
                    context.place(&cursor);
                }
                Some(0)
            }
            WM_IME_COMPOSITION => {
                let flags = lparam as u32;
                let change = match Context::of(hwnd) {
                    Some(context) => {
                        if let Some(cursor) = self.cursor.get() {
                            context.place(&cursor);
                        }
                        CompositionChange::read(
                            flags,
                            |kind| context.string(kind),
                            || context.cursor(),
                        )
                    }
                    None => CompositionChange::read(flags, |_| None, || None),
                };
                if let Some(events) = self.with_text(|text| change.apply(text)) {
                    self.send(events);
                }
                // Not passed on: Windows would type the result again as WM_IME_CHAR.
                Some(0)
            }
            WM_IME_ENDCOMPOSITION => {
                if let Some(ended) = self.with_text(TextInput::end) {
                    self.send(ended);
                }
                Some(0)
            }
            // The commit already carried these characters.
            WM_IME_CHAR => Some(0),
            WM_CHAR => {
                if let Some(typed) = self.with_text(|text| text.character(wparam as u16)) {
                    self.send(typed);
                }
                Some(0)
            }
            WM_KILLFOCUS => {
                // What is being composed is typed as it stands, as in any other window,
                // rather than left in the guest's editor half made.
                let composing = self.with_text(|text| text.is_composing()) == Some(true);
                if composing {
                    if let Some(context) = Context::of(hwnd) {
                        ImmNotifyIME(context.himc, NI_COMPOSITIONSTR, CPS_COMPLETE, 0);
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Option<Vec<u16>> {
        Some(text.encode_utf16().collect())
    }

    #[test]
    fn fr6_a_korean_syllable_committed_with_the_next_begun_reaches_the_guest_in_that_order() {
        let mut input = TextInput::new(3);
        input.composition("\u{d55c}", 1);
        let change = CompositionChange::read(
            GCS_RESULTSTR | GCS_COMPSTR | GCS_CURSORPOS,
            |kind| match kind {
                GCS_RESULTSTR => units("\u{d55c}"),
                GCS_COMPSTR => units("\u{3131}"),
                _ => None,
            },
            || Some(1),
        );
        assert_eq!(
            change.apply(&mut input),
            vec![
                ImeEvent::Commit {
                    task_id: 3,
                    text: "\u{d55c}".into()
                },
                ImeEvent::Composition {
                    task_id: 3,
                    text: "\u{3131}".into(),
                    cursor: 1
                },
            ]
        );
    }

    #[test]
    fn fr6_a_composition_without_a_cursor_has_it_at_its_end() {
        let change = CompositionChange::read(GCS_COMPSTR, |_| units("ab"), || Some(0));
        assert_eq!(change.composition, Some(("ab".into(), 2)));
        let change =
            CompositionChange::read(GCS_COMPSTR | GCS_CURSORPOS, |_| units("ab"), || Some(1));
        assert_eq!(change.composition, Some(("ab".into(), 1)));
    }

    #[test]
    fn fr6_a_message_with_no_flags_cancels_the_composition() {
        let mut input = TextInput::new(3);
        input.composition("\u{3131}", 1);
        let change = CompositionChange::read(0, |_| None, || None);
        assert_eq!(
            change.apply(&mut input),
            vec![ImeEvent::Composition {
                task_id: 3,
                text: String::new(),
                cursor: 0
            }]
        );
    }

    #[test]
    fn fr6_the_candidate_list_goes_below_the_cursor_and_never_over_it() {
        let cursor = ImeCursor {
            left: 40,
            top: 100,
            right: 42,
            bottom: 130,
        };
        let form = candidate_form(&cursor);
        assert_eq!(form.dwStyle, CFS_EXCLUDE);
        assert_eq!((form.ptCurrentPos.x, form.ptCurrentPos.y), (40, 130));
        assert_eq!(
            (
                form.rcArea.left,
                form.rcArea.top,
                form.rcArea.right,
                form.rcArea.bottom
            ),
            (40, 100, 42, 130)
        );
        let form = composition_form(&cursor);
        assert_eq!((form.ptCurrentPos.x, form.ptCurrentPos.y), (40, 100));
    }
}

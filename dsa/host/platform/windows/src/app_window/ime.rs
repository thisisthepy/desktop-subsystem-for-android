//! Text input in a task window: the system input method's composition and the characters the
//! keys type, as the platform-neutral text input the guest's editor receives.
//!
//! The window takes the input method through the text services framework ([`super::tsf`]):
//! it is a text services client with a document of its own, the text of the guest's focused
//! editor around its selection, which the agent describes ([`super::text_store`]). The input
//! method composes and commits in that document, and each edit reaches the guest as the
//! composition, the commit, the selection or the reconverted range it made. The Microsoft
//! Korean IME composes there, the touch keyboard takes its layout from the editor's input
//! scope, and dictation (Win+H), handwriting and the emoji panel type into it.
//!
//! The guest's editor shows the composition itself, underlined where the text is, so no
//! composition window is drawn over it. The candidate list (Hanja, say) is the input
//! method's, placed beside the cursor of the guest's editor. The guest reports it in the
//! task's pixels, and the window manager gives it in the window's client pixels: the task's
//! at the scale the window shows the task at, told again when that scale changes.
//!
//! A key that types a character without the input method (any keyboard layout, with no input
//! method composing) arrives as `WM_CHAR`, and goes as text alone: `KeyWindow` gives the guest
//! no key event for it (see [`super::keys`]). A control character (Enter, Tab, Backspace, Ctrl
//! with a letter) went as a key event, and the store drops it here.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use dsa_host_core::ime::{ImeCursor, ImeEditor};
use dsa_host_core::input::InputEvent;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_CHAR, WM_KILLFOCUS};

use super::tsf::{Document, Shared};

/// One task window's text input.
pub struct ImeWindow {
    shared: Rc<Shared>,
    /// The framework's document for the window, once the window exists; none when the window
    /// thread has no framework, and the keys then still type.
    document: RefCell<Option<Document>>,
}

impl ImeWindow {
    pub fn new(task_id: i32, events: Sender<InputEvent>) -> Self {
        ImeWindow {
            shared: Shared::new(task_id, events),
            document: RefCell::new(None),
        }
    }

    /// Gives the window `hwnd` its document, so the input method composes in it. Called once
    /// the window exists, on its thread. The reason it could not, for the log.
    pub fn attach(&self, hwnd: HWND) -> Result<(), String> {
        let document = Document::attach(hwnd as isize, &self.shared)?;
        if let Ok(mut slot) = self.document.try_borrow_mut() {
            *slot = Some(document);
        }
        Ok(())
    }

    /// Lets the document go, as the window closes.
    pub fn detach(&self) {
        let document = self
            .document
            .try_borrow_mut()
            .ok()
            .and_then(|mut slot| slot.take());
        drop(document);
    }

    /// Where the guest last said the cursor of the task's focused editor is.
    pub fn cursor(&self) -> Option<ImeCursor> {
        self.shared.cursor()
    }

    /// What the guest last said of the task's focused editor.
    pub fn editor(&self) -> Option<ImeEditor> {
        self.shared.described()
    }

    /// The guest says the cursor of the task's focused editor is at `cursor`: the input
    /// method's windows go beside it from now on.
    pub fn cursor_moved(&self, cursor: ImeCursor) {
        self.shared.cursor_moved(cursor);
    }

    /// The guest described the task's focused editor: the document becomes its text, and the
    /// input scope follows its kind.
    pub fn editor_changed(&self, editor: ImeEditor) {
        let document = self.document.try_borrow().ok();
        match document.as_ref().and_then(|document| document.as_ref()) {
            Some(document) => document.described(editor),
            // No framework: the description is kept for what it says of the editor.
            None => self.shared.described_without_framework(editor),
        }
    }

    /// Handles a typed character, and ends a composition as the window loses the focus.
    /// `None` for any other message, or one that Windows must also see.
    ///
    /// # Safety
    ///
    /// Called on the window's own thread, from its window procedure.
    pub unsafe fn handle(
        &self,
        _hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_CHAR => {
                self.shared.typed(wparam as u16);
                Some(0)
            }
            WM_KILLFOCUS => {
                // What is being composed is typed as it stands, as in any other window,
                // rather than left in the guest's editor half made.
                if let Ok(document) = self.document.try_borrow() {
                    if let Some(document) = document.as_ref() {
                        document.end_composition();
                    }
                }
                None
            }
            _ => None,
        }
    }
}

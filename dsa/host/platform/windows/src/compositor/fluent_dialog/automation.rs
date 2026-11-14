//! The dialog's UI Automation tree, so Narrator and every other client read and operate it as
//! they do a Win32 dialog box.
//!
//! The dialog's window is a Window element that is a dialog (`IsDialog`), named by its title.
//! Its children, in reading order, are a Text with the message, the CheckBox (Toggle pattern)
//! and the two Buttons (Invoke pattern), the primary one marked as the default button as a
//! Win32 dialog marks its default push button (`STATE_SYSTEM_DEFAULT`). Each control says its
//! access key, whether it has the keyboard focus and where it is on the screen. The dialog
//! raises a focus change whenever the focus moves, and a toggle state change when the box is
//! ticked or cleared.
//!
//! The providers read a [`View`] the dialog writes after every change; a pattern call goes to
//! the view's [`ActionSink`], which the dialog's window turns into a posted message, so the
//! action is done on the dialog's own thread as a key would be.

// UI Automation's constants keep their Windows names, which are matched on below.
#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::{implement, IUnknown, IUnknownImpl, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;

use super::{Control, DialogLayout, DialogState, DialogText};
use crate::app_window::accessibility::{
    none, not_available, safe_array, variant_bool, variant_empty, variant_i4, variant_str,
};
use crate::ui_automation::control_type;

/// UI Automation's Window control type, which a dialog is.
const WINDOW: i32 = 50032;
/// MSAA's state of a dialog's default push button, which screen readers announce.
pub const STATE_SYSTEM_DEFAULT: i32 = 0x100;
/// MSAA's state of a control that can take the focus.
const STATE_SYSTEM_FOCUSABLE: i32 = 0x0010_0000;
/// MSAA's state of the control with the focus.
const STATE_SYSTEM_FOCUSED: i32 = 0x4;
/// MSAA's state of a ticked check box.
const STATE_SYSTEM_CHECKED: i32 = 0x10;

/// What a UI Automation client asked the dialog to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Invoke a button, or toggle the check box.
    Activate(Control),
    /// Move the keyboard focus to a control.
    Focus(Control),
}

impl Action {
    /// The action as a posted message's `wparam`.
    pub fn code(self) -> usize {
        let (kind, control) = match self {
            Action::Activate(control) => (1, control),
            Action::Focus(control) => (2, control),
        };
        let control = match control {
            Control::Check => 0,
            Control::Primary => 1,
            Control::Secondary => 2,
        };
        (kind << 4) | control
    }

    pub fn from_code(code: usize) -> Option<Action> {
        let control = match code & 0xf {
            0 => Control::Check,
            1 => Control::Primary,
            2 => Control::Secondary,
            _ => return None,
        };
        match code >> 4 {
            1 => Some(Action::Activate(control)),
            2 => Some(Action::Focus(control)),
            _ => None,
        }
    }
}

/// Where the providers send what a client asked for.
pub type ActionSink = Arc<dyn Fn(Action) + Send + Sync>;

/// What an element of the dialog is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The message under the title.
    Message,
    Control(Control),
}

/// One element under the dialog, as UI Automation reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewElement {
    pub kind: Kind,
    pub name: String,
    pub access_key: Option<char>,
    /// Left, top, width and height on the screen.
    pub bounds: (i32, i32, i32, i32),
    pub focused: bool,
    /// The check box's state; None for every other element.
    pub toggled: Option<bool>,
    /// Whether it is the default button, which Enter presses.
    pub default: bool,
}

/// What the dialog's providers read.
#[derive(Default)]
pub struct View {
    /// False once the dialog closed: every provider then answers that it is gone.
    pub alive: bool,
    /// The dialog's window.
    pub hwnd: isize,
    /// The dialog's name: its title.
    pub name: String,
    pub elements: Vec<ViewElement>,
    /// Tells this dialog's elements from an earlier dialog's in their runtime IDs.
    pub epoch: u32,
    pub actions: Option<ActionSink>,
}

impl View {
    /// The view of a dialog that is opening, whose actions go to `actions`.
    pub fn open(hwnd: isize, actions: ActionSink) -> View {
        static EPOCH: AtomicU32 = AtomicU32::new(1);
        View {
            alive: true,
            hwnd,
            epoch: EPOCH.fetch_add(1, Ordering::Relaxed),
            actions: Some(actions),
            ..View::default()
        }
    }

    /// The element of `control`, by its place among the elements.
    pub fn index_of(&self, control: Control) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| e.kind == Kind::Control(control))
    }
}

/// The elements of the dialog `text` laid out as `laid`, in the state `state`, with the
/// window's top left corner at `origin` on the screen.
pub fn elements(
    text: &DialogText,
    laid: &DialogLayout,
    state: &DialogState,
    origin: (i32, i32),
) -> Vec<ViewElement> {
    let on_screen = |r: super::Rect| (origin.0 + r.x, origin.1 + r.y, r.w, r.h);
    let mut out = Vec::with_capacity(4);
    let message = match (laid.lines.first(), laid.lines.last()) {
        (Some((_, first)), Some((_, last))) => (
            origin.0 + first.x,
            origin.1 + first.y,
            first.w,
            last.y + last.h - first.y,
        ),
        _ => on_screen(laid.title),
    };
    out.push(ViewElement {
        kind: Kind::Message,
        name: text.body.clone(),
        access_key: None,
        bounds: message,
        focused: false,
        toggled: None,
        default: false,
    });
    if let (Some(label), Some(row)) = (&text.check, laid.check_row) {
        out.push(ViewElement {
            kind: Kind::Control(Control::Check),
            name: label.clone(),
            access_key: text.keys[0],
            bounds: on_screen(row),
            focused: state.focus == Control::Check,
            toggled: Some(state.checked),
            default: false,
        });
    }
    for (control, name, key, at) in [
        (Control::Primary, &text.primary, text.keys[1], laid.primary),
        (
            Control::Secondary,
            &text.secondary,
            text.keys[2],
            laid.secondary,
        ),
    ] {
        out.push(ViewElement {
            kind: Kind::Control(control),
            name: name.clone(),
            access_key: key,
            bounds: on_screen(at),
            focused: state.focus == control,
            toggled: None,
            default: control == Control::Primary,
        });
    }
    out
}

fn lock(view: &Mutex<View>) -> MutexGuard<'_, View> {
    view.lock().unwrap_or_else(|e| e.into_inner())
}

/// The dialog's window: a Window that is a dialog.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct DialogProvider {
    view: Arc<Mutex<View>>,
}

/// One element under the dialog.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    IToggleProvider
)]
struct ElementProvider {
    view: Arc<Mutex<View>>,
    index: usize,
}

fn root_of(view: &Arc<Mutex<View>>) -> IRawElementProviderFragmentRoot {
    DialogProvider {
        view: Arc::clone(view),
    }
    .into()
}

fn element_of(view: &Arc<Mutex<View>>, index: usize) -> IRawElementProviderFragment {
    ElementProvider {
        view: Arc::clone(view),
        index,
    }
    .into()
}

/// The provider the dialog's window answers `WM_GETOBJECT` with.
pub fn dialog_provider(view: &Arc<Mutex<View>>) -> IRawElementProviderSimple {
    DialogProvider {
        view: Arc::clone(view),
    }
    .into()
}

/// The provider of the element of `control`, when the dialog has it.
pub fn control_provider(
    view: &Arc<Mutex<View>>,
    control: Control,
) -> Option<IRawElementProviderSimple> {
    let index = lock(view).index_of(control)?;
    element_of(view, index).cast().ok()
}

/// Answers `WM_GETOBJECT` for UI Automation's root object; None for anything else.
pub fn answer(
    hwnd: isize,
    wparam: usize,
    lparam: isize,
    provider: &IRawElementProviderSimple,
) -> Option<isize> {
    if lparam as i32 != UiaRootObjectId {
        return None;
    }
    // SAFETY: the documented answer to WM_GETOBJECT, for the window that got it.
    let result = unsafe {
        UiaReturnRawElementProvider(
            HWND(hwnd as *mut _),
            WPARAM(wparam),
            LPARAM(lparam),
            provider,
        )
    };
    Some(result.0)
}

/// The window is closing: UI Automation lets go of its provider.
pub fn release(hwnd: isize, provider: Option<IRawElementProviderSimple>) {
    // SAFETY: the documented way for a window that answered WM_GETOBJECT to say it is going.
    unsafe {
        UiaReturnRawElementProvider(
            HWND(hwnd as *mut _),
            WPARAM(0),
            LPARAM(0),
            None::<&IRawElementProviderSimple>,
        );
        if let Some(provider) = provider {
            let _ = UiaDisconnectProvider(&provider);
        }
    }
}

fn listening() -> bool {
    // SAFETY: a plain query.
    unsafe { UiaClientsAreListening() }.as_bool()
}

/// Says that the dialog opened.
pub fn raise_opened(view: &Arc<Mutex<View>>) {
    if listening() {
        let provider = dialog_provider(view);
        // SAFETY: an event of the dialog's own provider.
        let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_Window_WindowOpenedEventId) };
    }
}

/// Says that the keyboard focus is on `control` now.
pub fn raise_focus(view: &Arc<Mutex<View>>, control: Control) {
    if !listening() {
        return;
    }
    if let Some(provider) = control_provider(view, control) {
        // SAFETY: as above.
        let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId) };
    }
}

/// Says that the check box was ticked (`checked`) or cleared.
pub fn raise_toggled(view: &Arc<Mutex<View>>, checked: bool) {
    if !listening() {
        return;
    }
    if let Some(provider) = control_provider(view, Control::Check) {
        let (old, new) = (
            variant_i4(toggle_state(!checked).0),
            variant_i4(toggle_state(checked).0),
        );
        // SAFETY: as above; the values live through the call.
        let _ = unsafe {
            UiaRaiseAutomationPropertyChangedEvent(
                &provider,
                UIA_ToggleToggleStatePropertyId,
                &old,
                &new,
            )
        };
    }
}

fn toggle_state(checked: bool) -> ToggleState {
    if checked {
        ToggleState_On
    } else {
        ToggleState_Off
    }
}

impl IRawElementProviderSimple_Impl for DialogProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        none()
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        let view = lock(&self.view);
        if !view.alive {
            return Ok(variant_empty());
        }
        Ok(match property {
            UIA_ControlTypePropertyId => variant_i4(WINDOW),
            UIA_IsDialogPropertyId => variant_bool(true),
            UIA_LocalizedControlTypePropertyId => variant_str("dialog"),
            UIA_NamePropertyId => variant_str(&view.name),
            UIA_IsKeyboardFocusablePropertyId => variant_bool(false),
            UIA_IsControlElementPropertyId => variant_bool(true),
            UIA_IsContentElementPropertyId => variant_bool(true),
            _ => variant_empty(),
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        let hwnd = lock(&self.view).hwnd;
        if hwnd == 0 {
            return none();
        }
        // SAFETY: the dialog's own window.
        unsafe { UiaHostProviderFromHwnd(HWND(hwnd as *mut _)) }
    }
}

impl IRawElementProviderFragment_Impl for DialogProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let count = lock(&self.view).elements.len();
        let index = match direction {
            NavigateDirection_FirstChild if count > 0 => 0,
            NavigateDirection_LastChild if count > 0 => count - 1,
            _ => return none(),
        };
        Ok(element_of(&self.view, index))
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        // The window's own provider gives the dialog its runtime ID.
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        // The window's own provider gives the dialog its bounds.
        Ok(UiaRect::default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> windows::core::Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> windows::core::Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for DialogProvider_Impl {
    fn ElementProviderFromPoint(
        &self,
        x: f64,
        y: f64,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let index = lock(&self.view).elements.iter().position(|e| {
            let (left, top, width, height) = e.bounds;
            let (left, top) = (f64::from(left), f64::from(top));
            x >= left && y >= top && x < left + f64::from(width) && y < top + f64::from(height)
        });
        match index {
            Some(index) => Ok(element_of(&self.view, index)),
            None => none(),
        }
    }

    fn GetFocus(&self) -> windows::core::Result<IRawElementProviderFragment> {
        let index = lock(&self.view).elements.iter().position(|e| e.focused);
        match index {
            Some(index) => Ok(element_of(&self.view, index)),
            None => none(),
        }
    }
}

impl ElementProvider {
    /// Reads the element while the dialog is open.
    fn with_element<T>(&self, read: impl FnOnce(&ViewElement) -> T) -> windows::core::Result<T> {
        let view = lock(&self.view);
        if !view.alive {
            return Err(not_available());
        }
        let element = view.elements.get(self.index).ok_or_else(not_available)?;
        Ok(read(element))
    }

    /// Sends `action` on this element's control to the dialog.
    fn act(&self, action: impl FnOnce(Control) -> Action) -> windows::core::Result<()> {
        let kind = self.with_element(|e| e.kind)?;
        let Kind::Control(control) = kind else {
            return Err(not_available());
        };
        let sink = lock(&self.view).actions.clone();
        if let Some(sink) = sink {
            sink(action(control));
        }
        Ok(())
    }
}

impl IRawElementProviderSimple_Impl for ElementProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        let supported = self.with_element(|e| {
            matches!(
                (e.kind, pattern),
                (Kind::Control(Control::Check), UIA_TogglePatternId)
                    | (
                        Kind::Control(Control::Primary | Control::Secondary),
                        UIA_InvokePatternId
                    )
            )
        })?;
        if supported {
            Ok(self.to_interface::<IRawElementProviderSimple>().cast()?)
        } else {
            none()
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        self.with_element(|e| {
            let control = matches!(e.kind, Kind::Control(_));
            let kind = match e.kind {
                Kind::Message => control_type::TEXT,
                Kind::Control(Control::Check) => control_type::CHECK_BOX,
                Kind::Control(_) => control_type::BUTTON,
            };
            let mut state = 0;
            if control {
                state |= STATE_SYSTEM_FOCUSABLE;
            }
            if e.focused {
                state |= STATE_SYSTEM_FOCUSED;
            }
            if e.default {
                state |= STATE_SYSTEM_DEFAULT;
            }
            if e.toggled == Some(true) {
                state |= STATE_SYSTEM_CHECKED;
            }
            match property {
                UIA_ControlTypePropertyId => variant_i4(kind),
                UIA_NamePropertyId => variant_str(&e.name),
                UIA_AccessKeyPropertyId => match e.access_key {
                    Some(key) => variant_str(&format!("Alt+{key}")),
                    None => variant_empty(),
                },
                UIA_IsEnabledPropertyId => variant_bool(true),
                UIA_HasKeyboardFocusPropertyId => variant_bool(e.focused),
                UIA_IsKeyboardFocusablePropertyId => variant_bool(control),
                UIA_IsControlElementPropertyId => variant_bool(true),
                UIA_IsContentElementPropertyId => variant_bool(true),
                UIA_IsOffscreenPropertyId => variant_bool(false),
                UIA_LegacyIAccessibleStatePropertyId => variant_i4(state),
                UIA_ToggleToggleStatePropertyId if e.toggled.is_some() => {
                    variant_i4(toggle_state(e.toggled == Some(true)).0)
                }
                _ => variant_empty(),
            }
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for ElementProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let count = lock(&self.view).elements.len();
        let index = match direction {
            NavigateDirection_Parent => return root_of(&self.view).cast(),
            NavigateDirection_NextSibling if self.index + 1 < count => self.index + 1,
            NavigateDirection_PreviousSibling if self.index > 0 => self.index - 1,
            _ => return none(),
        };
        Ok(element_of(&self.view, index))
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        let epoch = lock(&self.view).epoch;
        safe_array(&[UiaAppendRuntimeId as i32, epoch as i32, self.index as i32])
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        self.with_element(|e| {
            let (left, top, width, height) = e.bounds;
            UiaRect {
                left: f64::from(left),
                top: f64::from(top),
                width: f64::from(width),
                height: f64::from(height),
            }
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> windows::core::Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        self.act(Action::Focus)
    }

    fn FragmentRoot(&self) -> windows::core::Result<IRawElementProviderFragmentRoot> {
        Ok(root_of(&self.view))
    }
}

impl IInvokeProvider_Impl for ElementProvider_Impl {
    fn Invoke(&self) -> windows::core::Result<()> {
        self.act(Action::Activate)
    }
}

impl IToggleProvider_Impl for ElementProvider_Impl {
    fn Toggle(&self) -> windows::core::Result<()> {
        self.act(Action::Activate)
    }

    fn ToggleState(&self) -> windows::core::Result<ToggleState> {
        self.with_element(|e| toggle_state(e.toggled == Some(true)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::fluent_dialog::{layout, Font};
    use windows::Win32::System::Variant::{VT_BOOL, VT_BSTR, VT_I4};

    fn i4(value: &VARIANT) -> Option<i32> {
        // SAFETY: the member read is the one the tag names.
        unsafe {
            let inner = &value.Anonymous.Anonymous;
            (inner.vt == VT_I4).then(|| inner.Anonymous.lVal)
        }
    }

    fn boolean(value: &VARIANT) -> Option<bool> {
        // SAFETY: as above.
        unsafe {
            let inner = &value.Anonymous.Anonymous;
            (inner.vt == VT_BOOL).then(|| inner.Anonymous.boolVal.0 != 0)
        }
    }

    fn string(value: &VARIANT) -> Option<String> {
        // SAFETY: as above.
        unsafe {
            let inner = &value.Anonymous.Anonymous;
            (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
        }
    }

    fn stop_music() -> DialogText {
        DialogText {
            title: "Stop Music?".into(),
            body: "It stops everything it does in the background.".into(),
            check: Some("Do not ask again".into()),
            primary: "Stop".into(),
            secondary: "Cancel".into(),
            keys: [Some('D'), Some('S'), Some('C')],
        }
    }

    /// A dialog's view, laid out with every character 8 px wide, at (100, 200) on the screen,
    /// whose actions are recorded.
    fn open(state: &DialogState) -> (Arc<Mutex<View>>, Arc<Mutex<Vec<Action>>>) {
        let text = stop_music();
        let fixed = |line: &str, _: Font| 8 * line.chars().count() as i32;
        let laid = layout(&text, 96, &fixed);
        let done = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let done = Arc::clone(&done);
            Arc::new(move |action| done.lock().unwrap().push(action)) as ActionSink
        };
        let mut view = View::open(0, sink);
        view.name = text.title.clone();
        view.elements = elements(&text, &laid, state, (100, 200));
        (Arc::new(Mutex::new(view)), done)
    }

    fn children(view: &Arc<Mutex<View>>) -> Vec<IRawElementProviderSimple> {
        let root: IRawElementProviderFragment = dialog_provider(view).cast().unwrap();
        let mut out = Vec::new();
        // SAFETY: calls on the providers made above, on this thread.
        let mut next = unsafe { root.Navigate(NavigateDirection_FirstChild) }.ok();
        while let Some(child) = next {
            out.push(child.cast().unwrap());
            next = unsafe { child.Navigate(NavigateDirection_NextSibling) }.ok();
        }
        out
    }

    #[test]
    fn fr24_the_stop_dialog_is_read_as_a_dialog_by_screen_readers() {
        let state = DialogState::new(true);
        let (view, _) = open(&state);
        let root = dialog_provider(&view);
        // SAFETY: calls on the providers made above, on this thread.
        unsafe {
            let property = |p| root.GetPropertyValue(p).unwrap();
            assert_eq!(i4(&property(UIA_ControlTypePropertyId)), Some(WINDOW));
            assert_eq!(boolean(&property(UIA_IsDialogPropertyId)), Some(true));
            assert_eq!(
                string(&property(UIA_NamePropertyId)).as_deref(),
                Some("Stop Music?")
            );
        }
        // The message, the check box and the two buttons, in reading order.
        let kids = children(&view);
        assert_eq!(kids.len(), 4);
        // SAFETY: as above.
        unsafe {
            let read = |index: usize, p| kids[index].GetPropertyValue(p).unwrap();
            let types: Vec<Option<i32>> = (0..4)
                .map(|i| i4(&read(i, UIA_ControlTypePropertyId)))
                .collect();
            assert_eq!(
                types,
                [
                    Some(control_type::TEXT),
                    Some(control_type::CHECK_BOX),
                    Some(control_type::BUTTON),
                    Some(control_type::BUTTON)
                ]
            );
            let names: Vec<Option<String>> = (0..4)
                .map(|i| string(&read(i, UIA_NamePropertyId)))
                .collect();
            assert_eq!(names[0].as_deref(), Some(stop_music().body.as_str()));
            assert_eq!(names[1].as_deref(), Some("Do not ask again"));
            assert_eq!(names[2].as_deref(), Some("Stop"));
            assert_eq!(names[3].as_deref(), Some("Cancel"));
            // The access keys, and the default button.
            assert_eq!(
                string(&read(1, UIA_AccessKeyPropertyId)).as_deref(),
                Some("Alt+D")
            );
            assert_eq!(
                string(&read(2, UIA_AccessKeyPropertyId)).as_deref(),
                Some("Alt+S")
            );
            let state_of = |i: usize| i4(&read(i, UIA_LegacyIAccessibleStatePropertyId)).unwrap();
            assert_ne!(state_of(2) & STATE_SYSTEM_DEFAULT, 0);
            assert_eq!(state_of(3) & STATE_SYSTEM_DEFAULT, 0);
            // The focus is on the default button as the dialog opens.
            assert_eq!(
                boolean(&read(2, UIA_HasKeyboardFocusPropertyId)),
                Some(true)
            );
            assert_eq!(
                boolean(&read(1, UIA_HasKeyboardFocusPropertyId)),
                Some(false)
            );
            assert_eq!(
                boolean(&read(0, UIA_IsKeyboardFocusablePropertyId)),
                Some(false)
            );
            let root: IRawElementProviderFragmentRoot = root.cast().unwrap();
            let focus: IRawElementProviderSimple = root.GetFocus().unwrap().cast().unwrap();
            assert_eq!(
                string(&focus.GetPropertyValue(UIA_NamePropertyId).unwrap()).as_deref(),
                Some("Stop")
            );
            // Every child is on the screen where the dialog draws it, and its parent is
            // the dialog.
            let fragment: IRawElementProviderFragment = kids[2].cast().unwrap();
            let rect = fragment.BoundingRectangle().unwrap();
            assert!(rect.left >= 100.0 && rect.top >= 200.0 && rect.width > 0.0);
            let parent: IRawElementProviderSimple = fragment
                .Navigate(NavigateDirection_Parent)
                .unwrap()
                .cast()
                .unwrap();
            assert_eq!(
                boolean(&parent.GetPropertyValue(UIA_IsDialogPropertyId).unwrap()),
                Some(true)
            );
            let hit: IRawElementProviderSimple = root
                .ElementProviderFromPoint(rect.left + 1.0, rect.top + 1.0)
                .unwrap()
                .cast()
                .unwrap();
            assert_eq!(
                string(&hit.GetPropertyValue(UIA_NamePropertyId).unwrap()).as_deref(),
                Some("Stop")
            );
        }
    }

    #[test]
    fn fr24_the_stop_dialog_is_operated_through_its_patterns() {
        let state = DialogState::new(true);
        let (view, done) = open(&state);
        let kids = children(&view);
        // SAFETY: calls on the providers made above, on this thread.
        unsafe {
            // The check box toggles, the buttons invoke, the message does neither.
            assert!(kids[0].GetPatternProvider(UIA_InvokePatternId).is_err());
            assert!(kids[0].GetPatternProvider(UIA_TogglePatternId).is_err());
            assert!(kids[1].GetPatternProvider(UIA_InvokePatternId).is_err());
            let toggle: IToggleProvider = kids[1]
                .GetPatternProvider(UIA_TogglePatternId)
                .unwrap()
                .cast()
                .unwrap();
            assert_eq!(toggle.ToggleState().unwrap(), ToggleState_Off);
            toggle.Toggle().unwrap();
            assert!(kids[2].GetPatternProvider(UIA_TogglePatternId).is_err());
            let stop: IInvokeProvider = kids[2]
                .GetPatternProvider(UIA_InvokePatternId)
                .unwrap()
                .cast()
                .unwrap();
            stop.Invoke().unwrap();
            let cancel: IInvokeProvider = kids[3]
                .GetPatternProvider(UIA_InvokePatternId)
                .unwrap()
                .cast()
                .unwrap();
            cancel.Invoke().unwrap();
            let fragment: IRawElementProviderFragment = kids[3].cast().unwrap();
            fragment.SetFocus().unwrap();
        }
        assert_eq!(
            *done.lock().unwrap(),
            vec![
                Action::Activate(Control::Check),
                Action::Activate(Control::Primary),
                Action::Activate(Control::Secondary),
                Action::Focus(Control::Secondary),
            ]
        );
        // The dialog does what the client asked, as a click would: the box is ticked, then
        // Stop answers with it.
        let mut state = DialogState::new(true);
        use crate::compositor::fluent_dialog::{Answer, Outcome};
        assert_eq!(state.activate(Control::Check), Outcome::Changed);
        assert_eq!(
            state.activate(Control::Primary),
            Outcome::Done(Answer::Confirmed { checked: true })
        );
        assert_eq!(state.focus_on(Control::Secondary), Outcome::Changed);
        assert_eq!(state.focus_on(Control::Secondary), Outcome::Nothing);
        // A ticked box reads as ticked, and its state follows the dialog's.
        let ticked = DialogState {
            checked: true,
            ..DialogState::new(true)
        };
        let (view, _) = open(&ticked);
        let kids = children(&view);
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                i4(&kids[1]
                    .GetPropertyValue(UIA_ToggleToggleStatePropertyId)
                    .unwrap()),
                Some(ToggleState_On.0)
            );
        }
        // The posted form of each action comes back as it went.
        for action in [
            Action::Activate(Control::Check),
            Action::Activate(Control::Secondary),
            Action::Focus(Control::Primary),
        ] {
            assert_eq!(Action::from_code(action.code()), Some(action));
        }
        assert_eq!(Action::from_code(0x3f), None);
        // Once the dialog closed, its elements say they are gone.
        lock(&view).alive = false;
        // SAFETY: as above.
        unsafe {
            assert!(kids[2].GetPropertyValue(UIA_NamePropertyId).is_err());
        }
    }

    #[test]
    fn fr24_a_dialog_without_a_check_box_has_only_its_message_and_buttons() {
        let mut text = stop_music();
        text.check = None;
        let fixed = |line: &str, _: Font| 8 * line.chars().count() as i32;
        let laid = layout(&text, 96, &fixed);
        let kinds: Vec<Kind> = elements(&text, &laid, &DialogState::new(false), (0, 0))
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                Kind::Message,
                Kind::Control(Control::Primary),
                Kind::Control(Control::Secondary)
            ]
        );
    }
}

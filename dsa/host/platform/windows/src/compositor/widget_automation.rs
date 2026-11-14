//! The UI Automation tree of a widget window: a pane named after the widget, which takes the
//! keyboard, so Narrator can reach the widget's menu. Focusing the pane gives the widget
//! window the keyboard, which then opens the menu under the widget with Enter, Space, the
//! menu key or Shift+F10, as a right click opens it at the pointer. Nothing is drawn over
//! the widget for it: the widget's own buttons stay free in every corner.
//!
//! The provider reads a [`WidgetView`] the window writes whenever it moves or draws; a call
//! is posted to the window and done on its thread.

// UI Automation's constants keep their Windows names, which are matched on below.
#![allow(non_upper_case_globals)]

use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::{implement, IUnknown, IUnknownImpl};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::app_window::accessibility::{
    none, not_available, variant_bool, variant_empty, variant_i4, variant_str,
};
use crate::ui_automation::control_type;

/// Posted to a widget window to take the keyboard.
pub const WM_WIDGET_FOCUS: u32 = WM_APP + 0x51;

/// What a widget window's provider reads.
#[derive(Debug, Default)]
pub struct WidgetView {
    /// False once the window closed.
    pub alive: bool,
    pub hwnd: isize,
    /// The widget's name.
    pub name: String,
    /// Whether the window has the keyboard.
    pub focused: bool,
}

fn lock(view: &Mutex<WidgetView>) -> MutexGuard<'_, WidgetView> {
    view.lock().unwrap_or_else(|e| e.into_inner())
}

/// The widget window: a Pane that takes the keyboard.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct PaneProvider {
    view: Arc<Mutex<WidgetView>>,
}

/// The provider a widget window answers `WM_GETOBJECT` with.
pub fn provider(view: &Arc<Mutex<WidgetView>>) -> IRawElementProviderSimple {
    PaneProvider {
        view: Arc::clone(view),
    }
    .into()
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
pub fn release(
    hwnd: isize,
    view: &Arc<Mutex<WidgetView>>,
    provider: Option<IRawElementProviderSimple>,
) {
    lock(view).alive = false;
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

/// Says that the widget has the keyboard now.
pub fn raise_focus(view: &Arc<Mutex<WidgetView>>) {
    // SAFETY: a plain query.
    if !unsafe { UiaClientsAreListening() }.as_bool() {
        return;
    }
    let provider = provider(view);
    // SAFETY: an event of the window's own provider.
    let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId) };
}

/// Posts `message` to the widget window, which does it on its own thread.
fn post(view: &Mutex<WidgetView>, message: u32) -> windows::core::Result<()> {
    let view = lock(view);
    if !view.alive {
        return Err(not_available());
    }
    // SAFETY: posts to the widget's window, which may have gone; then it fails.
    unsafe { PostMessageW(view.hwnd as _, message, 0, 0) };
    Ok(())
}

impl IRawElementProviderSimple_Impl for PaneProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        none()
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        let view = lock(&self.view);
        Ok(match property {
            UIA_ControlTypePropertyId => variant_i4(control_type::PANE),
            UIA_NamePropertyId => variant_str(&view.name),
            UIA_IsKeyboardFocusablePropertyId => variant_bool(true),
            UIA_HasKeyboardFocusPropertyId => variant_bool(view.focused),
            _ => variant_empty(),
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        let hwnd = lock(&self.view).hwnd;
        // SAFETY: the widget's own window.
        unsafe { UiaHostProviderFromHwnd(HWND(hwnd as *mut _)) }
    }
}

impl IRawElementProviderFragment_Impl for PaneProvider_Impl {
    fn Navigate(
        &self,
        _direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        // The pane has no children: the widget's menu opens from the keyboard.
        none()
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        // The window's own provider gives the pane its runtime ID.
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        // The window's own provider gives the pane its bounds.
        Ok(UiaRect::default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> windows::core::Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        post(&self.view, WM_WIDGET_FOCUS)
    }

    fn FragmentRoot(&self) -> windows::core::Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for PaneProvider_Impl {
    fn ElementProviderFromPoint(
        &self,
        _x: f64,
        _y: f64,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        // Every point of the window is the pane itself.
        none()
    }

    fn GetFocus(&self) -> windows::core::Result<IRawElementProviderFragment> {
        // The pane itself has the keyboard, which its HasKeyboardFocus says.
        none()
    }
}

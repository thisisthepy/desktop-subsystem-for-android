//! The menu's UI Automation tree: each open level's window is a Menu whose children are its
//! entries, MenuItems and Separators, so Narrator and every other client read the menu as
//! they read a Windows menu.
//!
//! An item says its name, its access key, whether it is enabled and focused, and its place
//! among the items ("2 of 3"). An item that is chosen has the Invoke pattern, one that opens
//! a submenu the ExpandCollapse pattern, and one that can be checked the Toggle pattern.
//! The menu raises Menu Opened and Menu Closed for each level and a focus change for each
//! item the keyboard or the pointer lands on.
//!
//! The providers read a [`View`] the menu writes after every change; a pattern call is
//! posted to the menu's first window ([`Action`]) and done on its thread, as a key would be.

// UI Automation's constants keep their Windows names, which are matched on below.
#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::{implement, IUnknown, IUnknownImpl, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::window::WM_MENU_ACTION;
use crate::app_window::accessibility::{
    none, not_available, safe_array, variant_bool, variant_empty, variant_i4, variant_str,
};
use crate::ui_automation::control_type;

/// What a UI Automation client asked an item to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Invoke,
    Expand,
    Collapse,
    Focus,
}

impl Action {
    fn code(self) -> usize {
        match self {
            Action::Invoke => 1,
            Action::Expand => 2,
            Action::Collapse => 3,
            Action::Focus => 4,
        }
    }

    pub fn from_code(code: usize) -> Option<Action> {
        match code {
            1 => Some(Action::Invoke),
            2 => Some(Action::Expand),
            3 => Some(Action::Collapse),
            4 => Some(Action::Focus),
            _ => None,
        }
    }
}

/// One entry of an open level, as UI Automation reads it.
#[derive(Debug, Clone)]
pub struct ViewRow {
    pub entry: usize,
    pub name: String,
    pub separator: bool,
    pub enabled: bool,
    pub checked: Option<bool>,
    pub submenu: bool,
    /// Whether its submenu is open.
    pub expanded: bool,
    pub access_key: Option<char>,
    /// Left, top, width and height on the screen.
    pub bounds: (i32, i32, i32, i32),
}

/// One open level.
#[derive(Debug, Clone)]
pub struct ViewLevel {
    pub hwnd: isize,
    pub name: String,
    pub rows: Vec<ViewRow>,
    /// The entry the keyboard or the pointer is on.
    pub focus: Option<usize>,
}

/// What the menu's providers read.
#[derive(Debug, Default)]
pub struct View {
    /// False once the menu closed: every provider then answers that it is gone.
    pub alive: bool,
    /// The menu's first window, which actions are posted to.
    pub root: isize,
    pub levels: Vec<ViewLevel>,
    /// Tells this menu's items from an earlier menu's in their runtime IDs.
    pub epoch: u32,
}

impl View {
    /// The view of a menu that is opening.
    pub fn open() -> View {
        static EPOCH: AtomicU32 = AtomicU32::new(1);
        View {
            epoch: EPOCH.fetch_add(1, Ordering::Relaxed),
            ..View::default()
        }
    }
}

fn lock(view: &Mutex<View>) -> MutexGuard<'_, View> {
    view.lock().unwrap_or_else(|e| e.into_inner())
}

/// The place in level `level` of the first row `find` picks.
fn row_where(view: &Mutex<View>, level: usize, find: impl Fn(&ViewRow) -> bool) -> Option<usize> {
    let view = lock(view);
    let level = view.levels.get(level)?;
    level.rows.iter().position(find)
}

/// A level's window: a Menu.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct LevelProvider {
    view: Arc<Mutex<View>>,
    level: usize,
}

/// One entry of a level: a MenuItem or a Separator.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    IExpandCollapseProvider,
    IToggleProvider
)]
struct RowProvider {
    view: Arc<Mutex<View>>,
    level: usize,
    row: usize,
}

fn level_of(view: &Arc<Mutex<View>>, level: usize) -> IRawElementProviderFragmentRoot {
    LevelProvider {
        view: Arc::clone(view),
        level,
    }
    .into()
}

fn row_of(view: &Arc<Mutex<View>>, level: usize, row: usize) -> IRawElementProviderFragment {
    RowProvider {
        view: Arc::clone(view),
        level,
        row,
    }
    .into()
}

/// The provider a level's window answers `WM_GETOBJECT` with.
pub fn level_provider(view: &Arc<Mutex<View>>, level: usize) -> IRawElementProviderSimple {
    LevelProvider {
        view: Arc::clone(view),
        level,
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

/// Says that level `level` opened.
pub fn raise_opened(view: &Arc<Mutex<View>>, level: usize) {
    if listening() {
        let provider = level_provider(view, level);
        // SAFETY: an event of the menu's own provider.
        let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_MenuOpenedEventId) };
    }
}

/// Says that level `level` is closing.
pub fn raise_closed(view: &Arc<Mutex<View>>, level: usize) {
    if listening() {
        let provider = level_provider(view, level);
        // SAFETY: as above.
        let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_MenuClosedEventId) };
    }
}

/// Says that the keyboard or the pointer is on entry `entry` of level `level` now.
pub fn raise_focus(view: &Arc<Mutex<View>>, level: usize, entry: usize) {
    if !listening() {
        return;
    }
    let Some(row) = row_where(view, level, |row| row.entry == entry) else {
        return;
    };
    if let Ok(provider) = row_of(view, level, row).cast::<IRawElementProviderSimple>() {
        // SAFETY: as above.
        let _ = unsafe { UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId) };
    }
}

impl IRawElementProviderSimple_Impl for LevelProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        none()
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        let view = lock(&self.view);
        let Some(level) = view.levels.get(self.level).filter(|_| view.alive) else {
            return Ok(variant_empty());
        };
        Ok(match property {
            UIA_ControlTypePropertyId => variant_i4(control_type::MENU),
            UIA_NamePropertyId => variant_str(&level.name),
            UIA_IsKeyboardFocusablePropertyId => variant_bool(false),
            UIA_IsControlElementPropertyId => variant_bool(true),
            UIA_IsContentElementPropertyId => variant_bool(true),
            _ => variant_empty(),
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        let hwnd = lock(&self.view).levels.get(self.level).map(|l| l.hwnd);
        match hwnd {
            // SAFETY: the level's own window.
            Some(hwnd) => unsafe { UiaHostProviderFromHwnd(HWND(hwnd as *mut _)) },
            None => none(),
        }
    }
}

impl IRawElementProviderFragment_Impl for LevelProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let view = lock(&self.view);
        let count = view.levels.get(self.level).map_or(0, |l| l.rows.len());
        drop(view);
        let row = match direction {
            NavigateDirection_FirstChild if count > 0 => 0,
            NavigateDirection_LastChild if count > 0 => count - 1,
            _ => return none(),
        };
        Ok(row_of(&self.view, self.level, row))
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        // The window's own provider gives the menu its runtime ID.
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        // The window's own provider gives the menu its bounds.
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

impl IRawElementProviderFragmentRoot_Impl for LevelProvider_Impl {
    fn ElementProviderFromPoint(
        &self,
        x: f64,
        y: f64,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let row = row_where(&self.view, self.level, |row| {
            let (left, top, width, height) = row.bounds;
            let (left, top) = (f64::from(left), f64::from(top));
            x >= left && y >= top && x < left + f64::from(width) && y < top + f64::from(height)
        });
        match row {
            Some(row) => Ok(row_of(&self.view, self.level, row)),
            None => none(),
        }
    }

    fn GetFocus(&self) -> windows::core::Result<IRawElementProviderFragment> {
        let view = lock(&self.view);
        let focus = view.levels.get(self.level).and_then(|l| l.focus);
        drop(view);
        let row = focus.and_then(|focus| row_where(&self.view, self.level, |r| r.entry == focus));
        match row {
            Some(row) => Ok(row_of(&self.view, self.level, row)),
            None => none(),
        }
    }
}

impl RowProvider {
    /// Reads the row and its level while the menu is open.
    fn with_row<T>(
        &self,
        read: impl FnOnce(&ViewRow, &ViewLevel) -> T,
    ) -> windows::core::Result<T> {
        let view = lock(&self.view);
        if !view.alive {
            return Err(not_available());
        }
        let level = view.levels.get(self.level).ok_or_else(not_available)?;
        let row = level.rows.get(self.row).ok_or_else(not_available)?;
        Ok(read(row, level))
    }

    /// Posts `action` on this row to the menu, which does it on its own thread.
    fn post(&self, action: Action) -> windows::core::Result<()> {
        let entry = self.with_row(|row, _| row.entry)?;
        let root = lock(&self.view).root;
        let at = (self.level << 16) | (entry & 0xffff);
        // SAFETY: posts to the menu's window, which may have gone; then it fails.
        unsafe { PostMessageW(root as _, WM_MENU_ACTION, action.code(), at as isize) };
        Ok(())
    }
}

/// UI Automation's toggle state of an item that can be checked.
fn toggle_state(checked: Option<bool>) -> ToggleState {
    if checked == Some(true) {
        ToggleState_On
    } else {
        ToggleState_Off
    }
}

/// UI Automation's expand and collapse state of a row.
fn expand_state(row: &ViewRow) -> ExpandCollapseState {
    match (row.submenu, row.expanded) {
        (false, _) => ExpandCollapseState_LeafNode,
        (true, true) => ExpandCollapseState_Expanded,
        (true, false) => ExpandCollapseState_Collapsed,
    }
}

impl IRawElementProviderSimple_Impl for RowProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        let supported = self.with_row(|row, _| {
            let item = !row.separator && row.enabled;
            match pattern {
                UIA_InvokePatternId => item && !row.submenu,
                UIA_ExpandCollapsePatternId => item && row.submenu,
                UIA_TogglePatternId => item && row.checked.is_some(),
                _ => false,
            }
        })?;
        if supported {
            Ok(self.to_interface::<IRawElementProviderSimple>().cast()?)
        } else {
            none()
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        let index = self.row;
        self.with_row(|row, level| {
            let items = |rows: &[ViewRow]| rows.iter().filter(|r| !r.separator).count();
            let position = items(&level.rows[..=index.min(level.rows.len() - 1)]);
            let focusable = !row.separator && row.enabled;
            let kind = if row.separator {
                control_type::SEPARATOR
            } else {
                control_type::MENU_ITEM
            };
            match property {
                UIA_ControlTypePropertyId => variant_i4(kind),
                UIA_NamePropertyId => variant_str(&row.name),
                UIA_AccessKeyPropertyId => match row.access_key {
                    Some(key) => variant_str(&key.to_string()),
                    None => variant_empty(),
                },
                UIA_IsEnabledPropertyId => variant_bool(row.enabled),
                UIA_HasKeyboardFocusPropertyId => variant_bool(level.focus == Some(row.entry)),
                UIA_IsKeyboardFocusablePropertyId => variant_bool(focusable),
                UIA_IsControlElementPropertyId => variant_bool(true),
                UIA_IsContentElementPropertyId => variant_bool(!row.separator),
                UIA_IsOffscreenPropertyId => variant_bool(false),
                UIA_PositionInSetPropertyId if !row.separator => variant_i4(position as i32),
                UIA_SizeOfSetPropertyId if !row.separator => variant_i4(items(&level.rows) as i32),
                UIA_ToggleToggleStatePropertyId if row.checked.is_some() => {
                    variant_i4(toggle_state(row.checked).0)
                }
                UIA_ExpandCollapseExpandCollapseStatePropertyId => variant_i4(expand_state(row).0),
                _ => variant_empty(),
            }
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for RowProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let count = self.with_row(|_, level| level.rows.len())?;
        let row = match direction {
            NavigateDirection_Parent => return Ok(level_of(&self.view, self.level).cast()?),
            NavigateDirection_NextSibling if self.row + 1 < count => self.row + 1,
            NavigateDirection_PreviousSibling if self.row > 0 => self.row - 1,
            _ => return none(),
        };
        Ok(row_of(&self.view, self.level, row))
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        let epoch = lock(&self.view).epoch;
        let id = (self.level * 1000 + self.row) as i32;
        safe_array(&[UiaAppendRuntimeId as i32, epoch as i32, id])
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        self.with_row(|row, _| {
            let (left, top, width, height) = row.bounds;
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
        self.post(Action::Focus)
    }

    fn FragmentRoot(&self) -> windows::core::Result<IRawElementProviderFragmentRoot> {
        Ok(level_of(&self.view, self.level))
    }
}

impl IInvokeProvider_Impl for RowProvider_Impl {
    fn Invoke(&self) -> windows::core::Result<()> {
        self.post(Action::Invoke)
    }
}

impl IExpandCollapseProvider_Impl for RowProvider_Impl {
    fn Expand(&self) -> windows::core::Result<()> {
        self.post(Action::Expand)
    }

    fn Collapse(&self) -> windows::core::Result<()> {
        self.post(Action::Collapse)
    }

    fn ExpandCollapseState(&self) -> windows::core::Result<ExpandCollapseState> {
        self.with_row(|row, _| expand_state(row))
    }
}

impl IToggleProvider_Impl for RowProvider_Impl {
    fn Toggle(&self) -> windows::core::Result<()> {
        self.post(Action::Invoke)
    }

    fn ToggleState(&self) -> windows::core::Result<ToggleState> {
        self.with_row(|row, _| toggle_state(row.checked))
    }
}

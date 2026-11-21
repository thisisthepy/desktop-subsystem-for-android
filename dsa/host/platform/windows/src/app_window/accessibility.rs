//! Each task window's UI Automation tree: the Android task's accessibility nodes, as Narrator,
//! Accessibility Insights and every other UI Automation client read them.
//!
//! A client that reads a window sends it `WM_GETOBJECT`. The window answers with a fragment
//! root ([`RootProvider`]) whose children are the task's nodes ([`NodeProvider`]). How the
//! task's tree is then had depends on the client ([`ClientDemand`]):
//!
//! - While a screen reader is on (`SPI_GETSCREENREADER`) or a client subscribed to focus
//!   changes, the request starts a thread that watches the task's tree through the service's
//!   local API ([`HostApiClient::watch_accessibility`]): the service asks the agent in Android
//!   to read the task's nodes, and their tree and its changes come back here. The watch tells
//!   Android whether that client listens for events, so focus comes at once. It ends once no
//!   such client is left, or once no client has asked for [`PRESENCE_GRACE`] and none
//!   listens.
//! - Any other client (a feature of the desktop or another app looking at every window) gets
//!   the tree read once when it reads the window, at most every [`SNAPSHOT_INTERVAL`], and
//!   once for each action it asks for; the watch then ends by itself. Reading every node is a
//!   call answered on the app's UI thread, so a continuous watch for such clients made apps
//!   stutter on a desktop with no screen reader at all.
//!
//! Until a client asks, nothing runs and Android reads nothing. The window's log says why
//! each watch started and ended.
//!
//! Editable nodes have the Text pattern ([`patterns`]): ranges by character, word and line
//! over the node's text, its selection, and Select, which moves the cursor in Android. Lists
//! and grids have Grid and Table, their items GridItem, TableItem and ScrollItem, sliders
//! RangeValue, expandable nodes ExpandCollapse. Each Android window's root says whether it is
//! a dialog or a pop-up, and an app's announcements and toasts are raised as notifications.
//! Before the first tree, the root's child is an item that says the app is loading.
//!
//! Each node gets the control type of its role, its name, its bounds on the screen (the
//! window's client area plus where the window shows the node: its place in the task at the
//! scale the window shows the task at, 1:1 unless the task keeps its density on a monitor of
//! another scale),
//! and the patterns it supports: Invoke, Value, Toggle,
//! SelectionItem and Scroll ([`crate::ui_automation`]). A pattern call becomes the node's
//! accessibility action in Android, so Invoke presses a list row without moving the cursor.
//! As changes arrive the window raises structure, property and focus change events.
//!
//! The title bar the window draws is in the tree too, before the task's nodes because it sits
//! at the window's top: one button for each of its parts ([`CaptionProvider`]), left to right.
//! The icon's cell is "System menu" ("시스템 메뉴"), whose Invoke opens the window menu as
//! Alt+Space does, or, while the task can go back and the cell shows the back arrow, "Back"
//! ("뒤로"), whose Invoke sends the task Back as a click on the arrow does. Then come
//! "Minimize" ("최소화"), "Maximize" ("최대화"), named "Restore" ("이전 크기로") while the
//! window is maximized or fills its monitor, and "Close" ("닫기"); each Invoke posts the
//! command a click on the button posts. Names follow the display language
//! ([`crate::ui_language`]), as Windows names its own caption buttons in it.
//! A button's bounds are the cell the window draws it in, on the screen. While a window that
//! fills its monitor keeps its title bar hidden, its buttons stay in the tree and say they are
//! off the screen (`IsOffscreen`). The window raises structure change events as buttons come
//! and go (the back arrow takes the icon's place), and property change events as they move,
//! are renamed (Maximize and Restore), are hidden or shown, or are enabled.
//!
//! Windows' own non-client proxy would describe a standard caption beside these buttons. The
//! title bar's information the window answers (`WM_GETTITLEBARINFOEX`, which that proxy reads)
//! keeps the buttons' real places and marks them invisible, so clients find these buttons
//! alone ([`super::caption`]).
//!
//! Until the app's first frame the window shows only its splash. Meanwhile the window says
//! it is opening the app: its item status is "Opening the app" ("앱을 여는 중") and it is
//! busy (`busy=true`). The first tick after the first frame empties the status and raises its
//! property change event, so a screen reader learns the app is there.
//!
//! A tree that breaks the rules is dropped and logged, and the watch starts over; nothing a
//! tree holds can end the window host.
//!
//! [`PRESENCE_GRACE`]: dsa_host_core::accessibility::PRESENCE_GRACE
//! [`SNAPSHOT_INTERVAL`]: dsa_host_core::accessibility::SNAPSHOT_INTERVAL

// UI Automation's constants keep their Windows names, which are matched on below.
#![allow(non_upper_case_globals)]

#[path = "accessibility_patterns.rs"]
mod patterns;

use std::collections::HashSet;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_host_core::accessibility::{
    focus_trace_line, from_screen, to_screen, wall_millis, AccessibilityTarget, ActionRequest,
    Announcement, Changes, ClientDemand, Demand, DemandReason, GuestAccessibility, Node,
    NodeAction, PresenceChange, Tree, TreeError,
};
#[cfg(test)]
use dsa_host_core::compositor::caption::{CaptionButton, CaptionLayout};
pub use dsa_host_core::compositor::caption::{CaptionCells, CaptionKind, ClientRect};
use dsa_host_core::compositor::window_sizing::{frame_placement, FramePlacement};
use dsa_host_core::compositor::window_tasks::WindowTasks;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::input::{InputEvent, WindowGeometry};
use dsa_host_core::ui_language::UiLanguage;
use dsa_host_core::ui_text::caption::{caption_name, loading_name, loading_status};
use dsa_transport::Connector;
use windows::core::{
    implement, Error, IUnknown, IUnknownImpl, Interface, BOOL, BSTR, HRESULT, PCWSTR,
};
use windows::Win32::Foundation::{HWND, LPARAM, VARIANT_BOOL, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_EMPTY, VT_I4};
use windows::Win32::UI::Accessibility::*;

use super::caption::back_keys;
use crate::compositor::log::{note, CompositorLogger};
use crate::ui_automation::{
    control_type, control_type_of, heading_level, is_content_element, is_control_element,
    patterns_of, row_at_percent, scroll_action, Pattern,
};

/// Whether the window `hwnd` has not yet presented its app's first frame, as the presenter
/// marks it on the window.
fn shared_loading(hwnd: isize) -> bool {
    use crate::compositor::window_marks::{read_time, FIRST_FRAME_PROPERTY};
    read_time(hwnd, FIRST_FRAME_PROPERTY).is_none()
}

/// What UI Automation needs of a title bar button beyond what the buttons are.
trait CaptionKindUia {
    fn runtime_id(self) -> [i32; 2];
    fn automation_id(self) -> &'static str;
    fn accelerator(self) -> Option<&'static str>;
}

impl CaptionKindUia for CaptionKind {
    /// The button's runtime ID. A node's has three parts, so one of two never equals it, and
    /// the second part tells the buttons apart.
    fn runtime_id(self) -> [i32; 2] {
        let id = match self {
            // "BACK", "MENU", "MINI", "MAXI" and "CLOS" in ASCII.
            CaptionKind::Back => 0x4241_434b,
            CaptionKind::SystemMenu => 0x4d45_4e55,
            CaptionKind::Minimize => 0x4d49_4e49,
            CaptionKind::Maximize => 0x4d41_5849,
            CaptionKind::Close => 0x434c_4f53,
        };
        [UiaAppendRuntimeId as i32, id]
    }

    /// The button's automation ID, the same in every language and window state.
    fn automation_id(self) -> &'static str {
        match self {
            CaptionKind::Back => "Back",
            CaptionKind::SystemMenu => "SystemMenu",
            CaptionKind::Minimize => "Minimize",
            CaptionKind::Maximize => "Maximize",
            CaptionKind::Close => "Close",
        }
    }

    /// The keys that do what the button does, as Windows names them on its own menu.
    fn accelerator(self) -> Option<&'static str> {
        match self {
            CaptionKind::SystemMenu => Some("Alt+Space"),
            CaptionKind::Close => Some("Alt+F4"),
            _ => None,
        }
    }
}

/// What pressing `kind` posts to the window, as the `WM_SYSCOMMAND` command and its `lparam`:
/// the window menu as Alt+Space opens it, and the commands a click on the caption buttons
/// posts. `None` for the back arrow, which sends the task Back instead, and for a button that
/// does nothing now.
fn caption_command(kind: CaptionKind, cells: &CaptionCells) -> Option<(u32, isize)> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SC_CLOSE, SC_KEYMENU, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE,
    };
    if !cells.enabled(kind) {
        return None;
    }
    match kind {
        CaptionKind::Back => None,
        // Alt+Space: the window menu, under the icon's cell, on its first item.
        CaptionKind::SystemMenu => Some((SC_KEYMENU, isize::from(b' '))),
        CaptionKind::Minimize => Some((SC_MINIMIZE, 0)),
        // A window that fills its monitor leaves it on Restore, as its middle button does.
        CaptionKind::Maximize if cells.maximized => Some((SC_RESTORE, 0)),
        CaptionKind::Maximize => Some((SC_MAXIMIZE, 0)),
        CaptionKind::Close => Some((SC_CLOSE, 0)),
    }
}

/// The loading item's runtime ID, as unlike a node's as the back button's.
const LOADING_RUNTIME_ID: [i32; 2] = [UiaAppendRuntimeId as i32, 0x4c4f_4144];

/// One child of the window's fragment root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootChild {
    /// A button of the title bar.
    Caption(CaptionKind),
    /// What the window shows before the app's first tree has come.
    Loading,
    /// A top-level node of the task.
    Node(u32),
}

/// The root's children in order: the title bar's buttons first, left to right (they are at
/// the window's top), then the task's top-level nodes.
fn root_children(captions: &[CaptionKind], roots: &[u32]) -> Vec<RootChild> {
    root_children_of(captions, roots, false)
}

/// The root's children as [`root_children`] has them, with the loading item after the title
/// bar's buttons while the app's first tree has not come (`loading`).
fn root_children_of(captions: &[CaptionKind], roots: &[u32], loading: bool) -> Vec<RootChild> {
    captions
        .iter()
        .copied()
        .map(RootChild::Caption)
        .chain(loading.then_some(RootChild::Loading))
        .chain(roots.iter().copied().map(RootChild::Node))
        .collect()
}

/// The child after (`forward`) or before `of` among the root's `children`.
fn root_sibling(children: &[RootChild], of: RootChild, forward: bool) -> Option<RootChild> {
    let at = children.iter().position(|child| *child == of)?;
    if forward {
        children.get(at + 1).copied()
    } else {
        at.checked_sub(1)
            .and_then(|before| children.get(before).copied())
    }
}

/// What happened to a title bar button, for the events the window raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptionChange {
    Added,
    Removed,
    Moved,
    /// The maximize button's name changed: it was named for a maximized window (`from`), and
    /// is named for one now (`to`).
    Renamed {
        from: bool,
        to: bool,
    },
    /// The title bar was hidden, or shown (`IsOffscreen` is now the value held).
    Offscreen(bool),
    /// The button can be pressed now, or no longer.
    Enabled(bool),
}

/// What changed for each button from the title bar `before` to `after`, in the order the
/// buttons stand.
fn caption_changes(
    before: &CaptionCells,
    after: &CaptionCells,
) -> Vec<(CaptionKind, CaptionChange)> {
    let mut changes = Vec::new();
    for kind in CaptionKind::ALL {
        match (before.cell(kind), after.cell(kind)) {
            (None, Some(_)) => changes.push((kind, CaptionChange::Added)),
            (Some(_), None) => changes.push((kind, CaptionChange::Removed)),
            (Some(was), Some(now)) => {
                if was != now {
                    changes.push((kind, CaptionChange::Moved));
                }
                if kind == CaptionKind::Maximize && before.maximized != after.maximized {
                    changes.push((
                        kind,
                        CaptionChange::Renamed {
                            from: before.maximized,
                            to: after.maximized,
                        },
                    ));
                }
                if before.offscreen != after.offscreen {
                    changes.push((kind, CaptionChange::Offscreen(after.offscreen)));
                }
                if before.enabled(kind) != after.enabled(kind) {
                    changes.push((kind, CaptionChange::Enabled(after.enabled(kind))));
                }
            }
            (None, None) => {}
        }
    }
    changes
}

/// Dials the service's local API, once for each watch.
pub type ApiConnect = Arc<dyn Fn() -> Box<dyn Connector> + Send + Sync>;

/// The build the window host names itself by when it watches a tree.
const BUILD: &str = "dsa-app-window accessibility";

/// How long the watch waits for the service, and between two tries after it failed.
const CONNECT: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch thread waits for news before it looks at its actions again.
const POLL: Duration = Duration::from_millis(20);

/// How long a watch for one read keeps going after it sent an action, so the screen the
/// action opens reaches the client too.
const ACTION_LINGER: Duration = Duration::from_secs(3);

/// How long a watch for one read waits for Android's tree before it gives up.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most actions that wait for a watch at once.
const MAX_QUEUED_ACTIONS: usize = 32;

/// The most property change events one batch of changes raises; past it the window says its
/// whole tree changed instead, which costs a client one read.
const MAX_PROPERTY_EVENTS: usize = 64;

/// The window message that tells a task window to raise the events of the changes that came.
pub const WM_APP_ACCESSIBILITY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;

impl State {
    /// What node bounds and points are converted with, for a client area of `client`. A
    /// window that holds the size the desktop gave it shows a frame of another size in its
    /// middle ([`frame_placement`]), at the scale that fits it there.
    fn geometry(&self, client: (u32, u32)) -> WindowGeometry {
        match self.placement(client) {
            Some(placement) => WindowGeometry {
                client: placement.size,
                task: self.task_size,
                scale: placement.scale,
            },
            None => WindowGeometry {
                client,
                task: self.task_size,
                scale: self.scale,
            },
        }
    }

    /// Where the frame's top left is on the screen, for a client area of `client` whose top
    /// left is at `origin`.
    fn frame_origin(&self, origin: (i32, i32), client: (u32, u32)) -> (i32, i32) {
        match self.placement(client) {
            Some(placement) => (
                origin.0 + placement.origin.0 as i32,
                origin.1 + placement.origin.1 as i32,
            ),
            None => origin,
        }
    }

    fn placement(&self, client: (u32, u32)) -> Option<FramePlacement> {
        self.held
            .then(|| frame_placement(self.task_size, client, self.scale, true))
    }
}

/// What the window's providers share with its watch thread.
struct State {
    tree: Tree,
    /// The task's size in Android, which node bounds are measured in.
    task_size: (u32, u32),
    /// How large the window shows each task pixel (window pixels per task pixels).
    scale: (u32, u32),
    /// Whether the window holds the size the desktop gave it (maximised, snapped, full
    /// screen), and so shows a frame of another size in its middle.
    held: bool,
    /// Actions waiting for the watch to send them to Android.
    queued: Vec<ActionRequest>,
    /// Whether a watch thread runs, continuous or for one read.
    watching: bool,
    /// A client read the tree, or asked for an action, while no watch ran: the window reads
    /// the tree once for it ([`AccessibilityWindow::serve`]).
    wants_read: bool,
    wants_action: bool,
    /// What the app asked a screen reader to say, waiting for the UI thread to raise it.
    announcements: Vec<Announcement>,
    next_request: u32,
    /// Changes waiting for the UI thread to raise their events, each with the tree's names
    /// from before it.
    pending: Vec<(Changes, Vec<(u32, String)>)>,
    /// The title bar's buttons as the window draws them now.
    caption: CaptionCells,
    /// Changes to the title bar's buttons waiting for the UI thread to raise their events.
    caption_changes: Vec<(CaptionKind, CaptionChange)>,
    /// Where the back button's Back goes: the window host's input for the guest.
    input: Sender<InputEvent>,
}

/// The state a window's providers read, behind one lock.
pub struct Shared {
    hwnd: isize,
    task_id: i32,
    state: Mutex<State>,
    /// How many clients subscribed to focus changes on the window ([`RootProvider`]'s
    /// AdviseEventAdded): a client that follows the focus is an assistive technology.
    focus_listeners: AtomicI32,
    /// Whether the watch should tell Android a screen reader listens, set by the UI thread.
    fast: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A client read the tree. With no watch running, the window is asked to read it once.
    fn read_wanted(&self) {
        let mut state = self.lock();
        if state.watching || state.wants_read {
            return;
        }
        state.wants_read = true;
        drop(state);
        post_events(self.hwnd);
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    /// Whether the window still waits for its app's first frame and shows the splash. The
    /// presenter marks the window when it presents that frame, from whichever thread presents.
    fn loading(&self) -> bool {
        shared_loading(self.hwnd)
    }

    /// The client area's top left on the screen and its size.
    fn client(&self) -> ((i32, i32), (u32, u32)) {
        use windows_sys::Win32::Foundation::{POINT, RECT};
        use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
        let hwnd = self.hwnd as windows_sys::Win32::Foundation::HWND;
        let mut origin = POINT { x: 0, y: 0 };
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: plain reads of a window's geometry; a stale handle only fails.
        unsafe {
            ClientToScreen(hwnd, &mut origin);
            GetClientRect(hwnd, &mut rect);
        }
        (
            (origin.x, origin.y),
            (
                (rect.right - rect.left).max(0) as u32,
                (rect.bottom - rect.top).max(0) as u32,
            ),
        )
    }

    /// Asks Android to do `request`'s action on its node. The watch sends it; with no watch
    /// running the window starts one for it. An error when the node is gone, or the window
    /// watches nothing at all.
    fn perform(&self, mut request: ActionRequest) -> windows::core::Result<()> {
        let mut state = self.lock();
        if state.tree.epoch() != Some(request.epoch) || state.tree.node(request.node_id).is_none() {
            return Err(not_available());
        }
        // A client that piles actions up on a window nobody watches is not let to grow it.
        if state.queued.len() >= MAX_QUEUED_ACTIONS {
            return Err(not_available());
        }
        state.next_request = state.next_request.wrapping_add(1);
        request.request_id = state.next_request;
        state.queued.push(request);
        let start = !state.watching && !state.wants_action;
        if start {
            state.wants_action = true;
        }
        drop(state);
        if start {
            post_events(self.hwnd);
        }
        Ok(())
    }

    /// Sends the task Back, as a click on the back arrow does. An error while the arrow is
    /// not shown.
    fn go_back(&self) -> windows::core::Result<()> {
        let state = self.lock();
        if state.caption.cell(CaptionKind::Back).is_none() {
            return Err(not_available());
        }
        // The same clock as a message's time, which a click's Back carries; UI Automation may
        // call from a thread other than the window's, which has no message time of its own.
        // SAFETY: a plain query.
        let time = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount() };
        for key in back_keys(self.task_id, time) {
            if state.input.send(InputEvent::Key(key)).is_err() {
                return Err(not_available());
            }
        }
        Ok(())
    }

    /// Presses the title bar's `kind` button, as a click on it does. An error while the title
    /// bar has no such button, or the button does nothing now.
    fn press(&self, kind: CaptionKind) -> windows::core::Result<()> {
        let cells = self.lock().caption;
        if cells.cell(kind).is_none() {
            return Err(not_available());
        }
        if kind == CaptionKind::Back {
            return self.go_back();
        }
        let Some((command, lparam)) = caption_command(kind, &cells) else {
            return Err(not_enabled());
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_SYSCOMMAND};
        // Posted, as a click on the button posts it: UI Automation calls from a thread of its
        // own, and the window carries the command out on its thread once the call returned
        // (the window menu runs a loop of its own until it closes).
        // SAFETY: posting to the window; a stale handle only fails.
        let posted = unsafe {
            PostMessageW(
                self.hwnd as windows_sys::Win32::Foundation::HWND,
                WM_SYSCOMMAND,
                command as usize,
                lparam,
            )
        };
        if posted == 0 {
            return Err(not_available());
        }
        Ok(())
    }
}

/// The element cannot be used now.
fn not_enabled() -> Error {
    Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTENABLED as i32))
}

pub(crate) fn not_available() -> Error {
    Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

fn invalid_operation() -> Error {
    Error::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32))
}

/// No element: S_OK with a null pointer, as UI Automation expects for "nothing there".
pub(crate) fn none<T>() -> windows::core::Result<T> {
    Err(Error::empty())
}

pub(crate) fn variant_i4(value: i32) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: writing the tag and the matching member of a fresh VARIANT.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = value;
    }
    variant
}

pub(crate) fn variant_bool(value: bool) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if value { -1 } else { 0 });
    }
    variant
}

pub(crate) fn variant_str(value: &str) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: as above; the caller owns the string with the VARIANT.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BSTR;
        inner.Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(value));
    }
    variant
}

pub(crate) fn variant_r8(value: f64) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = windows::Win32::System::Variant::VT_R8;
        inner.Anonymous.dblVal = value;
    }
    variant
}

pub(crate) fn variant_empty() -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        (*variant.Anonymous.Anonymous).vt = VT_EMPTY;
    }
    variant
}

/// A node's runtime ID: unique in the window while its epoch lasts.
fn runtime_id(epoch: u32, id: u32) -> [i32; 3] {
    [UiaAppendRuntimeId as i32, epoch as i32, id as i32]
}

pub(crate) fn safe_array(values: &[i32]) -> windows::core::Result<*mut SAFEARRAY> {
    // SAFETY: a new vector of 4-byte integers, each element written once.
    unsafe {
        let array = SafeArrayCreateVector(VT_I4, 0, values.len() as u32);
        if array.is_null() {
            return Err(Error::from_hresult(HRESULT(0x8007000Eu32 as i32)));
        }
        for (index, value) in values.iter().enumerate() {
            let index = index as i32;
            SafeArrayPutElement(
                array,
                &index,
                value as *const i32 as *const core::ffi::c_void,
            )?;
        }
        Ok(array)
    }
}

/// The window's fragment root: the task window itself, whose children are the top-level
/// nodes of the task. Its name and the rest come from the window's own provider.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot,
    IRawElementProviderAdviseEvents
)]
struct RootProvider {
    shared: Arc<Shared>,
}

/// One node of the task. The patterns past Scroll are in [`patterns`].
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderSimple2,
    IRawElementProviderFragment,
    IInvokeProvider,
    IValueProvider,
    IToggleProvider,
    ISelectionItemProvider,
    IScrollProvider,
    ITextProvider,
    IRangeValueProvider,
    IExpandCollapseProvider,
    IGridProvider,
    IGridItemProvider,
    ITableProvider,
    ITableItemProvider,
    IScrollItemProvider,
    ILegacyIAccessibleProvider,
    IItemContainerProvider
)]
struct NodeProvider {
    shared: Arc<Shared>,
    epoch: u32,
    id: u32,
}

/// One button of the title bar the window draws, while the title bar has it.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider
)]
struct CaptionProvider {
    shared: Arc<Shared>,
    kind: CaptionKind,
}

fn caption_of(shared: &Arc<Shared>, kind: CaptionKind) -> IRawElementProviderFragment {
    CaptionProvider {
        shared: Arc::clone(shared),
        kind,
    }
    .into()
}

/// The provider of one of the root's children; a node is of tree `epoch`.
fn child_of(
    shared: &Arc<Shared>,
    epoch: Option<u32>,
    child: Option<RootChild>,
) -> windows::core::Result<IRawElementProviderFragment> {
    match (child, epoch) {
        (Some(RootChild::Caption(kind)), _) => Ok(caption_of(shared, kind)),
        (Some(RootChild::Loading), _) => Ok(patterns::loading_of(shared)),
        (Some(RootChild::Node(id)), Some(epoch)) => Ok(node_of(shared, epoch, id)),
        _ => none(),
    }
}

fn root_of(shared: &Arc<Shared>) -> IRawElementProviderFragmentRoot {
    RootProvider {
        shared: Arc::clone(shared),
    }
    .into()
}

fn node_of(shared: &Arc<Shared>, epoch: u32, id: u32) -> IRawElementProviderFragment {
    NodeProvider {
        shared: Arc::clone(shared),
        epoch,
        id,
    }
    .into()
}

impl IRawElementProviderSimple_Impl for RootProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        none()
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        Ok(match property {
            UIA_FrameworkIdPropertyId => variant_str("Android"),
            UIA_ProviderDescriptionPropertyId => variant_str("Desktop Subsystem for Android"),
            UIA_ItemStatusPropertyId => {
                match loading_status(display_language(), self.shared.loading()) {
                    Some(status) => variant_str(status),
                    None => variant_empty(),
                }
            }
            UIA_AriaPropertiesPropertyId if self.shared.loading() => variant_str("busy=true"),
            _ => variant_empty(),
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        // SAFETY: the window this provider belongs to.
        unsafe { UiaHostProviderFromHwnd(self.shared.hwnd()) }
    }
}

impl IRawElementProviderFragment_Impl for RootProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        // A walk of the tree starts here: a client that is no screen reader gets it read.
        self.shared.read_wanted();
        let (epoch, child) = {
            let state = self.shared.lock();
            let epoch = state.tree.epoch();
            let roots: &[u32] = if epoch.is_some() {
                state.tree.roots()
            } else {
                &[]
            };
            let children = root_children_of(&state.caption.kinds(), roots, epoch.is_none());
            let child = match direction {
                NavigateDirection_FirstChild => children.first().copied(),
                NavigateDirection_LastChild => children.last().copied(),
                _ => None,
            };
            (epoch, child)
        };
        child_of(&self.shared, epoch, child)
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        // The window's own provider gives the root its runtime ID.
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        // The window's own provider gives the root its bounds.
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

impl IRawElementProviderFragmentRoot_Impl for RootProvider_Impl {
    fn ElementProviderFromPoint(
        &self,
        x: f64,
        y: f64,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        self.shared.read_wanted();
        let (origin, client) = self.shared.client();
        let state = self.shared.lock();
        // The title bar is drawn over the app's top, so a button is what a point in its cell
        // hits, while the title bar shows.
        let button = state.caption.at(x - origin.0 as f64, y - origin.1 as f64);
        if let Some(kind) = button {
            drop(state);
            return Ok(caption_of(&self.shared, kind));
        }
        let Some(epoch) = state.tree.epoch() else {
            return none();
        };
        let (tx, ty) = from_screen(
            (x, y),
            state.geometry(client),
            state.frame_origin(origin, client),
        );
        match state.tree.hit(tx, ty) {
            Some(id) => Ok(node_of(&self.shared, epoch, id)),
            None => none(),
        }
    }

    fn GetFocus(&self) -> windows::core::Result<IRawElementProviderFragment> {
        self.shared.read_wanted();
        let state = self.shared.lock();
        match (state.tree.epoch(), state.tree.client_focus()) {
            (Some(epoch), Some(id)) => Ok(node_of(&self.shared, epoch, id)),
            _ => none(),
        }
    }
}

impl IRawElementProviderAdviseEvents_Impl for RootProvider_Impl {
    fn AdviseEventAdded(
        &self,
        event: UIA_EVENT_ID,
        _: *const SAFEARRAY,
    ) -> windows::core::Result<()> {
        if event == UIA_AutomationFocusChangedEventId {
            self.shared.focus_listeners.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn AdviseEventRemoved(
        &self,
        event: UIA_EVENT_ID,
        _: *const SAFEARRAY,
    ) -> windows::core::Result<()> {
        if event == UIA_AutomationFocusChangedEventId {
            // A removal UI Automation reports twice never leaves the count below none.
            if self.shared.focus_listeners.fetch_sub(1, Ordering::Relaxed) <= 0 {
                self.shared.focus_listeners.store(0, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

impl NodeProvider {
    /// The node, while it is in the tree of this provider's epoch.
    fn with_node<T>(&self, read: impl FnOnce(&Tree, &Node) -> T) -> windows::core::Result<T> {
        let state = self.shared.lock();
        if state.tree.epoch() != Some(self.epoch) {
            return Err(not_available());
        }
        let node = state.tree.node(self.id).ok_or_else(not_available)?;
        Ok(read(&state.tree, node))
    }

    fn act(&self, action: NodeAction, text: String) -> windows::core::Result<()> {
        self.act_with(ActionRequest {
            action,
            text,
            ..ActionRequest::default()
        })
    }

    /// Asks Android for `request`'s action on this node, which must support it.
    fn act_with(&self, mut request: ActionRequest) -> windows::core::Result<()> {
        let supported = self.with_node(|_, node| node.supports(request.action))?;
        if !supported {
            return Err(invalid_operation());
        }
        request.epoch = self.epoch;
        request.node_id = self.id;
        self.shared.perform(request)
    }
}

impl IRawElementProviderSimple_Impl for NodeProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        let wanted = Pattern::from_id(pattern.0);
        let supported =
            self.with_node(|_, node| wanted.is_some_and(|p| patterns_of(node).contains(&p)))?;
        if supported {
            Ok(self.to_interface::<IRawElementProviderSimple>().cast()?)
        } else {
            none()
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        self.with_node(|tree, node| {
            let name = tree.name(node.id);
            match property {
                UIA_ControlTypePropertyId => variant_i4(match &node.window {
                    Some(window) if window.modal => control_type::WINDOW,
                    Some(window) if window.popup => control_type::PANE,
                    _ => control_type_of(node.role()),
                }),
                UIA_IsDialogPropertyId => {
                    variant_bool(node.window.as_ref().is_some_and(|window| window.modal))
                }
                UIA_HeadingLevelPropertyId => variant_i4(heading_level(node)),
                UIA_LiveSettingPropertyId => variant_i4(match node.live_region {
                    dsa_host_core::accessibility::LiveRegion::Off => Off.0,
                    dsa_host_core::accessibility::LiveRegion::Polite => Polite.0,
                    dsa_host_core::accessibility::LiveRegion::Assertive => Assertive.0,
                }),
                UIA_PositionInSetPropertyId | UIA_SizeOfSetPropertyId => {
                    match patterns::place_in_set(tree, node) {
                        Some((place, _)) if property == UIA_PositionInSetPropertyId => {
                            variant_i4(place)
                        }
                        Some((_, size)) => variant_i4(size),
                        None => variant_empty(),
                    }
                }
                UIA_ExpandCollapseExpandCollapseStatePropertyId
                    if node.supports(NodeAction::Expand) || node.supports(NodeAction::Collapse) =>
                {
                    variant_i4(patterns::expand_state(node).0)
                }
                UIA_RangeValueValuePropertyId if node.range.is_some() => {
                    variant_r8(f64::from(node.range.map_or(0.0, |range| range.current)))
                }
                UIA_NamePropertyId => variant_str(&name),
                UIA_AutomationIdPropertyId if !node.view_id.is_empty() => {
                    variant_str(&node.view_id)
                }
                UIA_ClassNamePropertyId => variant_str(&node.class_name),
                UIA_FrameworkIdPropertyId => variant_str("Android"),
                UIA_HelpTextPropertyId if !node.hint.is_empty() => variant_str(&node.hint),
                UIA_ItemStatusPropertyId if !node.state_description.is_empty() => {
                    variant_str(&node.state_description)
                }
                UIA_IsEnabledPropertyId => variant_bool(node.states.enabled),
                UIA_HasKeyboardFocusPropertyId => {
                    variant_bool(tree.client_focus() == Some(node.id))
                }
                UIA_IsKeyboardFocusablePropertyId => variant_bool(node.states.focusable),
                UIA_IsPasswordPropertyId => variant_bool(node.states.password),
                UIA_IsControlElementPropertyId => variant_bool(is_control_element(node, &name)),
                UIA_IsContentElementPropertyId => variant_bool(is_content_element(node, &name)),
                UIA_ValueValuePropertyId if node.states.editable => {
                    variant_str(&node.value().unwrap_or_default())
                }
                UIA_ValueIsReadOnlyPropertyId if node.states.editable => {
                    variant_bool(!node.supports(NodeAction::SetText))
                }
                UIA_ToggleToggleStatePropertyId if node.states.checkable => {
                    variant_i4(if node.states.checked {
                        ToggleState_On.0
                    } else {
                        ToggleState_Off.0
                    })
                }
                UIA_SelectionItemIsSelectedPropertyId => variant_bool(node.states.selected),
                _ => variant_empty(),
            }
        })
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for NodeProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        let captions = self.shared.lock().caption.kinds();
        // A top-level node's siblings are the root's children, the title bar's buttons among
        // them.
        let sibling = |tree: &Tree, id: u32, forward: bool| match tree.parent(id) {
            Some(_) => tree.sibling(id, forward).map(Target::Node),
            None => match root_sibling(
                &root_children(&captions, tree.roots()),
                RootChild::Node(id),
                forward,
            ) {
                Some(RootChild::Caption(kind)) => Some(Target::Caption(kind)),
                Some(RootChild::Node(id)) => Some(Target::Node(id)),
                Some(RootChild::Loading) | None => None,
            },
        };
        let target = self.with_node(|tree, node| match direction {
            NavigateDirection_Parent => {
                Some(tree.parent(node.id).map_or(Target::Root, Target::Node))
            }
            NavigateDirection_NextSibling => sibling(tree, node.id, true),
            NavigateDirection_PreviousSibling => sibling(tree, node.id, false),
            NavigateDirection_FirstChild => node.children.first().copied().map(Target::Node),
            NavigateDirection_LastChild => node.children.last().copied().map(Target::Node),
            _ => None,
        })?;
        match target {
            Some(Target::Root) => Ok(root_of(&self.shared).cast()?),
            Some(Target::Caption(kind)) => Ok(caption_of(&self.shared, kind)),
            Some(Target::Node(id)) => Ok(node_of(&self.shared, self.epoch, id)),
            None => none(),
        }
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        safe_array(&runtime_id(self.epoch, self.id))
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        let (origin, client) = self.shared.client();
        let (geometry, origin) = {
            let state = self.shared.lock();
            (state.geometry(client), state.frame_origin(origin, client))
        };
        let bounds = self.with_node(|_, node| node.bounds)?;
        let rect = to_screen(bounds, geometry, origin);
        Ok(UiaRect {
            left: rect.left,
            top: rect.top,
            width: rect.width,
            height: rect.height,
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> windows::core::Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        let focus = self.with_node(|_, node| {
            // A node that takes no input focus takes the accessibility focus, which moves
            // Android's own screen reader cursor and the host's client focus alike.
            if node.supports(NodeAction::Focus) {
                NodeAction::Focus
            } else {
                NodeAction::AccessibilityFocus
            }
        })?;
        self.act(focus, String::new())
    }

    fn FragmentRoot(&self) -> windows::core::Result<IRawElementProviderFragmentRoot> {
        Ok(root_of(&self.shared))
    }
}

enum Target {
    Root,
    Caption(CaptionKind),
    Node(u32),
}

/// What a title bar button says of `property`, its title bar being `cells`, named in
/// `language`.
fn caption_property(
    kind: CaptionKind,
    cells: &CaptionCells,
    property: UIA_PROPERTY_ID,
    language: UiLanguage,
) -> VARIANT {
    match property {
        UIA_ControlTypePropertyId => variant_i4(UIA_ButtonControlTypeId.0),
        UIA_NamePropertyId => variant_str(caption_name(kind, cells.maximized, language)),
        UIA_AutomationIdPropertyId => variant_str(kind.automation_id()),
        UIA_AcceleratorKeyPropertyId => match kind.accelerator() {
            Some(keys) => variant_str(keys),
            None => variant_empty(),
        },
        UIA_IsEnabledPropertyId => variant_bool(cells.enabled(kind)),
        // A hidden title bar's buttons are still there, at the monitor's top edge, and come
        // out with Alt or the pointer.
        UIA_IsOffscreenPropertyId => variant_bool(cells.offscreen),
        UIA_IsKeyboardFocusablePropertyId => variant_bool(false),
        UIA_HasKeyboardFocusPropertyId => variant_bool(false),
        UIA_IsControlElementPropertyId => variant_bool(true),
        UIA_IsContentElementPropertyId => variant_bool(true),
        UIA_IsInvokePatternAvailablePropertyId => variant_bool(true),
        _ => variant_empty(),
    }
}

impl CaptionProvider {
    /// The title bar while it has this button; an error once it is gone.
    fn cells(&self) -> windows::core::Result<CaptionCells> {
        let cells = self.shared.lock().caption;
        cells.cell(self.kind).ok_or_else(not_available)?;
        Ok(cells)
    }
}

impl IRawElementProviderSimple_Impl for CaptionProvider_Impl {
    fn ProviderOptions(&self) -> windows::core::Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> windows::core::Result<IUnknown> {
        self.cells()?;
        if pattern == UIA_InvokePatternId {
            Ok(self.to_interface::<IRawElementProviderSimple>().cast()?)
        } else {
            none()
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> windows::core::Result<VARIANT> {
        let cells = self.cells()?;
        Ok(caption_property(
            self.kind,
            &cells,
            property,
            display_language(),
        ))
    }

    fn HostRawElementProvider(&self) -> windows::core::Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for CaptionProvider_Impl {
    fn Navigate(
        &self,
        direction: NavigateDirection,
    ) -> windows::core::Result<IRawElementProviderFragment> {
        if direction == NavigateDirection_Parent {
            return root_of(&self.shared).cast();
        }
        let forward = match direction {
            NavigateDirection_NextSibling => true,
            NavigateDirection_PreviousSibling => false,
            // The button has no children.
            _ => return none(),
        };
        let (epoch, child) = {
            let state = self.shared.lock();
            let captions = state.caption.kinds();
            if !captions.contains(&self.kind) {
                return Err(not_available());
            }
            let epoch = state.tree.epoch();
            let roots: &[u32] = if epoch.is_some() {
                state.tree.roots()
            } else {
                &[]
            };
            let children = root_children_of(&captions, roots, epoch.is_none());
            (
                epoch,
                root_sibling(&children, RootChild::Caption(self.kind), forward),
            )
        };
        child_of(&self.shared, epoch, child)
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        safe_array(&self.kind.runtime_id())
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        let cells = self.cells()?;
        let (left, top, width, height) = cells.cell(self.kind).ok_or_else(not_available)?;
        let (origin, _) = self.shared.client();
        Ok(UiaRect {
            left: (origin.0 + left) as f64,
            top: (origin.1 + top) as f64,
            width: width as f64,
            height: height as f64,
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> windows::core::Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        // A caption button takes no keyboard focus, as Windows' own do not.
        Err(invalid_operation())
    }

    fn FragmentRoot(&self) -> windows::core::Result<IRawElementProviderFragmentRoot> {
        Ok(root_of(&self.shared))
    }
}

impl IInvokeProvider_Impl for CaptionProvider_Impl {
    fn Invoke(&self) -> windows::core::Result<()> {
        self.shared.press(self.kind)
    }
}

impl IInvokeProvider_Impl for NodeProvider_Impl {
    fn Invoke(&self) -> windows::core::Result<()> {
        self.act(NodeAction::Click, String::new())
    }
}

impl IValueProvider_Impl for NodeProvider_Impl {
    fn SetValue(&self, value: &PCWSTR) -> windows::core::Result<()> {
        // SAFETY: UI Automation hands a NUL-terminated string that lives through the call.
        let text = unsafe { value.to_string() }.map_err(|_| invalid_operation())?;
        self.act(NodeAction::SetText, text)
    }

    fn Value(&self) -> windows::core::Result<BSTR> {
        self.with_node(|_, node| BSTR::from(node.value().unwrap_or_default()))
    }

    fn IsReadOnly(&self) -> windows::core::Result<BOOL> {
        self.with_node(|_, node| BOOL::from(!node.supports(NodeAction::SetText)))
    }
}

impl IToggleProvider_Impl for NodeProvider_Impl {
    fn Toggle(&self) -> windows::core::Result<()> {
        self.act(NodeAction::Click, String::new())
    }

    fn ToggleState(&self) -> windows::core::Result<ToggleState> {
        self.with_node(|_, node| {
            if node.states.checked {
                ToggleState_On
            } else {
                ToggleState_Off
            }
        })
    }
}

impl ISelectionItemProvider_Impl for NodeProvider_Impl {
    fn Select(&self) -> windows::core::Result<()> {
        let action = self.with_node(|_, node| {
            if node.supports(NodeAction::Select) {
                NodeAction::Select
            } else {
                NodeAction::Click
            }
        })?;
        self.act(action, String::new())
    }

    fn AddToSelection(&self) -> windows::core::Result<()> {
        ISelectionItemProvider_Impl::Select(self)
    }

    fn RemoveFromSelection(&self) -> windows::core::Result<()> {
        Err(invalid_operation())
    }

    fn IsSelected(&self) -> windows::core::Result<BOOL> {
        self.with_node(|_, node| BOOL::from(node.states.selected))
    }

    fn SelectionContainer(&self) -> windows::core::Result<IRawElementProviderSimple> {
        let parent = self.with_node(|tree, node| tree.parent(node.id))?;
        match parent {
            Some(id) => Ok(node_of(&self.shared, self.epoch, id).cast()?),
            None => none(),
        }
    }
}

impl IScrollProvider_Impl for NodeProvider_Impl {
    fn Scroll(
        &self,
        horizontal: ScrollAmount,
        vertical: ScrollAmount,
    ) -> windows::core::Result<()> {
        // Android scrolls a view forward or backward, whichever way it scrolls.
        match scroll_action(vertical.0).or_else(|| scroll_action(horizontal.0)) {
            Some(action) => self.act(action, String::new()),
            None => Ok(()),
        }
    }

    fn SetScrollPercent(&self, horizontal: f64, vertical: f64) -> windows::core::Result<()> {
        // A list that says how many items it has scrolls to the one at the percent, made or
        // not: the way a client reaches items a virtualized list keeps off the screen.
        let (rows, columns) = self.with_node(|_, node| {
            node.collection
                .map_or((0, 0), |collection| (collection.rows, collection.columns))
        })?;
        let wanted = |percent: f64| percent != UIA_ScrollPatternNoScroll;
        if rows <= 0 && columns <= 0 {
            return Err(invalid_operation());
        }
        let row = if wanted(vertical) {
            row_at_percent(rows, vertical)
        } else {
            0
        };
        let column = if wanted(horizontal) {
            row_at_percent(columns, horizontal)
        } else {
            0
        };
        self.act_with(ActionRequest {
            action: NodeAction::ScrollToPosition,
            position: (row, column),
            ..ActionRequest::default()
        })
    }

    fn HorizontalScrollPercent(&self) -> windows::core::Result<f64> {
        Ok(UIA_ScrollPatternNoScroll)
    }

    fn VerticalScrollPercent(&self) -> windows::core::Result<f64> {
        self.with_node(|tree, node| patterns::list_scroll(tree, node).map(|(percent, _)| percent))
            .map(|percent| percent.unwrap_or(UIA_ScrollPatternNoScroll))
    }

    fn HorizontalViewSize(&self) -> windows::core::Result<f64> {
        Ok(100.0)
    }

    fn VerticalViewSize(&self) -> windows::core::Result<f64> {
        self.with_node(|tree, node| patterns::list_scroll(tree, node).map(|(_, view)| view))
            .map(|view| view.unwrap_or(100.0))
    }

    fn HorizontallyScrollable(&self) -> windows::core::Result<BOOL> {
        Ok(BOOL::from(false))
    }

    fn VerticallyScrollable(&self) -> windows::core::Result<BOOL> {
        self.with_node(|_, node| BOOL::from(node.states.scrollable))
    }
}

/// The watch thread of a window, while it runs.
struct Watch {
    stop: Arc<AtomicBool>,
    /// Whether the watch is for one read and ends by itself; cleared when an assistive
    /// technology comes while it runs, which keeps it.
    once: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// Whether an assistive technology uses the window: Windows says a screen reader is on, or a
/// client subscribed to the window's focus changes.
fn assistive(shared: &Shared) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETSCREENREADER};
    let mut on: i32 = 0;
    // SAFETY: SPI_GETSCREENREADER writes one BOOL to the pointer it is given.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETSCREENREADER,
            0,
            &mut on as *mut i32 as *mut core::ffi::c_void,
            0,
        )
    };
    (read != 0 && on != 0) || shared.focus_listeners.load(Ordering::Relaxed) > 0
}

/// A task window's UI Automation tree, kept by the window on its UI thread.
pub struct AccessibilityWindow {
    task_id: i32,
    shared: Option<Arc<Shared>>,
    root: Option<IRawElementProviderSimple>,
    demand: ClientDemand,
    watch: Option<Watch>,
    connect: Option<ApiConnect>,
    logger: Option<Arc<CompositorLogger>>,
    task_size: (u32, u32),
    /// How large the window shows each task pixel (window pixels per task pixels).
    scale: (u32, u32),
    /// Whether the window holds the size the desktop gave it.
    held: bool,
    /// The title bar's buttons as the window draws them now.
    caption: CaptionCells,
    /// Where the back button's Back goes.
    input: Sender<InputEvent>,
    /// The task the window shows, whose tree is watched: another than `task_id` once a task
    /// took the window over after a GPU reset.
    tasks: WindowTasks,
    /// Whether the window still says it is opening its app, until the tick that sees the
    /// first frame tells clients it no longer is.
    opening: bool,
}

impl AccessibilityWindow {
    /// A window's tree, which watches nothing until a client asks for it. Without `connect`
    /// the window answers clients with an empty tree. The back button's Back goes to `input`.
    /// The tree watched is that of the task `tasks` says the window shows.
    pub fn new(
        task_id: i32,
        task_size: (u32, u32),
        input: Sender<InputEvent>,
        tasks: WindowTasks,
        connect: Option<ApiConnect>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> AccessibilityWindow {
        AccessibilityWindow {
            tasks,
            task_id,
            shared: None,
            root: None,
            demand: ClientDemand::default(),
            watch: None,
            connect,
            logger,
            task_size,
            scale: (1, 1),
            held: false,
            caption: CaptionCells::default(),
            input,
            opening: true,
        }
    }

    /// The window holds the size the desktop gave it (maximised, snapped, full screen), so a
    /// frame of another size is in its middle, or takes the size of its frames again.
    pub fn set_held(&mut self, held: bool) {
        self.held = held;
        if let Some(shared) = &self.shared {
            shared.lock().held = held;
        }
    }

    /// The title bar's buttons as the window draws them now. A client that reads the window
    /// is told which buttons came, went, moved, were renamed, hidden, shown or enabled.
    pub fn set_caption(&mut self, hwnd: isize, cells: CaptionCells) {
        if self.caption == cells {
            return;
        }
        self.caption = cells;
        if let Some(shared) = &self.shared {
            let mut state = shared.lock();
            let changes = caption_changes(&state.caption, &cells);
            state.caption = cells;
            if !changes.is_empty() {
                state.caption_changes.extend(changes);
                drop(state);
                post_events(hwnd);
            }
        }
    }

    /// The task's size in Android changed.
    pub fn task_resized(&mut self, task_size: (u32, u32)) {
        self.task_size = task_size;
        if let Some(shared) = &self.shared {
            shared.lock().task_size = task_size;
        }
    }

    /// The window shows the task's frames at `scale` (window pixels per task pixels) from now
    /// on, so a node's bounds on the screen are its bounds in the task at that scale.
    pub fn set_scale(&mut self, scale: (u32, u32)) {
        self.scale = scale;
        if let Some(shared) = &self.shared {
            shared.lock().scale = scale;
        }
    }

    fn shared(&mut self, hwnd: isize) -> Arc<Shared> {
        if let Some(shared) = &self.shared {
            return Arc::clone(shared);
        }
        let task_size = self.task_size;
        let shared = Arc::new(Shared {
            hwnd,
            task_id: self.task_id,
            state: Mutex::new(State {
                tree: Tree::new(),
                task_size,
                scale: self.scale,
                held: self.held,
                queued: Vec::new(),
                watching: false,
                wants_read: false,
                wants_action: false,
                announcements: Vec::new(),
                next_request: 0,
                pending: Vec::new(),
                caption: self.caption,
                caption_changes: Vec::new(),
                input: self.input.clone(),
            }),
            focus_listeners: AtomicI32::new(0),
            fast: AtomicBool::new(false),
        });
        self.shared = Some(Arc::clone(&shared));
        shared
    }

    /// Answers `WM_GETOBJECT`. A request for the window's UI Automation tree starts the
    /// watch if it is not running; anything else goes to the default procedure.
    ///
    /// # Safety
    ///
    /// Called from the window's procedure with its own message.
    pub unsafe fn get_object(
        &mut self,
        hwnd: isize,
        wparam: usize,
        lparam: isize,
    ) -> Option<isize> {
        if lparam as i32 != UiaRootObjectId {
            return None;
        }
        let shared = self.shared(hwnd);
        let reading = self.watch.is_some();
        if let Some((demand, why)) = self
            .demand
            .asked(Instant::now(), assistive(&shared), reading)
        {
            self.start_watch(&shared, demand, why);
        }
        let root: IRawElementProviderSimple = match &self.root {
            Some(root) => root.clone(),
            None => {
                let root: IRawElementProviderSimple = root_of(&shared).cast().ok()?;
                self.root = Some(root.clone());
                root
            }
        };
        Some(
            UiaReturnRawElementProvider(
                HWND(hwnd as *mut _),
                WPARAM(wparam),
                LPARAM(lparam),
                &root,
            )
            .0,
        )
    }

    /// Starts the watch `demand` asks for, saying `why` in the log. A watch for one read that
    /// runs already becomes continuous when an assistive technology came.
    fn start_watch(&mut self, shared: &Arc<Shared>, demand: Demand, why: DemandReason) {
        let window = self.task_id;
        let task_id = self.tasks.task_of(window);
        if let Some(watch) = &self.watch {
            if demand == Demand::Continuous && watch.once.swap(false, Ordering::Relaxed) {
                note(
                    self.logger.as_deref(),
                    &format!("task {task_id}: {why}; watching its accessibility tree from now on"),
                );
            }
            return;
        }
        let Some(connect) = self.connect.clone() else {
            return;
        };
        shared.lock().watching = true;
        let stop = Arc::new(AtomicBool::new(false));
        let once = Arc::new(AtomicBool::new(demand == Demand::Snapshot));
        let tasks = self.tasks.clone();
        let thread_shared = Arc::clone(shared);
        let thread_stop = Arc::clone(&stop);
        let thread_once = Arc::clone(&once);
        let logger = self.logger.clone();
        note(
            self.logger.as_deref(),
            &format!(
                "task {task_id}: {why}; {}",
                match demand {
                    Demand::Continuous => "watching its accessibility tree",
                    Demand::Snapshot => "reading its accessibility tree once",
                }
            ),
        );
        let spawned = thread::Builder::new()
            .name("dsa-app-window-accessibility".into())
            .spawn(move || {
                watch(
                    window,
                    tasks,
                    connect,
                    thread_shared,
                    thread_stop,
                    thread_once,
                    logger,
                )
            });
        match spawned {
            Ok(thread) => self.watch = Some(Watch { stop, once, thread }),
            Err(error) => {
                note(
                    self.logger.as_deref(),
                    &format!("could not watch the accessibility tree: {error}"),
                );
                shared.lock().watching = false;
                self.demand.end();
            }
        }
    }

    /// Ends the watch. A continuous watch forgets the tree, since nothing keeps it current
    /// any more; one that read the tree once leaves it for the next read to replace.
    fn stop_watch(&mut self, why: Option<DemandReason>) {
        let mut once = false;
        if let Some(watch) = self.watch.take() {
            watch.stop.store(true, Ordering::Relaxed);
            once = watch.once.load(Ordering::Relaxed);
            let _ = watch.thread.join();
            if let Some(why) = why {
                note(
                    self.logger.as_deref(),
                    &format!(
                        "task {}: {why}; stopped watching its accessibility tree",
                        self.task_id
                    ),
                );
            }
        }
        if let Some(shared) = &self.shared {
            let mut state = shared.lock();
            state.watching = false;
            shared.fast.store(false, Ordering::Relaxed);
            if !once {
                state.queued.clear();
                let names = names_of(&state.tree);
                let changes = state.tree.clear();
                state.pending.push((changes, names));
            }
        }
    }

    /// Reads the tree once for a client that is no assistive technology, which read the
    /// window or asked for an action while no watch ran. Called on the UI thread when the
    /// window hears of it.
    pub fn serve(&mut self, hwnd: isize) {
        let Some(shared) = self.shared.clone() else {
            return;
        };
        let (read, action) = {
            let mut state = shared.lock();
            (
                std::mem::take(&mut state.wants_read),
                std::mem::take(&mut state.wants_action),
            )
        };
        let now = Instant::now();
        let reading = self.watch.is_some();
        let wanted = if action {
            self.demand.acted(now, reading)
        } else if read {
            self.demand.asked(now, assistive(&shared), reading)
        } else {
            None
        };
        if let Some((demand, why)) = wanted {
            self.start_watch(&shared, demand, why);
            post_events(hwnd);
        }
    }

    /// Checks, every so often on the UI thread, whether clients have gone, and whether the
    /// watch should tell Android a screen reader listens.
    pub fn tick(&mut self, hwnd: isize) {
        // SAFETY: a plain query.
        let listening = unsafe { UiaClientsAreListening() }.as_bool();
        let assistive = self.shared.as_deref().is_some_and(assistive);
        if let Some((PresenceChange::Stop, why)) =
            self.demand.poll(Instant::now(), listening, assistive)
        {
            self.stop_watch(Some(why));
            post_events(hwnd);
        }
        if let Some(shared) = &self.shared {
            shared.fast.store(
                self.demand.continuous() && assistive && listening,
                Ordering::Relaxed,
            );
        }
        // A watch that ended by itself (one read done, or the service went away for good)
        // lets a later request start a new one.
        if self
            .watch
            .as_ref()
            .is_some_and(|watch| watch.thread.is_finished())
        {
            let once = self
                .watch
                .as_ref()
                .is_some_and(|watch| watch.once.load(Ordering::Relaxed));
            self.stop_watch(None);
            if !once {
                self.demand.end();
            }
            // Actions that came as the read ended start the next one.
            let waiting = self
                .shared
                .as_ref()
                .is_some_and(|shared| !shared.lock().queued.is_empty());
            if waiting {
                if let Some(shared) = &self.shared {
                    shared.lock().wants_action = true;
                }
                self.serve(hwnd);
            }
        }
        self.opened(hwnd);
    }

    /// Once the app's first frame is up, tells a client that read the window while it opened
    /// that it no longer is opening: its item status empties and it is no longer busy.
    fn opened(&mut self, hwnd: isize) {
        if !self.opening {
            return;
        }
        let Some(shared) = &self.shared else {
            // No client has read the window: nobody was told it was opening.
            if !shared_loading(hwnd) {
                self.opening = false;
            }
            return;
        };
        if shared.loading() {
            return;
        }
        self.opening = false;
        let Some(root) = &self.root else {
            return;
        };
        // SAFETY: a plain query, then UI Automation's event call with this window's root.
        unsafe {
            if !UiaClientsAreListening().as_bool() {
                return;
            }
            let old = variant_str(loading_status(display_language(), true).unwrap_or_default());
            let new = variant_empty();
            let _ =
                UiaRaiseAutomationPropertyChangedEvent(root, UIA_ItemStatusPropertyId, &old, &new);
            free_variant(old);
            free_variant(new);
        }
    }

    /// Where the window raises its events, once a client has asked for its tree.
    pub fn events(&self) -> Option<EventTarget> {
        Some(EventTarget {
            shared: self.shared.clone()?,
            root: self.root.clone()?,
            logger: self.logger.clone(),
        })
    }
}

/// Where a window raises its events, taken from its tree so no borrow of it is held while UI
/// Automation calls back into the window.
pub struct EventTarget {
    logger: Option<Arc<CompositorLogger>>,
    shared: Arc<Shared>,
    root: IRawElementProviderSimple,
}

/// Asks the window to raise the events of the changes that came, from any thread.
fn post_events(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
    // SAFETY: posting to the window; a stale handle only fails.
    unsafe {
        PostMessageW(
            hwnd as windows_sys::Win32::Foundation::HWND,
            WM_APP_ACCESSIBILITY,
            0,
            0,
        );
    }
}

impl EventTarget {
    /// Raises the UI Automation events of the changes the watch received.
    pub fn raise(&self, hwnd: isize) {
        let shared = Arc::clone(&self.shared);
        let (pending, caption_changes, announcements, epoch) = {
            let mut state = shared.lock();
            (
                std::mem::take(&mut state.pending),
                std::mem::take(&mut state.caption_changes),
                std::mem::take(&mut state.announcements),
                state.tree.epoch(),
            )
        };
        if pending.is_empty() && caption_changes.is_empty() && announcements.is_empty() {
            return;
        }
        // SAFETY: a plain query.
        if !unsafe { UiaClientsAreListening() }.as_bool() {
            return;
        }
        let root = self.root.clone();
        let language = display_language();
        for (kind, change) in caption_changes {
            raise_caption(&shared, &root, kind, change, language);
        }
        for announcement in &announcements {
            patterns::raise_announcement(&root, announcement);
        }
        let foreground = {
            use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
            // SAFETY: a plain query.
            unsafe { GetForegroundWindow() as isize == hwnd }
        };
        for (changes, before) in pending {
            let focus = raise(&shared, &root, epoch, &changes, &before, foreground);
            if focus.is_some() {
                let (focused, accessibility_focused) = {
                    let state = shared.lock();
                    (state.tree.focused(), state.tree.accessibility_focused())
                };
                note(
                    self.logger.as_deref(),
                    &focus_trace_line(
                        AccessibilityTarget::Task(shared.task_id),
                        focused,
                        accessibility_focused,
                        "the window raised UI Automation's focus event",
                        wall_millis(),
                    ),
                );
            }
        }
    }
}

impl AccessibilityWindow {
    /// The window is closing: stops the watch and lets clients go.
    pub fn close(&mut self, hwnd: isize) {
        self.stop_watch(None);
        self.demand.end();
        // SAFETY: the documented way for a window that answered WM_GETOBJECT to say it is
        // going, and to let go of its providers.
        unsafe {
            UiaReturnRawElementProvider(
                HWND(hwnd as *mut _),
                WPARAM(0),
                LPARAM(0),
                None::<&IRawElementProviderSimple>,
            );
            if let Some(root) = self.root.take() {
                let _ = UiaDisconnectProvider(&root);
            }
        }
    }
}

fn names_of(tree: &Tree) -> Vec<(u32, String)> {
    let mut names = Vec::new();
    let mut stack: Vec<u32> = tree.roots().to_vec();
    while let Some(id) = stack.pop() {
        if let Some(node) = tree.node(id) {
            names.push((id, tree.name(id)));
            stack.extend(node.children.iter().copied());
        }
    }
    names
}

/// What a batch raises first: the structure of a replaced tree, so the node a client is sent
/// to is there, then the focus, so a screen reader starts speaking before the property and
/// text events of the same batch, then the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaiseStep {
    Structure,
    Focus,
    Rest,
}

/// The order [`raise`] raises a batch's events in.
fn raise_order(changes: &Changes, foreground: bool) -> Vec<RaiseStep> {
    let mut steps = Vec::new();
    if changes.replaced || changes.roots_changed {
        steps.push(RaiseStep::Structure);
    }
    if changes.focus.is_some() && foreground {
        steps.push(RaiseStep::Focus);
    }
    steps.push(RaiseStep::Rest);
    steps
}

/// Raises the events of one batch of changes, in [`raise_order`]. The node whose focus event
/// it raised, if any.
fn raise(
    shared: &Arc<Shared>,
    root: &IRawElementProviderSimple,
    epoch: Option<u32>,
    changes: &Changes,
    before: &[(u32, String)],
    foreground: bool,
) -> Option<u32> {
    let mut raised = None;
    // SAFETY: UI Automation's event calls, with providers of this window.
    unsafe {
        let steps = raise_order(changes, foreground);
        if steps.contains(&RaiseStep::Structure) {
            let _ = UiaRaiseStructureChangedEvent(
                root,
                StructureChangeType_ChildrenInvalidated,
                std::ptr::null_mut(),
                0,
            );
        }
        let epoch = epoch?;
        let simple = |id: u32| -> Option<IRawElementProviderSimple> {
            node_of(shared, epoch, id).cast().ok()
        };
        if steps.contains(&RaiseStep::Focus) {
            if let Some(focus) = changes.focus {
                if let Some(provider) = simple(focus) {
                    let _ = UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId);
                    raised = Some(focus);
                }
            }
        }
        if !changes.replaced {
            for id in &changes.children_changed {
                if let Some(provider) = simple(*id) {
                    let mut runtime = runtime_id(epoch, *id);
                    let _ = UiaRaiseStructureChangedEvent(
                        &provider,
                        StructureChangeType_ChildrenInvalidated,
                        runtime.as_mut_ptr(),
                        runtime.len() as i32,
                    );
                }
            }
            let state = shared.lock();
            let mut named: HashSet<u32> = HashSet::new();
            let old_names: std::collections::HashMap<u32, &String> =
                before.iter().map(|(id, name)| (*id, name)).collect();
            let mut events: Vec<(u32, UIA_PROPERTY_ID, VARIANT, VARIANT)> = Vec::new();
            for old in &changes.changed {
                let Some(new) = state.tree.node(old.id) else {
                    continue;
                };
                if old.states.checked != new.states.checked && new.states.checkable {
                    let state_of = |on: bool| {
                        if on {
                            ToggleState_On.0
                        } else {
                            ToggleState_Off.0
                        }
                    };
                    events.push((
                        new.id,
                        UIA_ToggleToggleStatePropertyId,
                        variant_i4(state_of(old.states.checked)),
                        variant_i4(state_of(new.states.checked)),
                    ));
                }
                if old.states.selected != new.states.selected {
                    events.push((
                        new.id,
                        UIA_SelectionItemIsSelectedPropertyId,
                        variant_bool(old.states.selected),
                        variant_bool(new.states.selected),
                    ));
                }
                if old.states.enabled != new.states.enabled {
                    events.push((
                        new.id,
                        UIA_IsEnabledPropertyId,
                        variant_bool(old.states.enabled),
                        variant_bool(new.states.enabled),
                    ));
                }
                if old.value() != new.value() && new.states.editable {
                    events.push((
                        new.id,
                        UIA_ValueValuePropertyId,
                        variant_str(&old.value().unwrap_or_default()),
                        variant_str(&new.value().unwrap_or_default()),
                    ));
                }
                if old.bounds != new.bounds {
                    events.push((
                        new.id,
                        UIA_BoundingRectanglePropertyId,
                        variant_empty(),
                        variant_empty(),
                    ));
                }
                // A node's name may come from the nodes inside it: look up to the nearest
                // ancestor whose name changed with it.
                let mut at = Some(new.id);
                while let Some(id) = at {
                    let name = state.tree.name(id);
                    let was = old_names.get(&id).map(|name| name.as_str()).unwrap_or("");
                    if name != was && named.insert(id) {
                        events.push((id, UIA_NamePropertyId, variant_str(was), variant_str(&name)));
                    }
                    at = state.tree.parent(id);
                    if named.len() > MAX_PROPERTY_EVENTS {
                        break;
                    }
                }
            }
            drop(state);
            let many = events.len() > MAX_PROPERTY_EVENTS;
            if many {
                let _ = UiaRaiseStructureChangedEvent(
                    root,
                    StructureChangeType_ChildrenInvalidated,
                    std::ptr::null_mut(),
                    0,
                );
            }
            for (id, property, old, new) in events {
                if !many {
                    if let Some(provider) = simple(id) {
                        let _ =
                            UiaRaiseAutomationPropertyChangedEvent(&provider, property, &old, &new);
                    }
                }
                free_variant(old);
                free_variant(new);
            }
        }
        // Text, live regions, and windows that opened.
        patterns::raise_node_events(shared, epoch, changes);
    }
    raised
}

/// Raises the event of a change to the title bar's `kind` button: an added child is raised on
/// itself, a removed one on its parent with its runtime ID, as UI Automation asks, and a
/// change of its place, name, visibility or state as a property change on the button.
fn raise_caption(
    shared: &Arc<Shared>,
    root: &IRawElementProviderSimple,
    kind: CaptionKind,
    change: CaptionChange,
    language: UiLanguage,
) {
    let mut runtime = kind.runtime_id();
    let button = || {
        caption_of(shared, kind)
            .cast::<IRawElementProviderSimple>()
            .ok()
    };
    let property = |property: UIA_PROPERTY_ID, old: VARIANT, new: VARIANT| {
        if let Some(provider) = button() {
            // SAFETY: UI Automation's event call, with a provider of this window.
            unsafe {
                let _ = UiaRaiseAutomationPropertyChangedEvent(&provider, property, &old, &new);
            }
        }
        free_variant(old);
        free_variant(new);
    };
    match change {
        CaptionChange::Added => {
            if let Some(provider) = button() {
                // SAFETY: as above.
                unsafe {
                    let _ = UiaRaiseStructureChangedEvent(
                        &provider,
                        StructureChangeType_ChildAdded,
                        runtime.as_mut_ptr(),
                        runtime.len() as i32,
                    );
                }
            }
        }
        CaptionChange::Removed => {
            // SAFETY: as above, raised on the root the button was a child of.
            unsafe {
                let _ = UiaRaiseStructureChangedEvent(
                    root,
                    StructureChangeType_ChildRemoved,
                    runtime.as_mut_ptr(),
                    runtime.len() as i32,
                );
            }
        }
        CaptionChange::Moved => property(
            UIA_BoundingRectanglePropertyId,
            variant_empty(),
            variant_empty(),
        ),
        CaptionChange::Renamed { from, to } => property(
            UIA_NamePropertyId,
            variant_str(caption_name(kind, from, language)),
            variant_str(caption_name(kind, to, language)),
        ),
        CaptionChange::Offscreen(now) => property(
            UIA_IsOffscreenPropertyId,
            variant_bool(!now),
            variant_bool(now),
        ),
        CaptionChange::Enabled(now) => property(
            UIA_IsEnabledPropertyId,
            variant_bool(!now),
            variant_bool(now),
        ),
    }
}

/// Frees a VARIANT this module made.
fn free_variant(mut variant: VARIANT) {
    // SAFETY: a VARIANT made by one of the constructors above, cleared once.
    unsafe {
        let _ = windows::Win32::System::Variant::VariantClear(&mut variant);
    }
}

/// The watch thread: watches the tree of the task window `window` shows (as `tasks` says)
/// through the service until told to stop, and starts over when the connection ends, a tree
/// cannot be taken, or another task takes the window over after a GPU reset, whose tree is
/// then the one watched and acted on. It sends the actions clients queued, and tells the
/// service whether a screen reader listens (`shared.fast`).
///
/// While `once` holds, the watch is for one read: it ends once a tree has come and no action
/// it sent is younger than [`ACTION_LINGER`], or after [`SNAPSHOT_TIMEOUT`] without a tree.
fn watch(
    window: i32,
    tasks: WindowTasks,
    connect: ApiConnect,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    once: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let started = Instant::now();
    let mut got_tree = false;
    let mut last_action: Option<Instant> = None;
    let done = |got_tree: bool, last_action: Option<Instant>| {
        once.load(Ordering::Relaxed)
            && ((got_tree && last_action.is_none_or(|at| at.elapsed() >= ACTION_LINGER))
                || (!got_tree && started.elapsed() >= SNAPSHOT_TIMEOUT))
    };
    let post = |shared: &Shared| {
        use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
        // SAFETY: posting to the window; a stale handle only fails.
        unsafe {
            PostMessageW(
                shared.hwnd as windows_sys::Win32::Foundation::HWND,
                WM_APP_ACCESSIBILITY,
                0,
                0,
            );
        }
    };
    let reset = |shared: &Shared| {
        let mut state = shared.lock();
        let names = names_of(&state.tree);
        let changes = state.tree.clear();
        state.pending.push((changes, names));
        drop(state);
        post(shared);
    };
    let mut failures = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if done(got_tree, last_action) {
            return;
        }
        let task_id = tasks.task_of(window);
        let mut connector = connect();
        let stream = HostApiClient::connect(&mut *connector, BUILD, CONNECT)
            .and_then(|client| client.watch_accessibility(task_id));
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => {
                failures += 1;
                if failures == 1 {
                    note(
                        logger.as_deref(),
                        &format!("task {task_id}: could not watch the accessibility tree: {why}"),
                    );
                }
                sleep_unless_stopped(&stop, RETRY);
                continue;
            }
        };
        failures = 0;
        // A new watch starts as not listening.
        let mut told_fast = false;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let shown = tasks.task_of(window);
            if shown != task_id {
                // The task that ended has no tree; the one that took the window over has.
                // Actions asked on the old tree are for nodes the new one does not have.
                note(
                    logger.as_deref(),
                    &format!(
                        "task {shown} took the window of task {task_id} over; watching its \
                         accessibility tree"
                    ),
                );
                shared.lock().queued.clear();
                reset(&shared);
                break;
            }
            if done(got_tree, last_action) {
                return;
            }
            let fast = shared.fast.load(Ordering::Relaxed);
            if fast != told_fast {
                if stream.set_listening(fast).is_err() {
                    break;
                }
                told_fast = fast;
            }
            let queued = std::mem::take(&mut shared.lock().queued);
            let mut broken = false;
            for action in &queued {
                if stream.perform(action).is_err() {
                    broken = true;
                    break;
                }
                last_action = Some(Instant::now());
            }
            if broken {
                break;
            }
            match stream.next(POLL) {
                Ok(None) => {}
                Ok(Some(GuestAccessibility::Announcement(announcement))) => {
                    shared.lock().announcements.push(announcement);
                    post(&shared);
                }
                Ok(Some(GuestAccessibility::Tree(update))) => {
                    got_tree = true;
                    let mut state = shared.lock();
                    let names = names_of(&state.tree);
                    match state.tree.apply(update) {
                        Ok(changes) => {
                            if !changes.is_empty() {
                                state.pending.push((changes, names));
                                drop(state);
                                post(&shared);
                            }
                        }
                        Err(TreeError::Epoch { .. }) => {
                            // A change that missed its tree: watch again, for a full tree.
                            drop(state);
                            break;
                        }
                        Err(TreeError::Malformed(why)) => {
                            drop(state);
                            note(
                                logger.as_deref(),
                                &format!("task {task_id}: dropped an accessibility tree: {why}"),
                            );
                            reset(&shared);
                            break;
                        }
                    }
                }
                Ok(Some(GuestAccessibility::ActionResult(result))) => {
                    if let Err(why) = result.outcome {
                        note(
                            logger.as_deref(),
                            &format!("task {task_id}: an accessibility action was not done: {why}"),
                        );
                    }
                }
                Err(why) => {
                    note(
                        logger.as_deref(),
                        &format!("task {task_id}: the accessibility watch ended: {why}"),
                    );
                    reset(&shared);
                    sleep_unless_stopped(&stop, RETRY);
                    break;
                }
            }
        }
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, wait: Duration) {
    let until = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) && Instant::now() < until {
        thread::sleep(Duration::from_millis(50));
    }
}

/// UI Automation's own numbers for what [`crate::ui_automation`] names, checked here against
/// the Windows headers.
/// The user's display language now, which the buttons are named in.
fn display_language() -> UiLanguage {
    crate::ui_language::user_language()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn fr18_the_mapping_uses_ui_automations_numbers() {
        assert_eq!(control_type::BUTTON, UIA_ButtonControlTypeId.0);
        assert_eq!(control_type::LIST_ITEM, UIA_ListItemControlTypeId.0);
        assert_eq!(control_type::LIST, UIA_ListControlTypeId.0);
        assert_eq!(control_type::EDIT, UIA_EditControlTypeId.0);
        assert_eq!(control_type::PANE, UIA_PaneControlTypeId.0);
        assert_eq!(control_type::GROUP, UIA_GroupControlTypeId.0);
        assert_eq!(Pattern::Invoke.id(), UIA_InvokePatternId.0);
        assert_eq!(Pattern::Value.id(), UIA_ValuePatternId.0);
        assert_eq!(Pattern::Toggle.id(), UIA_TogglePatternId.0);
        assert_eq!(Pattern::SelectionItem.id(), UIA_SelectionItemPatternId.0);
        assert_eq!(Pattern::Scroll.id(), UIA_ScrollPatternId.0);
        assert_eq!(Pattern::Text.id(), UIA_TextPatternId.0);
        assert_eq!(Pattern::RangeValue.id(), UIA_RangeValuePatternId.0);
        assert_eq!(Pattern::ExpandCollapse.id(), UIA_ExpandCollapsePatternId.0);
        assert_eq!(Pattern::Grid.id(), UIA_GridPatternId.0);
        assert_eq!(Pattern::GridItem.id(), UIA_GridItemPatternId.0);
        assert_eq!(Pattern::Table.id(), UIA_TablePatternId.0);
        assert_eq!(Pattern::TableItem.id(), UIA_TableItemPatternId.0);
        assert_eq!(Pattern::ScrollItem.id(), UIA_ScrollItemPatternId.0);
        assert_eq!(
            Pattern::LegacyIAccessible.id(),
            UIA_LegacyIAccessiblePatternId.0
        );
        assert_eq!(control_type::WINDOW, UIA_WindowControlTypeId.0);
        assert_eq!(control_type::HYPERLINK, UIA_HyperlinkControlTypeId.0);
        assert_eq!(HeadingLevel_None.0, 80050);
        assert_eq!(HeadingLevel2.0, 80052);
        assert_eq!(ScrollAmount_LargeIncrement.0, 3);
        assert_eq!(ScrollAmount_SmallDecrement.0, 1);
    }

    /// A task kept at the density of a main monitor at 100% (130 dpi), on a monitor at 125%
    /// (164 dpi) left of and above the main one.
    #[test]
    fn fr18_on_a_monitor_of_another_scale_ui_automation_sees_a_node_where_the_window_shows_it() {
        use dsa_host_core::compositor::window_sizing::{
            density_for_dpi, scale_between, to_window_size,
        };
        let (input, _) = mpsc::channel();
        let mut window =
            AccessibilityWindow::new(6, (810, 1661), input, WindowTasks::new(), None, None);
        let scale = scale_between(density_for_dpi(120), density_for_dpi(96));
        // The scale the window manager says reaches a tree made before and after it.
        let before = window.shared(0);
        window.set_scale(scale);
        let after = {
            window.shared = None;
            window.shared(0)
        };
        let client = to_window_size((810, 1661), scale);
        let origin = (-2900, -880);
        for shared in [before, after] {
            let geometry = shared.lock().geometry(client);
            assert_eq!(geometry.scale, (82, 65));
            let button = dsa_host_core::tasks::Bounds {
                left: 650,
                top: 1300,
                right: 780,
                bottom: 1365,
            };
            let rect = to_screen(button, geometry, origin);
            assert_eq!(
                (rect.left, rect.top, rect.width, rect.height),
                (-2900.0 + 820.0, -880.0 + 1640.0, 164.0, 82.0)
            );
            // Voice Access clicks the middle of what it highlights: the button's middle.
            let middle = (rect.left + rect.width / 2.0, rect.top + rect.height / 2.0);
            assert_eq!(from_screen(middle, geometry, origin), (715, 1332));
        }
    }

    #[test]
    fn fr22_the_back_arrow_is_a_button_named_back_for_ui_automation() {
        use crate::ui_language::from_langid;
        use dsa_host_core::ui_text::caption::back_name;
        assert_eq!(back_name(UiLanguage::Korean), "뒤로");
        assert_eq!(back_name(UiLanguage::English), "Back");
        // The language IDs Windows gives Korean and US English.
        assert_eq!(back_name(from_langid(0x0412)), "뒤로");
        assert_eq!(back_name(from_langid(0x0409)), "Back");
    }

    #[test]
    fn fr17_the_default_action_is_read_out_in_every_language() {
        use crate::ui_language::assert_translation;
        for language in UiLanguage::ALL {
            for long in [false, true] {
                assert_translation(
                    language,
                    "the default action",
                    patterns::action_name_in(long, UiLanguage::English),
                    patterns::action_name_in(long, language),
                );
            }
        }
    }

    const BACK: RootChild = RootChild::Caption(CaptionKind::Back);

    #[test]
    fn fr22_the_back_button_is_the_roots_first_child_while_the_arrow_shows() {
        let back = [CaptionKind::Back];
        assert_eq!(
            root_children(&back, &[7, 9]),
            vec![BACK, RootChild::Node(7), RootChild::Node(9)]
        );
        assert_eq!(root_children(&back, &[]), vec![BACK]);
        assert_eq!(
            root_children(&[], &[7, 9]),
            vec![RootChild::Node(7), RootChild::Node(9)]
        );
        assert!(root_children(&[], &[]).is_empty());
    }

    #[test]
    fn fr22_the_back_button_and_the_tasks_nodes_are_siblings() {
        let children = root_children(&[CaptionKind::Back], &[7, 9]);
        assert_eq!(
            root_sibling(&children, BACK, true),
            Some(RootChild::Node(7))
        );
        assert_eq!(root_sibling(&children, BACK, false), None);
        assert_eq!(
            root_sibling(&children, RootChild::Node(7), false),
            Some(BACK)
        );
        assert_eq!(
            root_sibling(&children, RootChild::Node(7), true),
            Some(RootChild::Node(9))
        );
        assert_eq!(root_sibling(&children, RootChild::Node(9), true), None);
        // Without the arrow the first node has nothing before it.
        let children = root_children(&[], &[7, 9]);
        assert_eq!(root_sibling(&children, RootChild::Node(7), false), None);
        assert_eq!(root_sibling(&children, BACK, true), None);
    }

    /// A title bar 800 pixels wide at 100%, its icon's cell showing the back arrow when
    /// `back`.
    fn title_bar(back: bool) -> CaptionCells {
        CaptionCells::of(
            &CaptionLayout::new(800, 31, 96, 0),
            back,
            false,
            true,
            false,
        )
    }

    #[test]
    fn fr18_caption_buttons_follow_back_in_the_root_children() {
        let caption = RootChild::Caption;
        let cells = title_bar(true);
        assert_eq!(
            root_children(&cells.kinds(), &[7, 9]),
            vec![
                BACK,
                caption(CaptionKind::Minimize),
                caption(CaptionKind::Maximize),
                caption(CaptionKind::Close),
                RootChild::Node(7),
                RootChild::Node(9),
            ]
        );
        // On the first screen the icon's cell is the system menu, in the same place.
        let cells = title_bar(false);
        assert_eq!(
            cells.kinds(),
            vec![
                CaptionKind::SystemMenu,
                CaptionKind::Minimize,
                CaptionKind::Maximize,
                CaptionKind::Close,
            ]
        );
        assert_eq!(
            cells.cell(CaptionKind::SystemMenu),
            title_bar(true).cell(CaptionKind::Back)
        );
        // The buttons and the task's nodes are one row of siblings.
        let children = root_children(&cells.kinds(), &[7]);
        assert_eq!(
            root_sibling(&children, caption(CaptionKind::Close), true),
            Some(RootChild::Node(7))
        );
        assert_eq!(
            root_sibling(&children, RootChild::Node(7), false),
            Some(caption(CaptionKind::Close))
        );
        assert_eq!(
            root_sibling(&children, caption(CaptionKind::Minimize), false),
            Some(caption(CaptionKind::SystemMenu))
        );
        // A window too narrow for the icon keeps its three buttons.
        let narrow = CaptionCells::of(
            &CaptionLayout::new(100, 31, 96, 0),
            false,
            false,
            true,
            false,
        );
        assert_eq!(
            narrow.kinds(),
            vec![
                CaptionKind::Minimize,
                CaptionKind::Maximize,
                CaptionKind::Close
            ]
        );
        // Before the window laid its title bar out, the tree has only the task's nodes.
        assert!(CaptionCells::default().kinds().is_empty());
    }

    #[test]
    fn fr18_caption_buttons_are_where_the_window_draws_them() {
        let layout = CaptionLayout::new(800, 31, 96, 0);
        let cells = CaptionCells::of(&layout, false, false, true, false);
        assert_eq!(cells.cell(CaptionKind::SystemMenu), layout.icon_cell());
        for (kind, button) in [
            (CaptionKind::Minimize, CaptionButton::Minimize),
            (CaptionKind::Maximize, CaptionButton::Maximize),
            (CaptionKind::Close, CaptionButton::Close),
        ] {
            assert_eq!(cells.cell(kind), Some(layout.button_rect(button)));
            // A point in the middle of the cell finds that button.
            let (left, top, width, height) = layout.button_rect(button);
            let middle = (
                left as f64 + width as f64 / 2.0,
                top as f64 + height as f64 / 2.0,
            );
            assert_eq!(cells.at(middle.0, middle.1), Some(kind));
        }
        // Below the band is the app.
        assert_eq!(cells.at(790.0, 40.0), None);
    }

    /// The text a VARIANT this module made holds, if it holds text.
    fn text_of(variant: &VARIANT) -> Option<String> {
        // SAFETY: the member read is the one the tag names.
        unsafe {
            let inner = &variant.Anonymous.Anonymous;
            (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
        }
    }

    /// The truth a VARIANT this module made holds, if it holds one.
    fn truth_of(variant: &VARIANT) -> Option<bool> {
        // SAFETY: as above.
        unsafe {
            let inner = &variant.Anonymous.Anonymous;
            (inner.vt == VT_BOOL).then(|| inner.Anonymous.boolVal.0 != 0)
        }
    }

    const EN: UiLanguage = UiLanguage::English;
    const KO: UiLanguage = UiLanguage::Korean;

    fn name_of(kind: CaptionKind, cells: &CaptionCells, language: UiLanguage) -> String {
        let name = caption_property(kind, cells, UIA_NamePropertyId, language);
        let text = text_of(&name).expect("a name");
        free_variant(name);
        text
    }

    #[test]
    fn fr18_a_window_says_it_is_opening_its_app_until_the_first_frame() {
        assert_eq!(loading_status(EN, true), Some("Opening the app"));
        assert_eq!(loading_status(KO, true), Some("앱을 여는 중"));
        assert_eq!(loading_status(EN, false), None);
        assert_eq!(loading_status(KO, false), None);
    }

    #[test]
    fn fr18_a_window_without_a_first_frame_mark_is_loading() {
        // A handle that names no window carries no mark, as a window before its first frame.
        assert!(shared_loading(0));
    }

    #[test]
    fn fr18_caption_buttons_are_named_in_the_display_language() {
        let cells = title_bar(false);
        let names = |language| {
            [
                CaptionKind::SystemMenu,
                CaptionKind::Minimize,
                CaptionKind::Maximize,
                CaptionKind::Close,
            ]
            .map(|kind| name_of(kind, &cells, language))
        };
        assert_eq!(names(EN), ["System menu", "Minimize", "Maximize", "Close"]);
        assert_eq!(names(KO), ["시스템 메뉴", "최소화", "최대화", "닫기"]);
        assert_eq!(names(UiLanguage::German)[3], "Schließen");
        assert_eq!(name_of(CaptionKind::Back, &title_bar(true), KO), "뒤로");
        for kind in CaptionKind::ALL {
            let control = caption_property(kind, &cells, UIA_ControlTypePropertyId, EN);
            // SAFETY: the tag and member a VARIANT of an integer has.
            let control_type = unsafe {
                let inner = &control.Anonymous.Anonymous;
                (inner.vt == VT_I4).then_some(inner.Anonymous.lVal)
            };
            assert_eq!(control_type, Some(UIA_ButtonControlTypeId.0), "{kind:?}");
        }
    }

    #[test]
    fn fr18_maximize_button_is_named_restore_while_maximized() {
        let layout = CaptionLayout::new(800, 31, 96, 0);
        let maximized = CaptionCells::of(&layout, false, true, true, false);
        assert_eq!(name_of(CaptionKind::Maximize, &maximized, EN), "Restore");
        assert_eq!(
            name_of(CaptionKind::Maximize, &maximized, KO),
            "이전 크기로"
        );
        let normal = CaptionCells::of(&layout, false, false, true, false);
        assert_eq!(name_of(CaptionKind::Maximize, &normal, EN), "Maximize");
        assert_eq!(name_of(CaptionKind::Maximize, &normal, KO), "최대화");
        // A client is told the name changed, with both names.
        assert_eq!(
            caption_changes(&normal, &maximized),
            vec![(
                CaptionKind::Maximize,
                CaptionChange::Renamed {
                    from: false,
                    to: true
                }
            )]
        );
    }

    #[test]
    fn fr18_hidden_full_screen_caption_buttons_are_offscreen() {
        // A full screen window on a monitor 1920 pixels wide, its title bar hidden.
        let layout = CaptionLayout::new(1920, 31, 96, 0);
        let hidden = CaptionCells::of(&layout, false, true, true, true);
        for kind in hidden.kinds() {
            let offscreen = caption_property(kind, &hidden, UIA_IsOffscreenPropertyId, EN);
            assert_eq!(truth_of(&offscreen), Some(true), "{kind:?}");
        }
        // Hidden, no point finds them: the app is drawn there.
        let (left, _, _, _) = layout.button_rect(CaptionButton::Close);
        assert_eq!(hidden.at(left as f64 + 4.0, 10.0), None);
        // Revealed, they are on the screen, and each says so.
        let revealed = CaptionCells::of(&layout, false, true, true, false);
        let shown = caption_property(CaptionKind::Close, &revealed, UIA_IsOffscreenPropertyId, EN);
        assert_eq!(truth_of(&shown), Some(false));
        assert_eq!(
            revealed.at(left as f64 + 4.0, 10.0),
            Some(CaptionKind::Close)
        );
        let changes = caption_changes(&hidden, &revealed);
        assert_eq!(
            changes,
            revealed
                .kinds()
                .into_iter()
                .map(|kind| (kind, CaptionChange::Offscreen(false)))
                .collect::<Vec<_>>()
        );
        // Its middle button is Restore, which leaves full screen.
        assert_eq!(name_of(CaptionKind::Maximize, &hidden, EN), "Restore");
    }

    #[test]
    fn fr18_each_caption_button_does_what_a_click_on_it_does() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SC_CLOSE, SC_KEYMENU, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE,
        };
        let layout = CaptionLayout::new(800, 31, 96, 0);
        let normal = CaptionCells::of(&layout, false, false, true, false);
        // The icon opens the window menu as Alt+Space does.
        assert_eq!(
            caption_command(CaptionKind::SystemMenu, &normal),
            Some((SC_KEYMENU, b' ' as isize))
        );
        assert_eq!(
            caption_command(CaptionKind::Minimize, &normal),
            Some((SC_MINIMIZE, 0))
        );
        assert_eq!(
            caption_command(CaptionKind::Maximize, &normal),
            Some((SC_MAXIMIZE, 0))
        );
        assert_eq!(
            caption_command(CaptionKind::Close, &normal),
            Some((SC_CLOSE, 0))
        );
        let maximized = CaptionCells::of(&layout, false, true, true, false);
        assert_eq!(
            caption_command(CaptionKind::Maximize, &maximized),
            Some((SC_RESTORE, 0))
        );
        // The back arrow sends the task Back itself, with no command.
        assert_eq!(caption_command(CaptionKind::Back, &title_bar(true)), None);
        // A window fixed in size cannot be maximized: the button says so and does nothing.
        let fixed = CaptionCells::of(&layout, false, false, false, false);
        assert_eq!(caption_command(CaptionKind::Maximize, &fixed), None);
        let enabled = caption_property(CaptionKind::Maximize, &fixed, UIA_IsEnabledPropertyId, EN);
        assert_eq!(truth_of(&enabled), Some(false));
        assert_eq!(
            caption_changes(&normal, &fixed),
            vec![(CaptionKind::Maximize, CaptionChange::Enabled(false))]
        );
    }

    #[test]
    fn fr18_the_back_arrow_takes_the_place_of_the_system_menu() {
        assert_eq!(
            caption_changes(&title_bar(false), &title_bar(true)),
            vec![
                (CaptionKind::Back, CaptionChange::Added),
                (CaptionKind::SystemMenu, CaptionChange::Removed),
            ]
        );
        // A wider window moves the buttons, not the icon's cell.
        let wider = CaptionCells::of(
            &CaptionLayout::new(900, 31, 96, 0),
            false,
            false,
            true,
            false,
        );
        assert_eq!(
            caption_changes(&title_bar(false), &wider),
            vec![
                (CaptionKind::Minimize, CaptionChange::Moved),
                (CaptionKind::Maximize, CaptionChange::Moved),
                (CaptionKind::Close, CaptionChange::Moved),
            ]
        );
    }

    #[test]
    fn fr22_the_back_button_comes_and_goes_with_the_arrow() {
        let back = |icon: Option<ClientRect>| CaptionCells {
            icon,
            back: true,
            ..CaptionCells::default()
        };
        let cell = Some((4, 0, 40, 32));
        let added = vec![(CaptionKind::Back, CaptionChange::Added)];
        let removed = vec![(CaptionKind::Back, CaptionChange::Removed)];
        let moved = vec![(CaptionKind::Back, CaptionChange::Moved)];
        assert_eq!(caption_changes(&back(None), &back(cell)), added);
        assert_eq!(caption_changes(&back(cell), &back(None)), removed);
        assert_eq!(
            caption_changes(&back(cell), &back(Some((6, 0, 50, 40)))),
            moved
        );
        assert!(caption_changes(&back(cell), &back(cell)).is_empty());
        assert!(caption_changes(&back(None), &back(None)).is_empty());
    }

    #[test]
    fn fr22_the_back_buttons_runtime_id_cannot_equal_a_nodes() {
        let ids: HashSet<[i32; 2]> = CaptionKind::ALL
            .into_iter()
            .map(|kind| kind.runtime_id())
            .collect();
        // Each button has its own, and none is as long as a node's.
        assert_eq!(ids.len(), CaptionKind::ALL.len());
        for id in ids {
            assert_ne!(id.len(), runtime_id(0, 0).len());
        }
    }

    /// Connects to the in-memory local API as one user.
    struct AsUser(dsa_transport::local::MemoryLocalConnector);

    impl Connector for AsUser {
        fn connect(
            &mut self,
            _: Duration,
        ) -> Result<Box<dyn dsa_transport::Connection>, dsa_transport::TransportError> {
            self.0.connect_as(dsa_transport::local::Peer {
                user: "test-user".into(),
                administrator: false,
            })
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn nfr5_a_window_taken_over_after_a_gpu_reset_watches_and_acts_on_the_new_tasks_tree() {
        use dsa_host_core::compositor::window_hosts::ToHost;
        use dsa_host_core::host_api::{UserAndroid, UserAndroids};
        use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
        let (listener, connector) = dsa_transport::local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let android = {
            let _ = androids.android("test-user");
            androids.of("test-user").expect("the user's Android")
        };
        android.start();
        let _service = RunningService::start(Box::new(listener), androids.clone());
        let connect: ApiConnect =
            Arc::new(move || Box::new(AsUser(connector.clone())) as Box<dyn Connector>);

        // Window 9 opened for task 9; a client reads it.
        let tasks = WindowTasks::new();
        let (input, _) = mpsc::channel();
        let mut window = AccessibilityWindow::new(9, (400, 300), input, tasks.clone(), None, None);
        let shared = window.shared(0);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (tasks, shared, stop) = (tasks.clone(), Arc::clone(&shared), Arc::clone(&stop));
            let once = Arc::new(AtomicBool::new(false));
            thread::spawn(move || watch(9, tasks, connect, shared, stop, once, None))
        };
        let watches = Arc::clone(&android.accessibility_watches);
        wait_until("task 9's tree to be watched", || {
            watches.lock().unwrap().contains(&(9, true))
        });

        // After a GPU reset task 9 ended, and the app's new task 14 took the window over.
        tasks.follow(&ToHost::ShownTask {
            task_id: 9,
            shown_task_id: 14,
        });
        wait_until("task 14's tree to be watched", || {
            watches.lock().unwrap().contains(&(14, true))
        });
        wait_until("the watch of task 9 to end", || {
            watches.lock().unwrap().contains(&(9, false))
        });

        // An action on the window's tree reaches task 14, not the task that ended.
        shared.lock().queued.push(ActionRequest {
            epoch: 1,
            node_id: 3,
            action: NodeAction::Click,
            request_id: 77,
            ..ActionRequest::default()
        });
        wait_until("the action to reach Android", || {
            !android.accessibility_actions.lock().unwrap().is_empty()
        });
        let done: Vec<(i32, u32)> = android
            .accessibility_actions
            .lock()
            .unwrap()
            .iter()
            .map(|(task_id, action)| (*task_id, action.request_id))
            .collect();
        assert_eq!(done, vec![(14, 77)]);

        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap();
    }

    /// The in-memory local API with one user's running Android, and a way to dial it.
    fn service() -> (
        Arc<dsa_host_core::host_api_conformance::FakeAndroid>,
        ApiConnect,
        dsa_host_core::host_api_conformance::RunningService,
    ) {
        use dsa_host_core::host_api::{UserAndroid, UserAndroids};
        use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
        let (listener, connector) = dsa_transport::local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let android = {
            let _ = androids.android("test-user");
            androids.of("test-user").expect("the user's Android")
        };
        android.start();
        let service = RunningService::start(Box::new(listener), androids.clone());
        let connect: ApiConnect =
            Arc::new(move || Box::new(AsUser(connector.clone())) as Box<dyn Connector>);
        (android, connect, service)
    }

    #[test]
    fn fr18_a_uia_client_that_is_not_a_screen_reader_does_not_start_the_watch() {
        let (android, connect, _service) = service();
        let (input, _) = mpsc::channel();
        let mut window = AccessibilityWindow::new(
            9,
            (400, 300),
            input,
            WindowTasks::new(),
            Some(connect),
            None,
        );
        let shared = window.shared(0);
        // A client that is no screen reader reads the window: the tree is read once.
        let started = window
            .demand
            .asked(Instant::now(), false, window.watch.is_some());
        assert_eq!(started, Some((Demand::Snapshot, DemandReason::Read)));
        window.start_watch(&shared, Demand::Snapshot, DemandReason::Read);
        let watches = Arc::clone(&android.accessibility_watches);
        wait_until("task 9's tree to be read", || {
            watches.lock().unwrap().contains(&(9, true))
        });
        // The tree comes; the watch ends by itself and Android stops reading the task.
        android.publish_accessibility(
            9,
            GuestAccessibility::Tree(dsa_host_core::accessibility::TreeUpdate {
                task_id: 9,
                epoch: 1,
                full: true,
                roots: vec![1],
                nodes: vec![Node {
                    id: 1,
                    class_name: "android.widget.FrameLayout".into(),
                    ..Node::default()
                }],
                ..Default::default()
            }),
        );
        wait_until("the read to end", || {
            watches.lock().unwrap().contains(&(9, false))
        });
        wait_until("the watch thread to finish", || {
            window
                .watch
                .as_ref()
                .is_some_and(|watch| watch.thread.is_finished())
        });
        window.tick(0);
        assert!(window.watch.is_none());
        assert!(!window.demand.continuous());
        // The tree it read stays for the client.
        assert_eq!(shared.lock().tree.roots(), &[1]);
        // Another read soon after reads nothing more.
        assert_eq!(window.demand.asked(Instant::now(), false, false), None);
        // It never said a screen reader listens.
        assert!(android.accessibility_listening.lock().unwrap().is_empty());
    }

    #[test]
    fn fr18_the_focus_event_is_raised_before_the_rest_of_its_batch() {
        let moved = Changes {
            focus: Some(4),
            changed: vec![Node::default()],
            children_changed: vec![1],
            ..Changes::default()
        };
        assert_eq!(
            raise_order(&moved, true),
            vec![RaiseStep::Focus, RaiseStep::Rest]
        );
        // A new tree's structure first, so the focused node is there when a client asks.
        let replaced = Changes {
            replaced: true,
            focus: Some(4),
            ..Changes::default()
        };
        assert_eq!(
            raise_order(&replaced, true),
            vec![RaiseStep::Structure, RaiseStep::Focus, RaiseStep::Rest]
        );
        // A window in the background takes no focus.
        assert_eq!(raise_order(&moved, false), vec![RaiseStep::Rest]);
    }

    #[test]
    fn fr18_a_screen_reader_starts_the_fast_watch() {
        let (android, connect, _service) = service();
        let (input, _) = mpsc::channel();
        let mut window = AccessibilityWindow::new(
            9,
            (400, 300),
            input,
            WindowTasks::new(),
            Some(connect),
            None,
        );
        let shared = window.shared(0);
        // A client subscribed to focus changes: an assistive technology.
        shared.focus_listeners.store(1, Ordering::Relaxed);
        assert!(assistive(&shared));
        let started = window.demand.asked(Instant::now(), true, false);
        assert_eq!(
            started,
            Some((Demand::Continuous, DemandReason::AssistiveTechnology))
        );
        window.start_watch(
            &shared,
            Demand::Continuous,
            DemandReason::AssistiveTechnology,
        );
        // It listens: the watch tells Android to send the focus at once.
        shared.fast.store(true, Ordering::Relaxed);
        let listening = Arc::clone(&android.accessibility_listening);
        wait_until("Android to hear a screen reader listens", || {
            listening.lock().unwrap().contains(&(9, true))
        });
        // It goes: the watch ends.
        shared.focus_listeners.store(0, Ordering::Relaxed);
        let stop = window.demand.poll(Instant::now(), true, assistive(&shared));
        if assistive(&shared) {
            // This machine runs a screen reader of its own; the watch rightly stays.
            assert_eq!(stop, None);
        } else {
            assert_eq!(
                stop,
                Some((PresenceChange::Stop, DemandReason::AssistiveTechnologyGone))
            );
        }
        window.stop_watch(None);
        let watches = Arc::clone(&android.accessibility_watches);
        wait_until("the watch to end", || {
            watches.lock().unwrap().contains(&(9, false))
        });
    }

    #[test]
    fn fr18_a_client_finds_and_realizes_the_items_a_virtualized_list_keeps_off_the_screen() {
        use dsa_host_core::accessibility::{Collection, CollectionItem, TreeUpdate};
        let (input, _) = mpsc::channel();
        let mut window =
            AccessibilityWindow::new(9, (400, 300), input, WindowTasks::new(), None, None);
        let shared = window.shared(0);
        // A list of 40 rows of which rows 0 to 2 are on the screen.
        let item = |id: u32, row: i32| Node {
            id,
            class_name: "android.widget.TextView".into(),
            text: format!("row {row}"),
            collection_item: Some(CollectionItem {
                row,
                row_span: 1,
                column: 0,
                column_span: 1,
                heading: false,
            }),
            ..Node::default()
        };
        let list = Node {
            id: 1,
            children: vec![2, 3, 4],
            class_name: "androidx.recyclerview.widget.RecyclerView".into(),
            collection: Some(Collection {
                rows: 40,
                columns: 1,
                hierarchical: false,
            }),
            actions: vec![NodeAction::ScrollToPosition],
            ..Node::default()
        };
        shared
            .lock()
            .tree
            .apply(TreeUpdate {
                task_id: 9,
                epoch: 1,
                full: true,
                roots: vec![1],
                nodes: vec![list, item(2, 0), item(3, 1), item(4, 2)],
                ..TreeUpdate::default()
            })
            .unwrap();
        let container: IItemContainerProvider = node_of(&shared, 1, 1).cast().unwrap();
        let nothing = VARIANT::default();
        // SAFETY: calls into this window's own providers.
        unsafe {
            let first = container
                .FindItemByProperty(None, UIA_PROPERTY_ID(0), &nothing)
                .unwrap();
            assert_eq!(
                patterns::runtime_for_test(&first),
                Some(vec![UiaAppendRuntimeId as i32, 1, 2])
            );
            let third = node_of(&shared, 1, 4)
                .cast::<IRawElementProviderSimple>()
                .unwrap();
            // After the last row on the screen comes row 3, which Android has not made.
            let fourth = container
                .FindItemByProperty(&third, UIA_PROPERTY_ID(0), &nothing)
                .unwrap();
            let virtualized: IVirtualizedItemProvider = fourth.cast().unwrap();
            virtualized.Realize().unwrap();
            // A name finds a row on the screen.
            let name = variant_str("row 1");
            let found = container
                .FindItemByProperty(None, UIA_NamePropertyId, &name)
                .unwrap();
            assert_eq!(
                patterns::runtime_for_test(&found),
                Some(vec![UiaAppendRuntimeId as i32, 1, 3])
            );
        }
        // Realizing scrolled the list to row 3.
        let queued = shared.lock().queued.clone();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].action, NodeAction::ScrollToPosition);
        assert_eq!(queued[0].node_id, 1);
        assert_eq!(queued[0].position, (3, 0));
    }

    #[test]
    fn fr18_before_the_first_tree_the_window_says_it_is_loading() {
        assert_eq!(
            root_children_of(&[CaptionKind::Back], &[], true),
            vec![RootChild::Caption(CaptionKind::Back), RootChild::Loading]
        );
        assert_eq!(root_children_of(&[], &[4], false), vec![RootChild::Node(4)]);
        assert_eq!(loading_name(KO), "불러오는 중");
        assert_eq!(loading_name(EN), "Loading");
        for kind in CaptionKind::ALL {
            assert_ne!(LOADING_RUNTIME_ID, kind.runtime_id());
        }
        assert_ne!(LOADING_RUNTIME_ID.len(), runtime_id(0, 0).len());
    }
}

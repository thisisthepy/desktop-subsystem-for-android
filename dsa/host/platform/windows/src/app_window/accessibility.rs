//! Each task window's UI Automation tree: the Android task's accessibility nodes, as Narrator,
//! Accessibility Insights and every other UI Automation client read them.
//!
//! A client that reads a window sends it `WM_GETOBJECT`. The window answers with a fragment
//! root ([`RootProvider`]) whose children are the task's nodes ([`NodeProvider`]), and that
//! first request starts a thread that watches the task's tree through the service's local
//! API ([`HostApiClient::watch_accessibility`]): the service asks the agent in Android to read
//! the task's nodes, and their tree and its changes come back here. Until a client asks,
//! nothing runs and Android reads nothing. Once no client has asked for [`PRESENCE_GRACE`]
//! and none listens for events (`UiaClientsAreListening`), the thread ends and Android stops
//! reading the task ([`ClientPresence`]).
//!
//! Each node gets the control type of its role, its name, its bounds on the screen (the
//! window's client area, where the task is shown, plus the node's place in the task, scaled
//! when the frame is shown stretched), and the patterns it supports: Invoke, Value, Toggle,
//! SelectionItem and Scroll ([`crate::ui_automation`]). A pattern call becomes the node's
//! accessibility action in Android, so Invoke presses a list row without moving the cursor.
//! As changes arrive the window raises structure, property and focus change events.
//!
//! A tree that breaks the rules is dropped and logged, and the watch starts over; nothing a
//! tree holds can end the window host.
//!
//! [`PRESENCE_GRACE`]: dsa_host_core::accessibility::PRESENCE_GRACE

use std::collections::HashSet;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_host_core::accessibility::{
    from_screen, to_screen, ActionRequest, Changes, ClientPresence, GuestAccessibility, Node,
    NodeAction, PresenceChange, Tree, TreeError,
};
use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;
use windows::core::{
    implement, Error, IUnknown, IUnknownImpl, Interface, BOOL, BSTR, HRESULT, PCWSTR,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, VARIANT_BOOL, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_EMPTY, VT_I4};
use windows::Win32::UI::Accessibility::*;

use crate::compositor::log::{note, CompositorLogger};
use crate::ui_automation::{
    control_type_of, is_content_element, is_control_element, patterns_of, scroll_action, Pattern,
};

/// Dials the service's local API, once for each watch.
pub type ApiConnect = Arc<dyn Fn() -> Box<dyn Connector> + Send + Sync>;

/// The build the window host names itself by when it watches a tree.
const BUILD: &str = "dsa-app-window accessibility";

/// How long the watch waits for the service, and between two tries after it failed.
const CONNECT: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch thread waits for news before it looks at its actions again.
const POLL: Duration = Duration::from_millis(20);

/// The most property change events one batch of changes raises; past it the window says its
/// whole tree changed instead, which costs a client one read.
const MAX_PROPERTY_EVENTS: usize = 64;

/// The window message that tells a task window to raise the events of the changes that came.
pub const WM_APP_ACCESSIBILITY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;

/// What the window's providers share with its watch thread.
struct State {
    tree: Tree,
    /// The task's size in Android, which node bounds are measured in.
    task_size: (u32, u32),
    /// Where actions go while the watch runs.
    actions: Option<Sender<ActionRequest>>,
    next_request: u32,
    /// Changes waiting for the UI thread to raise their events, each with the tree's names
    /// from before it.
    pending: Vec<(Changes, Vec<(u32, String)>)>,
}

/// The state a window's providers read, behind one lock.
pub struct Shared {
    hwnd: isize,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
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

    /// Asks Android to do `action` on node `id` of `epoch`. An error when nothing watches
    /// the tree, or the node is gone.
    fn perform(
        &self,
        epoch: u32,
        id: u32,
        action: NodeAction,
        text: String,
    ) -> windows::core::Result<()> {
        let mut state = self.lock();
        if state.tree.epoch() != Some(epoch) || state.tree.node(id).is_none() {
            return Err(not_available());
        }
        state.next_request = state.next_request.wrapping_add(1);
        let request = ActionRequest {
            epoch,
            node_id: id,
            action,
            text,
            request_id: state.next_request,
        };
        match &state.actions {
            Some(actions) if actions.send(request).is_ok() => Ok(()),
            _ => Err(not_available()),
        }
    }
}

fn not_available() -> Error {
    Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

fn invalid_operation() -> Error {
    Error::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32))
}

/// No element: S_OK with a null pointer, as UI Automation expects for "nothing there".
fn none<T>() -> windows::core::Result<T> {
    Err(Error::empty())
}

fn variant_i4(value: i32) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: writing the tag and the matching member of a fresh VARIANT.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = value;
    }
    variant
}

fn variant_bool(value: bool) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if value { -1 } else { 0 });
    }
    variant
}

fn variant_str(value: &str) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: as above; the caller owns the string with the VARIANT.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BSTR;
        inner.Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(value));
    }
    variant
}

fn variant_empty() -> VARIANT {
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

fn safe_array(values: &[i32]) -> windows::core::Result<*mut SAFEARRAY> {
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
    IRawElementProviderFragmentRoot
)]
struct RootProvider {
    shared: Arc<Shared>,
}

/// One node of the task.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    IValueProvider,
    IToggleProvider,
    ISelectionItemProvider,
    IScrollProvider
)]
struct NodeProvider {
    shared: Arc<Shared>,
    epoch: u32,
    id: u32,
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
        let state = self.shared.lock();
        let Some(epoch) = state.tree.epoch() else {
            return none();
        };
        let id = match direction {
            NavigateDirection_FirstChild => state.tree.roots().first().copied(),
            NavigateDirection_LastChild => state.tree.roots().last().copied(),
            _ => None,
        };
        match id {
            Some(id) => Ok(node_of(&self.shared, epoch, id)),
            None => none(),
        }
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
        let (origin, client) = self.shared.client();
        let state = self.shared.lock();
        let Some(epoch) = state.tree.epoch() else {
            return none();
        };
        let (tx, ty) = from_screen((x, y), state.task_size, client, origin);
        match state.tree.hit(tx, ty) {
            Some(id) => Ok(node_of(&self.shared, epoch, id)),
            None => none(),
        }
    }

    fn GetFocus(&self) -> windows::core::Result<IRawElementProviderFragment> {
        let state = self.shared.lock();
        match (state.tree.epoch(), state.tree.focused()) {
            (Some(epoch), Some(id)) => Ok(node_of(&self.shared, epoch, id)),
            _ => none(),
        }
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
        let supported = self.with_node(|_, node| node.supports(action))?;
        if !supported {
            return Err(invalid_operation());
        }
        self.shared.perform(self.epoch, self.id, action, text)
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
                UIA_ControlTypePropertyId => variant_i4(control_type_of(node.role())),
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
                UIA_HasKeyboardFocusPropertyId => variant_bool(node.states.focused),
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
        let target = self.with_node(|tree, node| match direction {
            NavigateDirection_Parent => {
                Some(tree.parent(node.id).map_or(Target::Root, Target::Node))
            }
            NavigateDirection_NextSibling => tree.sibling(node.id, true).map(Target::Node),
            NavigateDirection_PreviousSibling => tree.sibling(node.id, false).map(Target::Node),
            NavigateDirection_FirstChild => node.children.first().copied().map(Target::Node),
            NavigateDirection_LastChild => node.children.last().copied().map(Target::Node),
            _ => None,
        })?;
        match target {
            Some(Target::Root) => Ok(root_of(&self.shared).cast()?),
            Some(Target::Node(id)) => Ok(node_of(&self.shared, self.epoch, id)),
            None => none(),
        }
    }

    fn GetRuntimeId(&self) -> windows::core::Result<*mut SAFEARRAY> {
        safe_array(&runtime_id(self.epoch, self.id))
    }

    fn BoundingRectangle(&self) -> windows::core::Result<UiaRect> {
        let (origin, client) = self.shared.client();
        let task_size = self.shared.lock().task_size;
        let bounds = self.with_node(|_, node| node.bounds)?;
        let rect = to_screen(bounds, task_size, client, origin);
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
    Node(u32),
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
        self.Select()
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

    fn SetScrollPercent(&self, _: f64, _: f64) -> windows::core::Result<()> {
        Err(invalid_operation())
    }

    fn HorizontalScrollPercent(&self) -> windows::core::Result<f64> {
        Ok(UIA_ScrollPatternNoScroll)
    }

    fn VerticalScrollPercent(&self) -> windows::core::Result<f64> {
        Ok(UIA_ScrollPatternNoScroll)
    }

    fn HorizontalViewSize(&self) -> windows::core::Result<f64> {
        Ok(100.0)
    }

    fn VerticalViewSize(&self) -> windows::core::Result<f64> {
        Ok(100.0)
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
    thread: JoinHandle<()>,
}

/// A task window's UI Automation tree, kept by the window on its UI thread.
pub struct AccessibilityWindow {
    task_id: i32,
    shared: Option<Arc<Shared>>,
    root: Option<IRawElementProviderSimple>,
    presence: ClientPresence,
    watch: Option<Watch>,
    connect: Option<ApiConnect>,
    logger: Option<Arc<CompositorLogger>>,
    task_size: (u32, u32),
}

impl AccessibilityWindow {
    /// A window's tree, which watches nothing until a client asks for it. Without `connect`
    /// the window answers clients with an empty tree.
    pub fn new(
        task_id: i32,
        task_size: (u32, u32),
        connect: Option<ApiConnect>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> AccessibilityWindow {
        AccessibilityWindow {
            task_id,
            shared: None,
            root: None,
            presence: ClientPresence::default(),
            watch: None,
            connect,
            logger,
            task_size,
        }
    }

    /// The task's size in Android changed.
    pub fn task_resized(&mut self, task_size: (u32, u32)) {
        self.task_size = task_size;
        if let Some(shared) = &self.shared {
            shared.lock().task_size = task_size;
        }
    }

    fn shared(&mut self, hwnd: isize) -> Arc<Shared> {
        if let Some(shared) = &self.shared {
            return Arc::clone(shared);
        }
        let task_size = self.task_size;
        let shared = Arc::new(Shared {
            hwnd,
            state: Mutex::new(State {
                tree: Tree::new(),
                task_size,
                actions: None,
                next_request: 0,
                pending: Vec::new(),
            }),
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
        if self.presence.asked(Instant::now()) == Some(PresenceChange::Start) {
            self.start_watch(&shared);
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

    fn start_watch(&mut self, shared: &Arc<Shared>) {
        let Some(connect) = self.connect.clone() else {
            return;
        };
        if self.watch.is_some() {
            return;
        }
        let (tx, actions) = mpsc::channel();
        shared.lock().actions = Some(tx);
        let stop = Arc::new(AtomicBool::new(false));
        let task_id = self.task_id;
        let thread_shared = Arc::clone(shared);
        let thread_stop = Arc::clone(&stop);
        let logger = self.logger.clone();
        note(
            self.logger.as_deref(),
            &format!("task {task_id}: a UI Automation client reads the window; watching its accessibility tree"),
        );
        let spawned = thread::Builder::new()
            .name("dsa-app-window-accessibility".into())
            .spawn(move || {
                watch(
                    task_id,
                    connect,
                    thread_shared,
                    actions,
                    thread_stop,
                    logger,
                )
            });
        match spawned {
            Ok(thread) => self.watch = Some(Watch { stop, thread }),
            Err(why) => {
                note(
                    self.logger.as_deref(),
                    &format!("could not watch the accessibility tree: {why}"),
                );
                shared.lock().actions = None;
                self.presence.end();
            }
        }
    }

    fn stop_watch(&mut self) {
        if let Some(watch) = self.watch.take() {
            watch.stop.store(true, Ordering::Relaxed);
            let _ = watch.thread.join();
            note(
                self.logger.as_deref(),
                &format!("task {}: no UI Automation client reads the window; stopped watching its accessibility tree", self.task_id),
            );
        }
        if let Some(shared) = &self.shared {
            let mut state = shared.lock();
            state.actions = None;
            let names = names_of(&state.tree);
            let changes = state.tree.clear();
            state.pending.push((changes, names));
        }
    }

    /// Checks, every so often on the UI thread, whether clients have gone.
    pub fn tick(&mut self, hwnd: isize) {
        // SAFETY: a plain query.
        let listening = unsafe { UiaClientsAreListening() }.as_bool();
        if self.presence.poll(Instant::now(), listening) == Some(PresenceChange::Stop) {
            self.stop_watch();
            post_events(hwnd);
        }
        // A watch that ended by itself (the service went away for good) lets a later
        // request start a new one.
        if self
            .watch
            .as_ref()
            .is_some_and(|watch| watch.thread.is_finished())
        {
            self.stop_watch();
            self.presence.end();
        }
    }

    /// Where the window raises its events, once a client has asked for its tree.
    pub fn events(&self) -> Option<EventTarget> {
        Some(EventTarget {
            shared: self.shared.clone()?,
            root: self.root.clone()?,
        })
    }
}

/// Where a window raises its events, taken from its tree so no borrow of it is held while UI
/// Automation calls back into the window.
pub struct EventTarget {
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
        let (pending, epoch) = {
            let mut state = shared.lock();
            (std::mem::take(&mut state.pending), state.tree.epoch())
        };
        // SAFETY: a plain query.
        if pending.is_empty() || !unsafe { UiaClientsAreListening() }.as_bool() {
            return;
        }
        let root = self.root.clone();
        let foreground = {
            use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
            // SAFETY: a plain query.
            unsafe { GetForegroundWindow() as isize == hwnd }
        };
        for (changes, before) in pending {
            raise(&shared, &root, epoch, &changes, &before, foreground);
        }
    }
}

impl AccessibilityWindow {
    /// The window is closing: stops the watch and lets clients go.
    pub fn close(&mut self, hwnd: isize) {
        self.stop_watch();
        self.presence.end();
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

/// Raises the events of one batch of changes.
fn raise(
    shared: &Arc<Shared>,
    root: &IRawElementProviderSimple,
    epoch: Option<u32>,
    changes: &Changes,
    before: &[(u32, String)],
    foreground: bool,
) {
    // SAFETY: UI Automation's event calls, with providers of this window.
    unsafe {
        if changes.replaced || changes.roots_changed {
            let _ = UiaRaiseStructureChangedEvent(
                root,
                StructureChangeType_ChildrenInvalidated,
                std::ptr::null_mut(),
                0,
            );
        }
        let Some(epoch) = epoch else {
            return;
        };
        let simple = |id: u32| -> Option<IRawElementProviderSimple> {
            node_of(shared, epoch, id).cast().ok()
        };
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
        if let (Some(focus), true) = (changes.focus, foreground) {
            if let Some(provider) = simple(focus) {
                let _ = UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId);
            }
        }
    }
}

/// Frees a VARIANT this module made.
fn free_variant(mut variant: VARIANT) {
    // SAFETY: a VARIANT made by one of the constructors above, cleared once.
    unsafe {
        let _ = windows::Win32::System::Variant::VariantClear(&mut variant);
    }
}

/// The watch thread: watches the task's tree through the service until told to stop, and
/// starts over when the connection ends or a tree cannot be taken.
fn watch(
    task_id: i32,
    connect: ApiConnect,
    shared: Arc<Shared>,
    actions: Receiver<ActionRequest>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
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
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            loop {
                match actions.try_recv() {
                    Ok(action) => {
                        if stream.perform(&action).is_err() {
                            break;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            match stream.next(POLL) {
                Ok(None) => {}
                Ok(Some(GuestAccessibility::Tree(update))) => {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_automation::control_type;

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
        assert_eq!(ScrollAmount_LargeIncrement.0, 3);
        assert_eq!(ScrollAmount_SmallDecrement.0, 1);
    }
}

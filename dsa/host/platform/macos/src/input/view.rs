//! A task window's content view: an `NSView` subclass that hosts the task's `CAMetalLayer`,
//! takes the mouse, the trackpad, the pen and the keyboard, is the input method's
//! `NSTextInputClient`, and accepts files dropped on it.
//!
//! What the view sees goes through the platform-neutral mappers ([`PointerMapper`],
//! [`KeyMapper`]) and the input method client ([`TextInputClient`]) to the task's
//! [`InputSink`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;

use dsa_host_core::drag::LeaveWatch;
use dsa_host_core::ime::ImeCursor;
use dsa_host_core::input::{
    ContactPhase, InputEvent, MouseButton, PointerMapper, PointerTool, WindowGeometry, WindowInput,
};
use dsa_host_core::keys::{keycode, KeyInput, KeyMapper};
use dsa_host_core::tasks::Bounds;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::sel;

use super::keys::{guest_key, locks, modifier_change, ModifierChange};
use super::pointer::{
    button, client_pixel, pen_pressure, pen_tilt, scroll_notches, SUBTYPE_TABLET_POINT,
};
use super::text_input::TextInputClient;
use crate::compositor::title_bar::{band_hit, double_click_action, BandHit, DoubleClick};
use crate::objc::{class, ns_string, string_of, CGPoint, CGRect, CGSize, NSRange, NOT_FOUND};

/// Where a task window's input goes.
pub trait InputSink {
    /// One event for the guest's task.
    fn input(&mut self, event: InputEvent);
    /// Files were dropped on the window: the guest's app is offered them.
    fn files_dropped(&mut self, paths: Vec<PathBuf>);
    /// A press of the left button left the window at (`x`, `y`) of the task, the last place
    /// inside it: an app may be dragging something out.
    fn drag_left(&mut self, x: f32, y: f32);
    /// Where the cursor of the editor focused in the task is, for the input method's
    /// candidate window; `None` when the guest has not said.
    fn ime_cursor(&self) -> Option<ImeCursor>;
    /// The editor focused in the task as the agent last described it; `None` when it has
    /// not.
    fn ime_editor(&self) -> Option<dsa_host_core::ime::ImeEditor>;
}

/// `NSEventModifierFlagControl` and `NSEventModifierFlagCommand`.
const FLAG_CONTROL: u64 = 1 << 18;
const FLAG_COMMAND: u64 = 1 << 20;
/// `NSDragOperationCopy`, `NSDragOperationNone`.
const DRAG_COPY: usize = 1;
const DRAG_NONE: usize = 0;
/// `NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved | NSTrackingActiveAlways |
/// NSTrackingInVisibleRect`.
const TRACKING: usize = 0x01 | 0x02 | 0x80 | 0x200;

struct ViewState {
    sink: Box<dyn InputSink>,
    pointer: PointerMapper,
    keys: KeyMapper,
    text: TextInputClient,
    /// When a press of the left button leaves the view.
    leave: LeaveWatch,
    /// How many client pixels at the top the title bar band covers: only while the app
    /// draws its own title bar under it.
    band_height: u32,
    /// The app's controls in the band, in client pixels, which take the pointer there.
    controls: Vec<Bounds>,
    /// The last press went to the band (to drag the window, or for its menu), so its release
    /// is not the app's either.
    band_press: bool,
    /// The pointer hovers over the band's drag area, which the app was told it left.
    over_band: bool,
}

thread_local! {
    static VIEWS: RefCell<HashMap<usize, Rc<RefCell<ViewState>>>> = RefCell::new(HashMap::new());
    /// How large each view shows its task's pixels (view pixels per task pixels), for a task
    /// that keeps its density on a display of another scale. Kept apart from the views' state,
    /// which is borrowed while their input is mapped.
    static FRAME_SCALES: RefCell<HashMap<usize, (u32, u32)>> = RefCell::new(HashMap::new());
}

/// The view shows each of its task's pixels at `scale` (view pixels per task pixels) from now
/// on, and maps its pointer input at it.
pub fn set_frame_scale(view: &AnyObject, scale: (u32, u32)) {
    let key = view as *const AnyObject as usize;
    FRAME_SCALES.with(|scales| scales.borrow_mut().insert(key, scale));
}

fn frame_scale_of(view: &AnyObject) -> (u32, u32) {
    let key = view as *const AnyObject as usize;
    FRAME_SCALES.with(|scales| scales.borrow().get(&key).copied().unwrap_or((1, 1)))
}

fn state_of(view: &AnyObject) -> Option<Rc<RefCell<ViewState>>> {
    let key = view as *const AnyObject as usize;
    VIEWS.with(|views| views.borrow().get(&key).cloned())
}

/// Runs `f` on the view's state; a callback that panics never unwinds into AppKit, and one
/// that arrives while the state is in use (AppKit calling back from inside a call the state
/// made) is dropped rather than aliased.
fn with_state<R>(view: &AnyObject, f: impl FnOnce(&mut ViewState) -> R) -> Option<R> {
    let state = state_of(view)?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut state = state.try_borrow_mut().ok()?;
        Some(f(&mut state))
    }))
    .ok()
    .flatten()
}

fn send(state: &mut ViewState, event: impl Into<InputEvent>) {
    state.sink.input(event.into());
}

unsafe fn view_scale(view: &AnyObject) -> f64 {
    let window: *mut AnyObject = msg_send![view, window];
    if window.is_null() {
        1.0
    } else {
        msg_send![window, backingScaleFactor]
    }
}

/// The view's size in client pixels, which the task's frames fill one to one, or at the view's
/// frame scale for a task that keeps its density on a display of another scale.
unsafe fn geometry(view: &AnyObject) -> WindowGeometry {
    let bounds: CGRect = msg_send![view, bounds];
    let scale = view_scale(view);
    let client =
        crate::compositor::sizing::task_pixels((bounds.size.width, bounds.size.height), scale);
    let scale = frame_scale_of(view);
    WindowGeometry {
        client,
        task: dsa_host_core::compositor::window_sizing::to_task_size(client, scale),
        scale,
    }
}

/// Where `event` happened, in client pixels from the top left.
unsafe fn event_position(view: &AnyObject, event: *mut AnyObject) -> (f64, f64) {
    let in_window: CGPoint = msg_send![event, locationInWindow];
    let point: CGPoint =
        msg_send![view, convertPoint: in_window, fromView: std::ptr::null::<AnyObject>()];
    let bounds: CGRect = msg_send![view, bounds];
    client_pixel((point.x, point.y), bounds.size.height, view_scale(view))
}

/// A tablet event's tilt, as AppKit scales it: -1 to 1 on each axis, y up.
unsafe fn event_tilt(event: *mut AnyObject) -> (f64, f64) {
    let tilt: CGPoint = msg_send![event, tilt];
    (tilt.x, tilt.y)
}

unsafe fn modifier_flags(event: *mut AnyObject) -> u64 {
    let flags: usize = msg_send![event, modifierFlags];
    flags as u64
}

/// When `event` happened, in milliseconds of the system's uptime, which wraps.
unsafe fn event_time_ms(event: *mut AnyObject) -> u32 {
    let seconds: f64 = msg_send![event, timestamp];
    ((seconds * 1000.0) as u64 & u64::from(u32::MAX)) as u32
}

unsafe fn is_pen(event: *mut AnyObject) -> bool {
    let event_type: usize = msg_send![event, type];
    // Only the mouse event types carry a subtype; asking a key event would raise.
    if !matches!(event_type, 1..=7 | 25..=27) {
        return false;
    }
    let subtype: i16 = msg_send![event, subtype];
    subtype == SUBTYPE_TABLET_POINT
}

fn pointer(view: &AnyObject, input: WindowInput) {
    let geometry = unsafe { geometry(view) };
    with_state(view, |state| {
        let mapped = state.pointer.handle(input, geometry);
        for event in mapped.events {
            send(state, event);
        }
    });
}

unsafe fn mouse_button(view: &AnyObject, event: *mut AnyObject, down: bool) {
    let position = event_position(view, event);
    if is_pen(event) {
        let pressure: f32 = msg_send![event, pressure];
        pointer(
            view,
            WindowInput::Contact {
                tool: PointerTool::Pen,
                pointer_id: 0,
                phase: if down {
                    ContactPhase::Down
                } else {
                    ContactPhase::Up
                },
                position,
                pressure: pen_pressure(pressure),
                buttons: 0,
                tilt: pen_tilt(event_tilt(event)),
            },
        );
        return;
    }
    let number: isize = msg_send![event, buttonNumber];
    let control = modifier_flags(event) & FLAG_CONTROL != 0;
    // The primary and secondary buttons in the band of an app's own title bar, outside its
    // controls, are the window's, as on a title bar.
    if number == 0 || number == 1 {
        if !down {
            let taken = with_state(view, |state| std::mem::take(&mut state.band_press));
            if taken == Some(true) {
                return;
            }
        } else {
            let hit = with_state(view, |state| {
                let hit = band_hit(position, state.band_height, &state.controls);
                state.band_press = hit == BandHit::Drag;
                hit
            });
            if hit == Some(BandHit::Drag) {
                band_pressed(view, event, number == 1 || control);
                return;
            }
        }
    }
    if let Some(button) = button(number, control) {
        if button == MouseButton::Left {
            with_state(view, |state| state.leave.pressed(down));
        }
        pointer(
            view,
            WindowInput::MouseButton {
                button,
                down,
                position,
            },
        );
    }
}

/// A press on the band's drag area: a secondary click opens the window's menu, a double
/// click does what the user set for a title bar, and any other press drags the window.
unsafe fn band_pressed(view: &AnyObject, event: *mut AnyObject, secondary: bool) {
    let window: *mut AnyObject = msg_send![view, window];
    if window.is_null() {
        return;
    }
    if secondary {
        crate::compositor::menu_bar::pop_up(view, event);
        return;
    }
    let clicks: isize = msg_send![event, clickCount];
    if clicks < 2 {
        let _: () = msg_send![window, performWindowDragWithEvent: event];
        return;
    }
    let defaults: *mut AnyObject = match class(c"NSUserDefaults") {
        Ok(defaults) => msg_send![defaults, standardUserDefaults],
        Err(_) => std::ptr::null_mut(),
    };
    let (action, minimize) = if defaults.is_null() {
        (String::new(), false)
    } else {
        let action: *mut AnyObject =
            msg_send![defaults, stringForKey: &*ns_string("AppleActionOnDoubleClick")];
        let minimize: bool =
            msg_send![defaults, boolForKey: &*ns_string("AppleMiniaturizeOnDoubleClick")];
        (string_of(action), minimize)
    };
    let action = (!action.is_empty()).then_some(action.as_str());
    match double_click_action(action, minimize) {
        DoubleClick::Zoom => {
            let _: () = msg_send![window, performZoom: std::ptr::null::<AnyObject>()];
        }
        DoubleClick::Minimize => {
            let _: () = msg_send![window, performMiniaturize: std::ptr::null::<AnyObject>()];
        }
        DoubleClick::Nothing => {}
    }
}

unsafe extern "C-unwind" fn mouse_down(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    mouse_button(this, event, true);
}

unsafe extern "C-unwind" fn mouse_up(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    mouse_button(this, event, false);
}

unsafe extern "C-unwind" fn mouse_moved(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let position = event_position(this, event);
    if is_pen(event) {
        let pressure: f32 = msg_send![event, pressure];
        let pressed: usize = {
            let class = class(c"NSEvent").expect("AppKit has NSEvent");
            msg_send![class, pressedMouseButtons]
        };
        pointer(
            this,
            WindowInput::Contact {
                tool: PointerTool::Pen,
                pointer_id: 0,
                phase: if pressed & 1 != 0 {
                    ContactPhase::Move
                } else {
                    ContactPhase::Hover
                },
                position,
                pressure: pen_pressure(pressure),
                buttons: 0,
                tilt: pen_tilt(event_tilt(event)),
            },
        );
        return;
    }
    // Hovering over the band's drag area is the title bar's, not the app's: the app is told
    // the pointer left, once, and hears of it again when it comes back. A drag that started
    // in the app stays the app's.
    let pressed: usize = match class(c"NSEvent") {
        Ok(events) => msg_send![events, pressedMouseButtons],
        Err(_) => 0,
    };
    if pressed == 0 {
        let over = with_state(this, |state| {
            let over = band_hit(position, state.band_height, &state.controls) == BandHit::Drag;
            let entered = over && !state.over_band;
            state.over_band = over;
            (over, entered)
        });
        match over {
            Some((true, true)) => {
                pointer(this, WindowInput::MouseLeave);
                return;
            }
            Some((true, false)) => return,
            _ => {}
        }
    }
    pointer(this, WindowInput::MouseMove { position });
    let geometry = geometry(this);
    with_state(this, |state| {
        if let Some(inside) = state.leave.moved(position, geometry.client) {
            let (x, y) = geometry.to_guest(inside);
            state.sink.drag_left(x, y);
        }
    });
}

unsafe extern "C-unwind" fn mouse_exited(this: &AnyObject, _: Sel, _event: *mut AnyObject) {
    pointer(this, WindowInput::MouseLeave);
}

unsafe extern "C-unwind" fn scroll_wheel(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let position = event_position(this, event);
    let dx: f64 = msg_send![event, scrollingDeltaX];
    let dy: f64 = msg_send![event, scrollingDeltaY];
    let precise: bool = msg_send![event, hasPreciseScrollingDeltas];
    let notches = scroll_notches((dx, dy), precise);
    if notches != (0.0, 0.0) {
        pointer(this, WindowInput::Wheel { position, notches });
    }
}

fn key(view: &AnyObject, key_code: u32, down: bool, flags: u64, time_ms: u32) {
    with_state(view, |state| {
        let input = KeyInput {
            key_code,
            down,
            locks: locks(flags),
            time_ms,
        };
        if let Some(event) = state.keys.handle(input) {
            send(state, event);
        }
    });
}

unsafe extern "C-unwind" fn key_down(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let code: u16 = msg_send![event, keyCode];
    let flags = modifier_flags(event);
    let time = event_time_ms(event);
    // A shortcut with Command goes to the guest as keys; the input method has no part in it.
    if flags & FLAG_COMMAND == 0 {
        refresh_editor(this);
        with_state(this, |state| state.text.begin_key());
        let array_class = class(c"NSArray").expect("Foundation has NSArray");
        let events: *mut AnyObject = msg_send![array_class, arrayWithObject: event];
        let _: () = msg_send![this, interpretKeyEvents: events];
        let send_key = with_state(this, |state| state.text.end_key()).unwrap_or(false);
        if !send_key {
            return;
        }
    }
    if let Some(key_code) = guest_key(code) {
        key(this, key_code, true, flags, time);
    }
}

unsafe extern "C-unwind" fn key_up(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let code: u16 = msg_send![event, keyCode];
    if let Some(key_code) = guest_key(code) {
        key(
            this,
            key_code,
            false,
            modifier_flags(event),
            event_time_ms(event),
        );
    }
}

unsafe extern "C-unwind" fn flags_changed(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let code: u16 = msg_send![event, keyCode];
    let flags = modifier_flags(event);
    let time = event_time_ms(event);
    match modifier_change(code, flags) {
        Some(ModifierChange::Key { key_code, down }) => key(this, key_code, down, flags, time),
        Some(ModifierChange::CapsLockPressed) => {
            key(this, keycode::CAPS_LOCK, true, flags, time);
            key(this, keycode::CAPS_LOCK, false, flags, time);
        }
        None => {}
    }
}

unsafe extern "C-unwind" fn resign_first_responder(this: &AnyObject, _: Sel) -> Bool {
    let time = {
        let info_class = class(c"NSProcessInfo").expect("Foundation has NSProcessInfo");
        let info: *mut AnyObject = msg_send![info_class, processInfo];
        let uptime: f64 = msg_send![info, systemUptime];
        ((uptime * 1000.0) as u64 & u64::from(u32::MAX)) as u32
    };
    with_state(this, |state| {
        for event in state.keys.release_all(time) {
            send(state, event);
        }
        if let Some(event) = state.text.focus_lost() {
            send(state, event);
        }
    });
    let context: *mut AnyObject = msg_send![this, inputContext];
    if !context.is_null() {
        let _: () = msg_send![context, discardMarkedText];
    }
    Bool::YES
}

unsafe extern "C-unwind" fn yes(_this: &AnyObject, _: Sel) -> Bool {
    Bool::YES
}

unsafe extern "C-unwind" fn no(_this: &AnyObject, _: Sel) -> Bool {
    Bool::NO
}

unsafe extern "C-unwind" fn accepts_first_mouse(
    _this: &AnyObject,
    _: Sel,
    _event: *mut AnyObject,
) -> Bool {
    Bool::YES
}

// NSTextInputClient.

/// The range AppKit passes, or `None` for `{NSNotFound, 0}`.
fn range_of(range: NSRange) -> Option<(usize, usize)> {
    (range.location != NOT_FOUND).then_some((range.location, range.length))
}

/// Takes the agent's latest description of the editor into the client. Whether the document
/// changed.
fn sync_editor(view: &AnyObject) -> bool {
    with_state(view, |state| match state.sink.ime_editor() {
        Some(editor) => state.text.describe(editor),
        None => false,
    })
    .unwrap_or(false)
}

/// Takes the latest description, and tells the input method that what it knows of where
/// characters stand is out of date when the editor changed.
unsafe fn refresh_editor(view: &AnyObject) {
    if sync_editor(view) {
        let context: *mut AnyObject = msg_send![view, inputContext];
        if !context.is_null() {
            let _: () = msg_send![context, invalidateCharacterCoordinates];
        }
    }
}

unsafe extern "C-unwind" fn has_marked_text(this: &AnyObject, _: Sel) -> Bool {
    Bool::new(with_state(this, |state| state.text.has_marked_text()).unwrap_or(false))
}

unsafe extern "C-unwind" fn marked_range(this: &AnyObject, _: Sel) -> NSRange {
    sync_editor(this);
    match with_state(this, |state| state.text.marked_range()).flatten() {
        Some((location, length)) => NSRange { location, length },
        None => NSRange {
            location: NOT_FOUND,
            length: 0,
        },
    }
}

unsafe extern "C-unwind" fn selected_range(this: &AnyObject, _: Sel) -> NSRange {
    sync_editor(this);
    let (location, length) =
        with_state(this, |state| state.text.selected_range()).unwrap_or((0, 0));
    NSRange { location, length }
}

unsafe extern "C-unwind" fn set_marked_text(
    this: &AnyObject,
    _: Sel,
    text: *mut AnyObject,
    selected: NSRange,
    replacement: NSRange,
) {
    let text = string_of(text);
    with_state(this, |state| {
        let events = state.text.set_marked_text_in(
            &text,
            (selected.location, selected.length),
            range_of(replacement),
        );
        for event in events {
            send(state, event);
        }
    });
}

unsafe extern "C-unwind" fn unmark_text(this: &AnyObject, _: Sel) {
    with_state(this, |state| {
        if let Some(event) = state.text.unmark_text() {
            send(state, event);
        }
    });
}

unsafe extern "C-unwind" fn valid_attributes(_this: &AnyObject, _: Sel) -> *mut AnyObject {
    let array_class = class(c"NSArray").expect("Foundation has NSArray");
    msg_send![array_class, array]
}

unsafe extern "C-unwind" fn attributed_substring(
    this: &AnyObject,
    _: Sel,
    range: NSRange,
    actual: *mut NSRange,
) -> *mut AnyObject {
    sync_editor(this);
    // The guest's editor around its selection, as the agent described it.
    let Some((text, (location, length))) = with_state(this, |state| {
        state.text.substring((range.location, range.length))
    })
    .flatten() else {
        return std::ptr::null_mut();
    };
    if !actual.is_null() {
        *actual = NSRange { location, length };
    }
    let Ok(class) = class(c"NSAttributedString") else {
        return std::ptr::null_mut();
    };
    let string = ns_string(&text);
    let allocated: Allocated<AnyObject> = msg_send![class, alloc];
    let made: Option<Retained<AnyObject>> = msg_send![allocated, initWithString: &*string];
    // AppKit takes an autoreleased object, which it does not own.
    made.map_or(std::ptr::null_mut(), Retained::autorelease_return)
}

unsafe extern "C-unwind" fn insert_text(
    this: &AnyObject,
    _: Sel,
    text: *mut AnyObject,
    replacement: NSRange,
) {
    let text = string_of(text);
    with_state(this, |state| {
        for event in state.text.insert_text_in(&text, range_of(replacement)) {
            send(state, event);
        }
    });
}

unsafe extern "C-unwind" fn character_index(_this: &AnyObject, _: Sel, _point: CGPoint) -> usize {
    NOT_FOUND
}

unsafe extern "C-unwind" fn first_rect(
    this: &AnyObject,
    _: Sel,
    range: NSRange,
    actual: *mut NSRange,
) -> CGRect {
    // Every range is placed at the guest's cursor, which follows the composition.
    let (cursor, covered) = with_state(this, |state| {
        (
            state.sink.ime_cursor(),
            state.text.rect_range((range.location, range.length)),
        )
    })
    .unwrap_or((None, (range.location, range.length)));
    if !actual.is_null() {
        *actual = NSRange {
            location: covered.0,
            length: covered.1,
        };
    }
    let scale = view_scale(this);
    let bounds: CGRect = msg_send![this, bounds];
    // The editor's cursor in client pixels from the top left (the window manager gives it at
    // the view's frame scale), as view points from the bottom left; with no cursor known, the
    // view's bottom left.
    let rect = match cursor {
        Some(c) => CGRect {
            origin: CGPoint {
                x: f64::from(c.left) / scale,
                y: bounds.size.height - f64::from(c.bottom) / scale,
            },
            size: CGSize {
                width: (f64::from(c.right - c.left) / scale).max(1.0),
                height: (f64::from(c.bottom - c.top) / scale).max(1.0),
            },
        },
        None => CGRect {
            origin: CGPoint::default(),
            size: CGSize {
                width: 1.0,
                height: 1.0,
            },
        },
    };
    let in_window: CGRect =
        msg_send![this, convertRect: rect, toView: std::ptr::null::<AnyObject>()];
    let window: *mut AnyObject = msg_send![this, window];
    if window.is_null() {
        return in_window;
    }
    msg_send![window, convertRectToScreen: in_window]
}

unsafe extern "C-unwind" fn do_command(_this: &AnyObject, _: Sel, _command: Sel) {
    // A command (Return, an arrow, delete) is not text: the key goes to the guest as a key,
    // because the key-down handler sees nothing was typed.
}

// Drag and drop.

unsafe fn dropped_paths(info: *mut AnyObject) -> Vec<PathBuf> {
    let pasteboard: *mut AnyObject = msg_send![info, draggingPasteboard];
    if pasteboard.is_null() {
        return Vec::new();
    }
    let (Ok(url_class), Ok(array_class)) = (class(c"NSURL"), class(c"NSArray")) else {
        return Vec::new();
    };
    let classes: *mut AnyObject = msg_send![array_class, arrayWithObject: url_class];
    let urls: *mut AnyObject = msg_send![
        pasteboard,
        readObjectsForClasses: classes,
        options: std::ptr::null::<AnyObject>()
    ];
    if urls.is_null() {
        return Vec::new();
    }
    let count: usize = msg_send![urls, count];
    (0..count)
        .filter_map(|i| {
            let url: *mut AnyObject = msg_send![urls, objectAtIndex: i];
            let is_file: bool = msg_send![url, isFileURL];
            if !is_file {
                return None;
            }
            let path: *mut AnyObject = msg_send![url, path];
            Some(PathBuf::from(string_of(path)))
        })
        .collect()
}

unsafe extern "C-unwind" fn dragging_entered(
    _this: &AnyObject,
    _: Sel,
    info: *mut AnyObject,
) -> usize {
    if dropped_paths(info).is_empty() {
        DRAG_NONE
    } else {
        DRAG_COPY
    }
}

unsafe extern "C-unwind" fn perform_drag(this: &AnyObject, _: Sel, info: *mut AnyObject) -> Bool {
    let paths = dropped_paths(info);
    if paths.is_empty() {
        return Bool::NO;
    }
    with_state(this, |state| state.sink.files_dropped(paths));
    Bool::YES
}

fn view_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            type Event<'a> = unsafe extern "C-unwind" fn(&'a AnyObject, Sel, *mut AnyObject);
            let superclass = class(c"NSView")?;
            let mut b = ClassBuilder::new(c"DSATaskView", superclass)
                .ok_or("the task view class is already defined")?;
            for selector in [
                sel!(mouseDown:),
                sel!(rightMouseDown:),
                sel!(otherMouseDown:),
            ] {
                b.add_method(selector, mouse_down as Event<'_>);
            }
            for selector in [sel!(mouseUp:), sel!(rightMouseUp:), sel!(otherMouseUp:)] {
                b.add_method(selector, mouse_up as Event<'_>);
            }
            for selector in [
                sel!(mouseMoved:),
                sel!(mouseDragged:),
                sel!(rightMouseDragged:),
                sel!(otherMouseDragged:),
            ] {
                b.add_method(selector, mouse_moved as Event<'_>);
            }
            b.add_method(sel!(mouseExited:), mouse_exited as Event<'_>);
            b.add_method(sel!(scrollWheel:), scroll_wheel as Event<'_>);
            b.add_method(sel!(keyDown:), key_down as Event<'_>);
            b.add_method(sel!(keyUp:), key_up as Event<'_>);
            b.add_method(sel!(flagsChanged:), flags_changed as Event<'_>);
            b.add_method(
                sel!(acceptsFirstResponder),
                yes as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            // The view decides itself which presses drag the window (the band outside an
            // app's controls), so AppKit never moves the window on a press of its own.
            b.add_method(
                sel!(mouseDownCanMoveWindow),
                no as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(acceptsFirstMouse:),
                accepts_first_mouse as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            b.add_method(
                sel!(resignFirstResponder),
                resign_first_responder as unsafe extern "C-unwind" fn(_, _) -> _,
            );

            b.add_method(
                sel!(hasMarkedText),
                has_marked_text as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(markedRange),
                marked_range as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(selectedRange),
                selected_range as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(setMarkedText:selectedRange:replacementRange:),
                set_marked_text as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            b.add_method(
                sel!(unmarkText),
                unmark_text as unsafe extern "C-unwind" fn(_, _),
            );
            b.add_method(
                sel!(validAttributesForMarkedText),
                valid_attributes as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(attributedSubstringForProposedRange:actualRange:),
                attributed_substring as unsafe extern "C-unwind" fn(_, _, _, _) -> _,
            );
            b.add_method(
                sel!(insertText:replacementRange:),
                insert_text as unsafe extern "C-unwind" fn(_, _, _, _),
            );
            b.add_method(
                sel!(characterIndexForPoint:),
                character_index as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            b.add_method(
                sel!(firstRectForCharacterRange:actualRange:),
                first_rect as unsafe extern "C-unwind" fn(_, _, _, _) -> _,
            );
            b.add_method(
                sel!(doCommandBySelector:),
                do_command as unsafe extern "C-unwind" fn(_, _, _),
            );
            if let Some(protocol) = AnyProtocol::get(c"NSTextInputClient") {
                b.add_protocol(protocol);
            }

            b.add_method(
                sel!(draggingEntered:),
                dragging_entered as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            b.add_method(
                sel!(draggingUpdated:),
                dragging_entered as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            b.add_method(
                sel!(performDragOperation:),
                perform_drag as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            // The source of an app's drag out of the window.
            b.add_method(
                sel!(draggingSession:sourceOperationMaskForDraggingContext:),
                super::drag_source::source_operations
                    as unsafe extern "C-unwind" fn(_, _, _, _) -> _,
            );
            b.add_method(
                sel!(draggingSession:endedAtPoint:operation:),
                super::drag_source::session_ended as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            if let Some(protocol) = AnyProtocol::get(c"NSDraggingSource") {
                b.add_protocol(protocol);
            }
            Ok(b.register())
        })
        .clone()
}

/// A view of `frame` for task `task_id`, hosting `layer` and sending its input to `sink`.
pub fn make(
    task_id: i32,
    frame: CGRect,
    layer: &AnyObject,
    sink: Box<dyn InputSink>,
) -> Result<Retained<AnyObject>, String> {
    let view_class = view_class()?;
    unsafe {
        let allocated: Allocated<AnyObject> = msg_send![view_class, alloc];
        let view: Option<Retained<AnyObject>> = msg_send![allocated, initWithFrame: frame];
        let view = view.ok_or("AppKit could not make the task view")?;
        // A layer-hosting view: the layer is set before the view wants one.
        let _: () = msg_send![&*view, setLayer: layer];
        let _: () = msg_send![&*view, setWantsLayer: true];

        let tracking_class = class(c"NSTrackingArea")?;
        let tracking: Allocated<AnyObject> = msg_send![tracking_class, alloc];
        let tracking: Option<Retained<AnyObject>> = msg_send![
            tracking,
            initWithRect: CGRect::default(),
            options: TRACKING,
            owner: &*view,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        if let Some(tracking) = tracking {
            let _: () = msg_send![&*view, addTrackingArea: &*tracking];
        }

        let array_class = class(c"NSArray")?;
        let file_url = ns_string("public.file-url");
        let types: *mut AnyObject = msg_send![array_class, arrayWithObject: &*file_url];
        let _: () = msg_send![&*view, registerForDraggedTypes: types];

        let state = ViewState {
            sink,
            pointer: PointerMapper::new(task_id),
            keys: KeyMapper::new(task_id),
            text: TextInputClient::new(task_id),
            leave: LeaveWatch::default(),
            band_height: 0,
            controls: Vec::new(),
            band_press: false,
            over_band: false,
        };
        VIEWS.with(|views| {
            views.borrow_mut().insert(
                Retained::as_ptr(&view) as usize,
                Rc::new(RefCell::new(state)),
            )
        });
        Ok(view)
    }
}

/// Sets the task the view's input goes to. A window is made before the guest names its task's
/// id in some flows, and a task can move to a new id when it is recreated.
pub fn set_task(view: &AnyObject, task_id: i32) {
    with_state(view, |state| {
        state.pointer = PointerMapper::new(task_id);
        state.keys = KeyMapper::new(task_id);
        state.text = TextInputClient::new(task_id);
    });
}

/// The title bar band covers the top `height` client pixels of the view: presses there
/// outside the app's controls are the window's. 0 with a standard title bar.
pub fn set_band_height(view: &AnyObject, height: u32) {
    with_state(view, |state| {
        state.band_height = height;
        if height == 0 {
            state.over_band = false;
        }
    });
}

/// Where the app's controls are in the band, in client pixels; they take the pointer there.
pub fn set_controls(view: &AnyObject, controls: Vec<Bounds>) {
    with_state(view, |state| state.controls = controls);
}

/// Drops the view's state, when its window goes.
pub fn forget(view: &AnyObject) {
    let key = view as *const AnyObject as usize;
    VIEWS.with(|views| views.borrow_mut().remove(&key));
    FRAME_SCALES.with(|scales| scales.borrow_mut().remove(&key));
}

/// Asks the input method to drop a composition the guest has ended on its own (the editor
/// lost focus in the guest).
pub fn discard_composition(view: &AnyObject) {
    unsafe {
        let context: *mut AnyObject = msg_send![view, inputContext];
        if !context.is_null() {
            let _: () = msg_send![context, discardMarkedText];
        }
    }
    let _ = with_state(view, |state| state.text.focus_lost());
}

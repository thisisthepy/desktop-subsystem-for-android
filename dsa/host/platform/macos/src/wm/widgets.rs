//! The widgets placed on the user's desktop, as panels of the window manager on the Mac's
//! desktop, which the widget desk (`dsa_host_core::widget_windows::desk`) drives.
//!
//! The window manager's main thread runs AppKit for them, as an accessory app (no Dock icon,
//! no menu bar of its own); the rest of the window manager runs on threads of its own. The
//! widgets' watch runs on one too ([`dsa_host_core::widget_windows::sync`]) and hands each of
//! its commands to the main thread, which hands the panels' reports back.
//!
//! Each widget is a borderless, non-activating `NSPanel` on the desktop's layer (see
//! [`super::widget_panels`]) whose content view shows the desk's picture as its layer's
//! contents and passes the mouse, the trackpad's force click and the keyboard to the desk. The
//! pointer is taken from the screen, which a move of the panel does not change under it. The
//! widget's menu is an `NSMenu`, Resize a submenu of it. A press held for half a second, or a
//! force click, lifts the widget for editing; the panel then takes the keyboard, so Escape
//! ends the edit, and a click on another app's window ends it too.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use block2::RcBlock;
use dsa_host_core::widget_windows::desk::{Anchor, Desk, Effect};
use dsa_host_core::widget_windows::menu::Entry;
use dsa_host_core::widget_windows::paint::Image;
use dsa_host_core::widget_windows::{WidgetWindows, WindowCommand, WindowReport};
use dsa_host_unix::socket::UnixLocalConnector;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::sel;

use super::log;
use super::widget_panels::{
    argb_of, button, desk_point, dpi_of_backing, force_click, key, look, menu_items, panel_frame,
    widget_level, MenuItem, COLLECTION_BEHAVIOR, DESKTOP_ICON_LEVEL_KEY, STYLE,
};
use crate::compositor::menu_bar::korean;
use crate::objc::{class, ns_string, on_main, CGPoint, CGRect, CGSize};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowLevelForKey(key: i32) -> i32;
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        info: u32,
    ) -> *mut c_void;
    fn CGBitmapContextCreateImage(context: *mut c_void) -> *mut c_void;
    fn CGContextRelease(context: *mut c_void);
    fn CGImageRelease(image: *mut c_void);
}

/// `kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little`: premultiplied BGRA in
/// memory, the desk's pictures as they are.
const BITMAP_BGRA: u32 = 2 | (2 << 12);

/// What the window manager calls itself to the service for the widgets.
const BUILD: &str = "dsa-wm widgets";

/// How often the desk is looked at while a press may become a long press or a resize has a
/// size to pass on.
const TICK: f64 = 1.0 / 30.0;

/// How often the window manager looks whether it should stop.
const STOP_CHECK: f64 = 0.5;

/// One widget's panel.
struct Panel {
    window: Retained<AnyObject>,
    view: Retained<AnyObject>,
    /// The panel's screen's backing scale.
    scale: f64,
}

/// The widget panels, on the main thread.
struct Panels {
    desk: Desk,
    panels: BTreeMap<i32, Panel>,
    reports: Sender<WindowReport>,
    dirty: BTreeSet<i32>,
    /// The timer that ticks the desk, while it is busy.
    timer: Option<Retained<AnyObject>>,
}

thread_local! {
    static PANELS: RefCell<Option<Panels>> = const { RefCell::new(None) };
    /// The widget of each panel's view, by the view's address.
    static VIEWS: RefCell<HashMap<usize, i32>> = RefCell::new(HashMap::new());
    /// The tag of the menu item the user chose in the menu open now.
    static CHOSEN: Cell<Option<isize>> = const { Cell::new(None) };
}

/// Runs `f` on the panels. A call that comes while they are in use (AppKit calling back from
/// inside a call they made) is dropped rather than aliased, and one that panics never unwinds
/// into AppKit.
fn with_panels<R>(f: impl FnOnce(&mut Panels) -> R) -> Option<R> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PANELS.with(|panels| {
            let mut panels = panels.try_borrow_mut().ok()?;
            panels.as_mut().map(f)
        })
    }))
    .ok()
    .flatten()
}

/// The height of the main screen in points, which AppKit's screen coordinates count up from.
unsafe fn main_height() -> f64 {
    let Ok(screen_class) = class(c"NSScreen") else {
        return 0.0;
    };
    let screens: *mut AnyObject = msg_send![screen_class, screens];
    if screens.is_null() {
        return 0.0;
    }
    let count: usize = msg_send![screens, count];
    if count == 0 {
        return 0.0;
    }
    let main: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
    let frame: CGRect = msg_send![main, frame];
    frame.size.height
}

/// The main screen's backing scale.
unsafe fn main_scale() -> f64 {
    let Ok(screen_class) = class(c"NSScreen") else {
        return 1.0;
    };
    let screen: *mut AnyObject = msg_send![screen_class, mainScreen];
    if screen.is_null() {
        1.0
    } else {
        msg_send![screen, backingScaleFactor]
    }
}

/// The work areas of the screens (their visible frames, less the menu bar and the Dock), in
/// the desk's pixels at `scale`.
unsafe fn work_areas(scale: f64) -> Vec<(i32, i32, i32, i32)> {
    let mut out = Vec::new();
    let Ok(screen_class) = class(c"NSScreen") else {
        return out;
    };
    let screens: *mut AnyObject = msg_send![screen_class, screens];
    if screens.is_null() {
        return out;
    }
    let height = main_height();
    let count: usize = msg_send![screens, count];
    for index in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index];
        let visible: CGRect = msg_send![screen, visibleFrame];
        let (left, top) = desk_point(
            (visible.origin.x, visible.origin.y + visible.size.height),
            height,
            scale,
        );
        let (right, bottom) = desk_point(
            (visible.origin.x + visible.size.width, visible.origin.y),
            height,
            scale,
        );
        out.push((left, top, right, bottom));
    }
    out
}

/// Whether the user's appearance is dark, and their accent color.
unsafe fn appearance() -> (bool, u32) {
    let dark = (|| -> Option<bool> {
        let app: *mut AnyObject = msg_send![class(c"NSApplication").ok()?, sharedApplication];
        let appearance: *mut AnyObject = msg_send![app, effectiveAppearance];
        if appearance.is_null() {
            return None;
        }
        let name: *mut AnyObject = msg_send![appearance, name];
        Some(crate::objc::string_of(name).contains("Dark"))
    })()
    .unwrap_or(false);
    let accent = (|| -> Option<u32> {
        let color: *mut AnyObject = msg_send![class(c"NSColor").ok()?, controlAccentColor];
        let space: *mut AnyObject = msg_send![class(c"NSColorSpace").ok()?, sRGBColorSpace];
        if color.is_null() || space.is_null() {
            return None;
        }
        let rgb: *mut AnyObject = msg_send![color, colorUsingColorSpace: space];
        if rgb.is_null() {
            return None;
        }
        let red: f64 = msg_send![rgb, redComponent];
        let green: f64 = msg_send![rgb, greenComponent];
        let blue: f64 = msg_send![rgb, blueComponent];
        Some(argb_of(red, green, blue))
    })()
    .unwrap_or(0);
    (dark, accent)
}

/// The widget of the panel `view` is the content of.
fn widget_of(view: &AnyObject) -> Option<i32> {
    let key = view as *const AnyObject as usize;
    VIEWS.with(|views| views.borrow().get(&key).copied())
}

/// The pointer's place on the screen, in the desk's pixels of widget `widget_id`'s window.
unsafe fn pointer_local(panels: &Panels, widget_id: i32) -> (i32, i32) {
    let scale = panels.panels.get(&widget_id).map_or(1.0, |p| p.scale);
    let Ok(event_class) = class(c"NSEvent") else {
        return (0, 0);
    };
    let at: CGPoint = msg_send![event_class, mouseLocation];
    let (x, y) = desk_point((at.x, at.y), main_height(), scale);
    let origin = panels.desk.window(widget_id).map_or((0, 0), |w| w.origin);
    (x - origin.0, y - origin.1)
}

unsafe extern "C-unwind" fn mouse_down(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    let number: isize = msg_send![event, buttonNumber];
    let modifiers: usize = msg_send![event, modifierFlags];
    let Some(button) = button(number, modifiers as u64) else {
        return;
    };
    with_panels(|panels| {
        let local = pointer_local(panels, widget_id);
        panels
            .desk
            .down(widget_id, local, button, false, Instant::now());
    });
    after_input();
}

unsafe extern "C-unwind" fn mouse_up(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    let number: isize = msg_send![event, buttonNumber];
    let modifiers: usize = msg_send![event, modifierFlags];
    let Some(button) = button(number, modifiers as u64) else {
        return;
    };
    with_panels(|panels| {
        let local = pointer_local(panels, widget_id);
        panels.desk.up(widget_id, local, button);
    });
    after_input();
}

unsafe extern "C-unwind" fn mouse_moved(this: &AnyObject, _: Sel, _event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    with_panels(|panels| {
        let local = pointer_local(panels, widget_id);
        panels.desk.moved(widget_id, local);
    });
    after_input();
}

unsafe extern "C-unwind" fn mouse_entered(this: &AnyObject, _: Sel, _event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    with_panels(|panels| {
        let local = pointer_local(panels, widget_id);
        panels.desk.entered(widget_id, local);
    });
    after_input();
}

unsafe extern "C-unwind" fn mouse_exited(this: &AnyObject, _: Sel, _event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    with_panels(|panels| panels.desk.left(widget_id));
    after_input();
}

unsafe extern "C-unwind" fn pressure_change(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    let stage: isize = msg_send![event, stage];
    if force_click(stage) {
        with_panels(|panels| panels.desk.force_press(widget_id));
        after_input();
    }
}

unsafe extern "C-unwind" fn key_down(this: &AnyObject, _: Sel, event: *mut AnyObject) {
    let Some(widget_id) = widget_of(this) else {
        return;
    };
    let code: u16 = msg_send![event, keyCode];
    if let Some(key) = key(code) {
        with_panels(|panels| panels.desk.key(widget_id, key));
        after_input();
    }
}

unsafe extern "C-unwind" fn resign_key(this: &AnyObject, _: Sel) {
    // The panel's view is its content: the panel lost the keyboard to another window.
    let view: *mut AnyObject = msg_send![this, contentView];
    if let Some(widget_id) = (!view.is_null()).then(|| widget_of(&*view)).flatten() {
        with_panels(|panels| panels.desk.focus(widget_id, false));
        after_input();
    }
    let superclass = class(c"NSPanel").expect("AppKit has NSPanel");
    let _: () = msg_send![super(this, superclass), resignKeyWindow];
}

unsafe extern "C-unwind" fn become_key(this: &AnyObject, _: Sel) {
    let view: *mut AnyObject = msg_send![this, contentView];
    if let Some(widget_id) = (!view.is_null()).then(|| widget_of(&*view)).flatten() {
        with_panels(|panels| panels.desk.focus(widget_id, true));
        after_input();
    }
    let superclass = class(c"NSPanel").expect("AppKit has NSPanel");
    let _: () = msg_send![super(this, superclass), becomeKeyWindow];
}

unsafe extern "C-unwind" fn yes(_this: &AnyObject, _: Sel) -> Bool {
    Bool::YES
}

unsafe extern "C-unwind" fn no(_this: &AnyObject, _: Sel) -> Bool {
    Bool::NO
}

unsafe extern "C-unwind" fn accepts_first_mouse(_: &AnyObject, _: Sel, _: *mut AnyObject) -> Bool {
    Bool::YES
}

unsafe extern "C-unwind" fn choose(_this: &AnyObject, _: Sel, sender: *mut AnyObject) {
    if sender.is_null() {
        return;
    }
    let tag: isize = msg_send![sender, tag];
    CHOSEN.with(|chosen| chosen.set(Some(tag)));
}

fn view_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            type Event<'a> = unsafe extern "C-unwind" fn(&'a AnyObject, Sel, *mut AnyObject);
            let mut b = ClassBuilder::new(c"DSAWidgetView", class(c"NSView")?)
                .ok_or("the widget view class is already defined")?;
            for selector in [sel!(mouseDown:), sel!(rightMouseDown:)] {
                b.add_method(selector, mouse_down as Event<'_>);
            }
            for selector in [sel!(mouseUp:), sel!(rightMouseUp:)] {
                b.add_method(selector, mouse_up as Event<'_>);
            }
            for selector in [sel!(mouseMoved:), sel!(mouseDragged:)] {
                b.add_method(selector, mouse_moved as Event<'_>);
            }
            b.add_method(sel!(mouseEntered:), mouse_entered as Event<'_>);
            b.add_method(sel!(mouseExited:), mouse_exited as Event<'_>);
            b.add_method(sel!(pressureChangeWithEvent:), pressure_change as Event<'_>);
            b.add_method(sel!(keyDown:), key_down as Event<'_>);
            b.add_method(
                sel!(acceptsFirstResponder),
                yes as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            // The desk moves the panel itself, after a long press.
            b.add_method(
                sel!(mouseDownCanMoveWindow),
                no as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(acceptsFirstMouse:),
                accepts_first_mouse as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            Ok(b.register())
        })
        .clone()
}

fn panel_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let mut b = ClassBuilder::new(c"DSAWidgetPanel", class(c"NSPanel")?)
                .ok_or("the widget panel class is already defined")?;
            // A borderless panel takes the keyboard only when it says it can: while its
            // widget is edited, for Escape.
            b.add_method(
                sel!(canBecomeKeyWindow),
                yes as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(canBecomeMainWindow),
                no as unsafe extern "C-unwind" fn(_, _) -> _,
            );
            b.add_method(
                sel!(becomeKeyWindow),
                become_key as unsafe extern "C-unwind" fn(_, _),
            );
            b.add_method(
                sel!(resignKeyWindow),
                resign_key as unsafe extern "C-unwind" fn(_, _),
            );
            Ok(b.register())
        })
        .clone()
}

fn target_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let mut b = ClassBuilder::new(c"DSAWidgetMenuTarget", class(c"NSObject")?)
                .ok_or("the widget menu's target class is already defined")?;
            b.add_method(
                sel!(chooseWidgetItem:),
                choose as unsafe extern "C-unwind" fn(_, _, _),
            );
            Ok(b.register())
        })
        .clone()
}

/// `NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved | NSTrackingActiveAlways |
/// NSTrackingInVisibleRect`.
const TRACKING: usize = 0x01 | 0x02 | 0x80 | 0x200;

/// Opens widget `widget_id`'s panel at `origin`, `size` pixels.
unsafe fn open_panel(
    widget_id: i32,
    origin: (i32, i32),
    size: (i32, i32),
) -> Result<Panel, String> {
    let scale = main_scale();
    let (x, y, width, height) = panel_frame(origin, size, main_height(), scale);
    let rect = CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    };
    let allocated: Allocated<AnyObject> = msg_send![panel_class()?, alloc];
    let window: Option<Retained<AnyObject>> = msg_send![
        allocated,
        initWithContentRect: rect,
        styleMask: STYLE,
        backing: 2usize,
        defer: false
    ];
    let window = window.ok_or("AppKit could not make the widget's panel")?;
    // The screen it is on may have another scale than the main one.
    let scale: f64 = msg_send![&*window, backingScaleFactor];
    let scale = if scale > 0.0 { scale } else { main_scale() };
    let _: () = msg_send![&*window, setReleasedWhenClosed: false];
    let _: () = msg_send![&*window, setOpaque: false];
    let clear: *mut AnyObject = msg_send![class(c"NSColor")?, clearColor];
    let _: () = msg_send![&*window, setBackgroundColor: clear];
    let _: () = msg_send![&*window, setHasShadow: false];
    let _: () =
        msg_send![&*window, setLevel: widget_level(CGWindowLevelForKey(DESKTOP_ICON_LEVEL_KEY))];
    let _: () = msg_send![&*window, setCollectionBehavior: COLLECTION_BEHAVIOR];
    let _: () = msg_send![&*window, setHidesOnDeactivate: false];
    let _: () = msg_send![&*window, setBecomesKeyOnlyIfNeeded: true];
    let _: () = msg_send![&*window, setAcceptsMouseMovedEvents: true];

    let allocated: Allocated<AnyObject> = msg_send![view_class()?, alloc];
    let bounds = CGRect {
        origin: CGPoint::default(),
        size: CGSize { width, height },
    };
    let view: Option<Retained<AnyObject>> = msg_send![allocated, initWithFrame: bounds];
    let view = view.ok_or("AppKit could not make the widget's view")?;
    let _: () = msg_send![&*view, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![&*view, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setContentsScale: scale];
    }
    let tracking: Allocated<AnyObject> = msg_send![class(c"NSTrackingArea")?, alloc];
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
    let _: () = msg_send![&*window, setContentView: &*view];
    VIEWS.with(|views| {
        views
            .borrow_mut()
            .insert(Retained::as_ptr(&view) as usize, widget_id)
    });
    let _: () = msg_send![&*window, orderFront: std::ptr::null::<AnyObject>()];
    Ok(Panel {
        window,
        view,
        scale,
    })
}

/// Moves and sizes a panel to the desk's `origin` and `size`.
unsafe fn place_panel(panel: &Panel, origin: (i32, i32), size: (i32, i32)) {
    let (x, y, width, height) = panel_frame(origin, size, main_height(), panel.scale);
    let rect = CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    };
    let _: () = msg_send![&*panel.window, setFrame: rect, display: false];
}

/// Shows `picture` in a panel, as its view's layer's contents.
unsafe fn show_picture(panel: &Panel, picture: &Image) {
    let layer: *mut AnyObject = msg_send![&*panel.view, layer];
    if layer.is_null() || picture.width == 0 || picture.height == 0 {
        return;
    }
    let mut pixels = picture.pixels.clone();
    let space = CGColorSpaceCreateDeviceRGB();
    let context = CGBitmapContextCreate(
        pixels.as_mut_ptr().cast(),
        picture.width as usize,
        picture.height as usize,
        8,
        picture.width as usize * 4,
        space,
        BITMAP_BGRA,
    );
    CGColorSpaceRelease(space);
    if context.is_null() {
        return;
    }
    // The image is a copy; the pixels may go once it is made.
    let image = CGBitmapContextCreateImage(context);
    CGContextRelease(context);
    if image.is_null() {
        return;
    }
    let _: () = msg_send![layer, setContents: image as *mut AnyObject];
    let _: () = msg_send![layer, setContentsScale: panel.scale];
    CGImageRelease(image);
}

/// Shows the widget's menu where `anchor` says and tells the desk what the user chose. The
/// menu runs a loop of its own, so nothing of the panels is held while it is open.
unsafe fn show_menu(widget_id: i32, anchor: Anchor, entries: Vec<Entry>) {
    let made = (|| -> Result<(Retained<AnyObject>, Retained<AnyObject>), String> {
        let target: Option<Retained<AnyObject>> = msg_send![target_class()?, new];
        let target = target.ok_or("no target for the widget's menu could be made")?;
        let menu = new_menu(&menu_items(&entries), &target)?;
        Ok((menu, target))
    })();
    let (menu, _target) = match made {
        Ok(made) => made,
        Err(why) => {
            log(&format!(
                "the menu of widget {widget_id} did not open: {why}"
            ));
            with_panels(|panels| panels.desk.menu_closed(widget_id, None, false));
            after_input();
            return;
        }
    };
    let Some((view, scale, origin)) = with_panels(|panels| {
        let panel = panels.panels.get(&widget_id)?;
        let origin = panels.desk.window(widget_id)?.origin;
        Some((panel.view.clone(), panel.scale, origin))
    })
    .flatten() else {
        return;
    };
    // The anchor, from the desk's screen pixels to the view's points from its bottom left.
    let bounds: CGRect = msg_send![&*view, bounds];
    let at = match anchor {
        Anchor::Below((left, _, _, bottom)) => (left, bottom),
        Anchor::At(x, y) => (x, y),
    };
    let location = CGPoint {
        x: f64::from(at.0 - origin.0) / scale,
        y: bounds.size.height - f64::from(at.1 - origin.1) / scale,
    };
    CHOSEN.with(|chosen| chosen.set(None));
    let _: Bool = msg_send![
        &*menu,
        popUpMenuPositioningItem: std::ptr::null::<AnyObject>(),
        atLocation: location,
        inView: &*view
    ];
    let chosen = CHOSEN.with(|chosen| chosen.take());
    // A menu closed with nothing chosen: by a press outside the widget, which ends an edit,
    // or with Escape, which does not.
    let pressed_beside = chosen.is_none() && {
        let pressed: usize = msg_send![
            class(c"NSEvent").expect("AppKit has NSEvent"),
            pressedMouseButtons
        ];
        let mouse: CGPoint = msg_send![
            class(c"NSEvent").expect("AppKit has NSEvent"),
            mouseLocation
        ];
        let window: *mut AnyObject = msg_send![&*view, window];
        let frame: CGRect = if window.is_null() {
            CGRect::default()
        } else {
            msg_send![window, frame]
        };
        let inside = mouse.x >= frame.origin.x
            && mouse.x < frame.origin.x + frame.size.width
            && mouse.y >= frame.origin.y
            && mouse.y < frame.origin.y + frame.size.height;
        pressed != 0 && !inside
    };
    with_panels(|panels| {
        panels
            .desk
            .menu_closed(widget_id, chosen.map(|tag| tag as u32), pressed_beside)
    });
    after_input();
}

unsafe fn new_menu(items: &[MenuItem], target: &AnyObject) -> Result<Retained<AnyObject>, String> {
    let allocated: Allocated<AnyObject> = msg_send![class(c"NSMenu")?, alloc];
    let menu: Option<Retained<AnyObject>> = msg_send![allocated, initWithTitle: &*ns_string("")];
    let menu = menu.ok_or("AppKit could not make a menu")?;
    let _: () = msg_send![&*menu, setAutoenablesItems: false];
    for item in items {
        let made: Retained<AnyObject> = match item {
            MenuItem::Separator => {
                let separator: Option<Retained<AnyObject>> =
                    msg_send![class(c"NSMenuItem")?, separatorItem];
                separator.ok_or("AppKit could not make a separator")?
            }
            MenuItem::Item {
                title,
                tag,
                checked,
            } => {
                let allocated: Allocated<AnyObject> = msg_send![class(c"NSMenuItem")?, alloc];
                let made: Option<Retained<AnyObject>> = msg_send![
                    allocated,
                    initWithTitle: &*ns_string(title),
                    action: sel!(chooseWidgetItem:),
                    keyEquivalent: &*ns_string("")
                ];
                let made = made.ok_or("AppKit could not make a menu item")?;
                let _: () = msg_send![&*made, setTarget: target];
                let _: () = msg_send![&*made, setTag: *tag];
                // NSControlStateValueOn.
                let _: () = msg_send![&*made, setState: if *checked { 1isize } else { 0 }];
                made
            }
            MenuItem::Submenu { title, items } => {
                let allocated: Allocated<AnyObject> = msg_send![class(c"NSMenuItem")?, alloc];
                let made: Option<Retained<AnyObject>> = msg_send![
                    allocated,
                    initWithTitle: &*ns_string(title),
                    action: None::<Sel>,
                    keyEquivalent: &*ns_string("")
                ];
                let made = made.ok_or("AppKit could not make a menu item")?;
                let submenu = new_menu(items, target)?;
                let _: () = msg_send![&*made, setSubmenu: &*submenu];
                made
            }
        };
        let _: () = msg_send![&*menu, addItem: &*made];
    }
    Ok(menu)
}

/// Carries out the desk's effects on the panels, draws the ones that changed, passes the
/// reports on, and keeps the desk's timer while it is busy.
fn after_input() {
    let mut menus = Vec::new();
    with_panels(|panels| unsafe {
        // An effect may lead to more (a panel opening on a screen of another scale).
        loop {
            let effects = panels.desk.take_effects();
            if effects.is_empty() {
                break;
            }
            for effect in effects {
                match effect {
                    Effect::Open {
                        widget_id,
                        origin,
                        size,
                    } => {
                        if let Some(panel) = panels.panels.get(&widget_id) {
                            place_panel(panel, origin, size);
                        } else {
                            match open_panel(widget_id, origin, size) {
                                Ok(panel) => {
                                    // A panel on a screen of another scale keeps its widget's size
                                    // in dp there.
                                    if (panel.scale - main_scale()).abs() > f64::EPSILON {
                                        panels
                                            .desk
                                            .scale_changed(widget_id, dpi_of_backing(panel.scale));
                                    }
                                    panels.panels.insert(widget_id, panel);
                                }
                                Err(why) => log(&format!(
                                    "the panel of widget {widget_id} did not open: {why}"
                                )),
                            }
                        }
                        panels.dirty.insert(widget_id);
                    }
                    Effect::Place {
                        widget_id,
                        origin,
                        size,
                    } => {
                        if let Some(panel) = panels.panels.get(&widget_id) {
                            place_panel(panel, origin, size);
                        }
                        panels.dirty.insert(widget_id);
                    }
                    Effect::Redraw { widget_id } => {
                        panels.dirty.insert(widget_id);
                    }
                    Effect::Close { widget_id } => {
                        if let Some(panel) = panels.panels.remove(&widget_id) {
                            VIEWS.with(|views| {
                                views
                                    .borrow_mut()
                                    .remove(&(Retained::as_ptr(&panel.view) as usize))
                            });
                            let _: () =
                                msg_send![&*panel.window, orderOut: std::ptr::null::<AnyObject>()];
                            let _: () = msg_send![&*panel.window, close];
                        }
                        panels.dirty.remove(&widget_id);
                    }
                    Effect::Menu {
                        widget_id,
                        anchor,
                        entries,
                        ..
                    } => menus.push((widget_id, anchor, entries)),
                    Effect::Focus { widget_id } => {
                        if let Some(panel) = panels.panels.get(&widget_id) {
                            let _: () = msg_send![&*panel.window, makeKeyWindow];
                            let _: Bool =
                                msg_send![&*panel.window, makeFirstResponder: &*panel.view];
                        }
                    }
                    Effect::Log(line) => log(&line),
                }
            }
        }
        let now = Instant::now();
        for widget_id in std::mem::take(&mut panels.dirty) {
            if let (Some(panel), Some(picture)) = (
                panels.panels.get(&widget_id),
                panels.desk.picture(widget_id, now),
            ) {
                show_picture(panel, &picture);
            }
        }
        for report in panels.desk.take_reports() {
            let _ = panels.reports.send(report);
        }
        keep_timer(panels, now);
    });
    // The menus open once nothing of the panels is held: each runs a loop of its own.
    for (widget_id, anchor, entries) in menus {
        unsafe { show_menu(widget_id, anchor, entries) };
    }
}

/// Starts the desk's timer while it is busy, and stops it once it is not.
unsafe fn keep_timer(panels: &mut Panels, now: Instant) {
    let busy = panels.desk.busy(now);
    match (&panels.timer, busy) {
        (Some(timer), false) => {
            let _: () = msg_send![&**timer, invalidate];
            panels.timer = None;
        }
        (None, true) => {
            let block = RcBlock::new(|_timer: *mut AnyObject| {
                with_panels(|panels| panels.desk.tick(Instant::now()));
                after_input();
            });
            let Ok(timer_class) = class(c"NSTimer") else {
                return;
            };
            let timer: Option<Retained<AnyObject>> = msg_send![
                timer_class,
                scheduledTimerWithTimeInterval: TICK,
                repeats: true,
                block: &*block
            ];
            panels.timer = timer;
        }
        _ => {}
    }
}

/// The panels as the widgets' watch drives them from its thread: its commands go to the
/// main thread, which hands the reports back.
struct MainPanels {
    reports: Receiver<WindowReport>,
}

impl WidgetWindows for MainPanels {
    fn command(&mut self, command: WindowCommand) {
        on_main(move || {
            with_panels(|panels| {
                let scale = unsafe { main_scale() };
                panels.desk.dpi = dpi_of_backing(scale);
                panels.desk.set_areas(unsafe { work_areas(scale) });
                panels.desk.command(command, Instant::now());
            });
            after_input();
        });
    }

    fn reports(&mut self) -> Vec<WindowReport> {
        self.reports.try_iter().collect()
    }
}

/// Runs the widget panels on this thread, the main one, until `stop` is set or `done` says the
/// rest of the window manager has ended: AppKit as an accessory app, and the widgets' watch on
/// a thread of its own through the local API at `host_api`.
pub fn run_main(
    host_api: PathBuf,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
) -> Result<(), String> {
    if !crate::objc::is_main_thread() {
        return Err("the widget panels must run on the main thread".into());
    }
    let (reports, received) = channel();
    let (dark, accent) = unsafe { appearance() };
    let decode: dsa_host_core::widget_windows::desk::Decode = Box::new(|png: &[u8]| {
        dsa_host_core::png::decode(png)
            .map(|image| Image::from_rgba(image.width, image.height, &image.pixels))
    });
    let dpi = dpi_of_backing(unsafe { main_scale() });
    PANELS.with(|panels| {
        *panels.borrow_mut() = Some(Panels {
            desk: Desk::new(look(dark, accent), dpi, korean(), decode),
            panels: BTreeMap::new(),
            reports,
            dirty: BTreeSet::new(),
            timer: None,
        })
    });
    {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("dsa-wm-widgets".into())
            .spawn(move || {
                let mut panels = MainPanels { reports: received };
                let connector = UnixLocalConnector { path: host_api };
                dsa_host_core::widget_windows::sync(
                    Box::new(connector),
                    &mut panels,
                    stop,
                    BUILD,
                    Arc::new(log),
                );
            })
            .map_err(|e| format!("the widgets' watch could not start: {e}"))?;
    }
    unsafe {
        let app: *mut AnyObject = msg_send![class(c"NSApplication")?, sharedApplication];
        // An accessory: no Dock icon and no menu bar, and its panels never take the focus
        // from the app the user works in but while a widget is edited.
        let _: bool = msg_send![app, setActivationPolicy: 1isize];
        let check = RcBlock::new(move |_timer: *mut AnyObject| {
            if stop.load(Ordering::Relaxed) || done.load(Ordering::Relaxed) {
                if let Ok(app_class) = class(c"NSApplication") {
                    let app: *mut AnyObject = msg_send![app_class, sharedApplication];
                    let _: () = msg_send![app, stop: std::ptr::null::<AnyObject>()];
                    // `stop:` takes effect after the next event; one is posted so it does.
                    post_wake(app);
                }
            }
            // The appearance may have changed; the next drawing follows it.
            let (dark, accent) = appearance();
            with_panels(|panels| {
                let new = look(dark, accent);
                if panels.desk.look != new {
                    panels.desk.look = new;
                    let ids = panels.desk.ids();
                    panels.dirty.extend(ids);
                }
            });
            after_input();
        });
        let _: Option<Retained<AnyObject>> = msg_send![
            class(c"NSTimer")?,
            scheduledTimerWithTimeInterval: STOP_CHECK,
            repeats: true,
            block: &*check
        ];
        let _: () = msg_send![app, run];
    }
    Ok(())
}

/// Posts an application-defined event, which wakes the run loop so a `stop:` takes effect.
unsafe fn post_wake(app: *mut AnyObject) {
    let Ok(event_class) = class(c"NSEvent") else {
        return;
    };
    // NSEventTypeApplicationDefined.
    let event: *mut AnyObject = msg_send![
        event_class,
        otherEventWithType: 15usize,
        location: CGPoint::default(),
        modifierFlags: 0usize,
        timestamp: 0f64,
        windowNumber: 0isize,
        context: std::ptr::null::<AnyObject>(),
        subtype: 0i16,
        data1: 0isize,
        data2: 0isize
    ];
    if !event.is_null() {
        let _: () = msg_send![app, postEvent: event, atStart: true];
    }
}

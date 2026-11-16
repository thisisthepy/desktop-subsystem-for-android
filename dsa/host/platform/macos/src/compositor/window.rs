//! One `NSWindow` per Android task.
//!
//! The window's content view is the task's input view ([`crate::input::view`]), which hosts
//! the task's `CAMetalLayer` ([`super::metal`]). The window's delegate reports resizes, the
//! close button, focus and display changes to the code that runs the task, which passes them
//! to the guest. Closing a window asks the guest to finish its task; the window goes when the
//! task does, as on every other host.
//!
//! Every window has the unified title bar: transparent, with the app's frames under the band
//! and the window buttons over it, with the window's name after them and, while the task can go
//! back, a back button at the right end. The title's tone follows the app's status bar, and a band the app leaves
//! empty takes the colour under it ([`super::title_bar`]). An app that draws its own title bar
//! hides the name; an app set to start below the title bar gets AppKit's standard one. Whenever the band changes (the mode, the back button, the scale,
//! full screen), the window reports it, so the guest gives the app its inset and the areas
//! the window's buttons take.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use dsa_host_core::compositor::caption::{BandSample, CaptionBand, CaptionTone};
use dsa_host_core::compositor::window_sizing::to_task_size;
use dsa_host_core::tasks::Bounds;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::sel;

use super::metal::{FrameRef, Presenter};
use super::sizing::{picture_in_picture, task_pixels, window_points, Rect, MIN_CONTENT_POINTS};
use super::title_bar::{
    appearance_name, back_label, caption_band, sample_bgra, title_layout, BandTone, Chrome, Look,
};
use crate::input::view::{self, InputSink};
use crate::objc::{class, ns_string, CGPoint, CGRect, CGSize};

/// What a task window tells the code running its task.
pub trait WindowEvents {
    /// The window's content is now `pixels` big: the guest's task should take that size.
    fn resized(&mut self, pixels: (u32, u32));
    /// The user asked the window to close: the guest should finish the task.
    fn close_requested(&mut self);
    /// The window gained or lost the keyboard focus.
    fn focused(&mut self, focused: bool);
    /// The window moved to a display of another backing scale.
    fn scale_changed(&mut self, scale: f64);
    /// The window was minimized into the Dock (`shown` false), or came back from it.
    fn shown(&mut self, _shown: bool) {}
    /// The user started resizing the window by its edge, or let go.
    fn dragging(&mut self, _dragging: bool) {}
    /// The title bar band over the window's content is `band` now (empty with a standard
    /// title bar, which sits above the content).
    fn caption(&mut self, _band: CaptionBand) {}
    /// The user clicked the title bar's back button: the task should go back.
    fn back_pressed(&mut self) {}
}

/// No title bar, no buttons, no border: the window of an app overlay window.
const STYLE_BORDERLESS: usize = 0;
const STYLE_TITLED: usize = 1 << 0;
const STYLE_CLOSABLE: usize = 1 << 1;
const STYLE_MINIATURIZABLE: usize = 1 << 2;
const STYLE_RESIZABLE: usize = 1 << 3;
/// `NSWindowStyleMaskFullSizeContentView`: the content runs under the title bar.
const STYLE_FULL_SIZE_CONTENT: usize = 1 << 15;
/// `NSWindowTitleVisible` and `NSWindowTitleHidden`.
const TITLE_VISIBLE: isize = 0;
const TITLE_HIDDEN: isize = 1;
/// `NSBezelStyleToolbar` (formerly `NSBezelStyleTexturedRounded`), the look of a button in a
/// title bar.
const BEZEL_TOOLBAR: usize = 11;
const BACKING_BUFFERED: usize = 2;
const BEHAVIOR_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const BEHAVIOR_FULL_SCREEN_PRIMARY: usize = 1 << 7;
const LEVEL_NORMAL: isize = 0;
const LEVEL_FLOATING: isize = 3;

thread_local! {
    /// Each open window's events, by the window's address. Only the main thread touches
    /// windows, so only it has entries.
    static EVENTS: RefCell<HashMap<usize, Box<dyn WindowEvents>>> = RefCell::new(HashMap::new());
    /// The band each open window last reported, by the window's address.
    static BANDS: RefCell<HashMap<usize, CaptionBand>> = RefCell::new(HashMap::new());
    /// What decides each open window's title bar tone and fill, by the window's address.
    static TONES: RefCell<HashMap<usize, BandTone>> = RefCell::new(HashMap::new());
    /// The layer each open window fills its title bar band with when its app leaves the band
    /// empty, by the window's address.
    static FILLS: RefCell<HashMap<usize, Retained<AnyObject>>> = RefCell::new(HashMap::new());
    /// The parts of each open window's title bar, by the window's address.
    static TITLES: RefCell<HashMap<usize, TitleParts>> = RefCell::new(HashMap::new());
}

/// The back button and the window's name, both subviews of the title bar the window buttons
/// stand in. A transparent title bar puts AppKit's own title elsewhere, and its accessories
/// ignore a hidden view, so both are laid out here against the window buttons.
struct TitleParts {
    button: Retained<AnyObject>,
    label: Retained<AnyObject>,
    /// Whether the back button shows.
    back: bool,
    /// Whether the name shows (the unified title bar).
    named: bool,
}

/// Puts the name and the back button where [`title_layout`] says, in the title bar.
unsafe fn relayout_title(window: *mut AnyObject) {
    TITLES.with(|titles| {
        let titles = titles.borrow();
        let Some(parts) = titles.get(&(window as usize)) else {
            return;
        };
        let _: () = msg_send![&*parts.button, setHidden: !parts.back];
        let _: () = msg_send![&*parts.label, setHidden: !parts.named];
        let close: *mut AnyObject = msg_send![window, standardWindowButton: 0usize];
        let zoom: *mut AnyObject = msg_send![window, standardWindowButton: 2usize];
        if close.is_null() || zoom.is_null() {
            return;
        }
        let bar: *mut AnyObject = msg_send![close, superview];
        if bar.is_null() {
            return;
        }
        let close_frame: CGRect = msg_send![close, frame];
        let zoom_frame: CGRect = msg_send![zoom, frame];
        let bounds: CGRect = msg_send![bar, bounds];
        let _: () = msg_send![&*parts.label, sizeToFit];
        let natural: CGRect = msg_send![&*parts.label, frame];
        let layout = title_layout(
            bounds.size.width,
            close_frame.origin.y + close_frame.size.height / 2.0,
            zoom_frame.origin.x + zoom_frame.size.width,
            (natural.size.width, natural.size.height),
            parts.back,
        );
        let name = layout.name;
        let _: () = msg_send![
            &*parts.label,
            setFrame: CGRect {
                origin: CGPoint { x: name.0, y: name.1 },
                size: CGSize { width: name.2, height: natural.size.height },
            }
        ];
        if let Some((x, y, w, h)) = layout.back {
            let _: () = msg_send![
                &*parts.button,
                setFrame: CGRect {
                    origin: CGPoint { x, y },
                    size: CGSize { width: w, height: h },
                }
            ];
        }
    });
}

/// The left and right edges of `view`, in points from its window's left.
unsafe fn x_range(view: *mut AnyObject) -> (f64, f64) {
    let bounds: CGRect = msg_send![view, bounds];
    let in_window: CGRect =
        msg_send![view, convertRect: bounds, toView: std::ptr::null::<AnyObject>()];
    (
        in_window.origin.x,
        in_window.origin.x + in_window.size.width,
    )
}

/// The band `window`'s title bar takes over its content: none with a standard title bar,
/// else its height and what the window buttons and a shown back button take at its ends.
unsafe fn band_of(window: *mut AnyObject) -> CaptionBand {
    let mask: usize = msg_send![window, styleMask];
    if mask & STYLE_FULL_SIZE_CONTENT == 0 {
        return CaptionBand::default();
    }
    let _: () = msg_send![window, layoutIfNeeded];
    let content: *mut AnyObject = msg_send![window, contentView];
    if content.is_null() {
        return CaptionBand::default();
    }
    let bounds: CGRect = msg_send![content, bounds];
    // The part of the content no title bar covers, in window coordinates (from the bottom).
    let layout: CGRect = msg_send![window, contentLayoutRect];
    let height = bounds.size.height - (layout.origin.y + layout.size.height);
    let mut items = Vec::new();
    // NSWindowCloseButton, NSWindowMiniaturizeButton, NSWindowZoomButton.
    for kind in 0usize..3 {
        let button: *mut AnyObject = msg_send![window, standardWindowButton: kind];
        if button.is_null() {
            continue;
        }
        let hidden: bool = msg_send![button, isHidden];
        if !hidden {
            items.push(x_range(button));
        }
    }
    let back = TITLES.with(|titles| {
        titles
            .borrow()
            .get(&(window as usize))
            .filter(|parts| parts.back)
            .map(|parts| Retained::as_ptr(&parts.button) as *mut AnyObject)
    });
    if let Some(button) = back {
        items.push(x_range(button));
    }
    let scale: f64 = msg_send![window, backingScaleFactor];
    caption_band(bounds.size.width, height, &items, scale)
}

/// The size of the title bar over `window`'s content, in points: the window's width and the
/// height of the part of the content the title bar covers.
unsafe fn title_bar_points(window: *mut AnyObject) -> Option<(f64, f64)> {
    let mask: usize = msg_send![window, styleMask];
    if mask & STYLE_FULL_SIZE_CONTENT == 0 {
        return None;
    }
    let content: *mut AnyObject = msg_send![window, contentView];
    if content.is_null() {
        return None;
    }
    let bounds: CGRect = msg_send![content, bounds];
    let layout: CGRect = msg_send![window, contentLayoutRect];
    let height = bounds.size.height - (layout.origin.y + layout.size.height);
    (height > 0.0).then_some((bounds.size.width, height))
}

/// Puts the band's fill layer over the top of the content, as high as the title bar.
unsafe fn layout_fill(window: *mut AnyObject) {
    let key = window as usize;
    let Some(layer) = FILLS.with(|fills| fills.borrow().get(&key).cloned()) else {
        return;
    };
    let content: *mut AnyObject = msg_send![window, contentView];
    let Some((width, height)) = title_bar_points(window) else {
        let _: () = msg_send![&*layer, setHidden: true];
        return;
    };
    if content.is_null() {
        return;
    }
    let bounds: CGRect = msg_send![content, bounds];
    let frame = CGRect {
        origin: CGPoint {
            x: 0.0,
            y: bounds.size.height - height,
        },
        size: CGSize { width, height },
    };
    let _: () = msg_send![&*layer, setFrame: frame];
}

/// Shows `look`: the title's tone through the window's appearance, and the band's fill.
unsafe fn apply_look(window: *mut AnyObject, look: Look) {
    if let Ok(appearances) = class(c"NSAppearance") {
        let name = ns_string(appearance_name(look.tone));
        let appearance: *mut AnyObject = msg_send![appearances, appearanceNamed: &*name];
        if !appearance.is_null() {
            let _: () = msg_send![window, setAppearance: appearance];
        }
    }
    let key = window as usize;
    let Some(fill) = look.fill else {
        if let Some(layer) = FILLS.with(|fills| fills.borrow().get(&key).cloned()) {
            let _: () = msg_send![&*layer, setHidden: true];
        }
        return;
    };
    let layer = match FILLS.with(|fills| fills.borrow().get(&key).cloned()) {
        Some(layer) => layer,
        None => {
            let content: *mut AnyObject = msg_send![window, contentView];
            let Some(host) = (unsafe { content.as_ref() }) else {
                return;
            };
            let root: *mut AnyObject = msg_send![host, layer];
            let Ok(layer_class) = class(c"CALayer") else {
                return;
            };
            let made: Option<Retained<AnyObject>> = msg_send![layer_class, new];
            let (Some(made), false) = (made, root.is_null()) else {
                return;
            };
            let _: () = msg_send![&*made, setZPosition: 10.0f64];
            let _: () = msg_send![root, addSublayer: &*made];
            FILLS.with(|fills| fills.borrow_mut().insert(key, made.clone()));
            made
        }
    };
    if let Ok(colors) = class(c"NSColor") {
        let channel = |shift: u32| f64::from((fill >> shift) & 0xFF) / 255.0;
        let color: *mut AnyObject = msg_send![
            colors,
            colorWithSRGBRed: channel(16),
            green: channel(8),
            blue: channel(0),
            alpha: 1.0f64
        ];
        if !color.is_null() {
            let cg: *const std::ffi::c_void = msg_send![color, CGColor];
            let _: () = msg_send![&*layer, setBackgroundColor: cg];
        }
    }
    let _: () = msg_send![&*layer, setHidden: false];
    layout_fill(window);
}

/// Dims the name in the title bar while its window is not the key window, as AppKit dims its own.
unsafe fn dim_title(window: *mut AnyObject, dim: bool) {
    TITLES.with(|titles| {
        if let Some(parts) = titles.borrow().get(&(window as usize)) {
            let alpha: f64 = if dim { 0.5 } else { 1.0 };
            let _: () = msg_send![&*parts.label, setAlphaValue: alpha];
        }
    });
}

/// Tells the window's events and its view the band, when it changed since it was last told.
unsafe fn report_band(window: *mut AnyObject) {
    relayout_title(window);
    layout_fill(window);
    let band = band_of(window);
    let key = window as usize;
    let changed = BANDS.with(|bands| bands.borrow_mut().insert(key, band) != Some(band));
    if !changed {
        return;
    }
    let content: *mut AnyObject = msg_send![window, contentView];
    if let Some(content) = content.as_ref() {
        view::set_band_height(content, band.height);
    }
    with_events(window, |events| events.caption(band));
}

/// The back button's action: the button's window's task goes back.
unsafe extern "C-unwind" fn go_back(_this: &AnyObject, _: Sel, sender: *mut AnyObject) {
    if sender.is_null() {
        return;
    }
    let window: *mut AnyObject = msg_send![sender, window];
    if !window.is_null() {
        with_events(window, |events| events.back_pressed());
    }
}

fn back_target_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let superclass = class(c"NSObject")?;
            let mut builder = ClassBuilder::new(c"DSABackButtonTarget", superclass)
                .ok_or("the back button's target class is already defined")?;
            builder.add_method(
                sel!(goBack:),
                go_back as unsafe extern "C-unwind" fn(_, _, _),
            );
            Ok(builder.register())
        })
        .clone()
}

/// The back button and the name's label, in the title bar the window buttons stand in. The
/// button is a `chevron.backward`, named "Back" in the user's language, and starts hidden.
/// Returns the button and the button's target, which the button holds only weakly.
unsafe fn add_back_button(
    window: &AnyObject,
    title: &str,
) -> Result<(Retained<AnyObject>, Retained<AnyObject>), String> {
    let target: Option<Retained<AnyObject>> = msg_send![back_target_class()?, new];
    let target = target.ok_or("no target for the back button could be made")?;
    let label = ns_string(back_label(crate::ui_language::current()));
    let image: *mut AnyObject = msg_send![
        class(c"NSImage")?,
        imageWithSystemSymbolName: &*ns_string("chevron.backward"),
        accessibilityDescription: &*label
    ];
    let button: *mut AnyObject = msg_send![
        class(c"NSButton")?,
        buttonWithImage: image,
        target: &*target,
        action: sel!(goBack:)
    ];
    if button.is_null() {
        return Err("AppKit could not make the back button".into());
    }
    let _: () = msg_send![button, setBezelStyle: BEZEL_TOOLBAR];
    let _: () = msg_send![button, setShowsBorderOnlyWhileMouseInside: true];
    let _: () = msg_send![button, setToolTip: &*label];
    let _: () = msg_send![button, setAccessibilityLabel: &*label];
    let _: () = msg_send![button, setHidden: true];
    let name: *mut AnyObject = msg_send![
        class(c"NSTextField")?,
        labelWithString: &*ns_string(title)
    ];
    if name.is_null() {
        return Err("AppKit could not make the title's label".into());
    }
    let font: *mut AnyObject = msg_send![
        class(c"NSFont")?,
        systemFontOfSize: 13.0f64,
        weight: 0.3f64
    ];
    if !font.is_null() {
        let _: () = msg_send![name, setFont: font];
    }
    // The tail of a name too long for the window gives way.
    let _: () = msg_send![name, setLineBreakMode: 4usize];
    let close: *mut AnyObject = msg_send![window, standardWindowButton: 0usize];
    let bar: *mut AnyObject = if close.is_null() {
        std::ptr::null_mut()
    } else {
        msg_send![close, superview]
    };
    if bar.is_null() {
        return Err("the window has no title bar to put them in".into());
    }
    let _: () = msg_send![bar, addSubview: button];
    let _: () = msg_send![bar, addSubview: name];
    let button = Retained::retain(button).ok_or("the back button went away")?;
    let name = Retained::retain(name).ok_or("the title's label went away")?;
    TITLES.with(|titles| {
        titles.borrow_mut().insert(
            window as *const AnyObject as usize,
            TitleParts {
                button: button.clone(),
                label: name,
                back: false,
                named: true,
            },
        )
    });
    Ok((button, target))
}

fn with_events(window: *mut AnyObject, f: impl FnOnce(&mut dyn WindowEvents)) {
    // A callback that panics must not unwind into AppKit.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        EVENTS.with(|events| {
            if let Some(events) = events.borrow_mut().get_mut(&(window as usize)) {
                f(events.as_mut());
            }
        })
    }));
}

unsafe fn content_pixels(window: *mut AnyObject) -> ((u32, u32), f64) {
    let view: *mut AnyObject = msg_send![window, contentView];
    let frame: CGRect = msg_send![view, frame];
    let scale: f64 = msg_send![window, backingScaleFactor];
    (
        task_pixels((frame.size.width, frame.size.height), scale),
        scale,
    )
}

unsafe extern "C-unwind" fn did_resize(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    let (pixels, _) = content_pixels(window);
    with_events(window, |events| events.resized(pixels));
    report_band(window);
}

unsafe extern "C-unwind" fn full_screen_changed(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    // Full screen hides the title bar until the pointer reaches the top of the screen, so an
    // app drawing its own title bar has no band to keep clear there.
    let window: *mut AnyObject = msg_send![note, object];
    report_band(window);
}

unsafe extern "C-unwind" fn should_close(
    _this: &AnyObject,
    _: Sel,
    window: *mut AnyObject,
) -> Bool {
    with_events(window, |events| events.close_requested());
    // The window closes when the guest's task finishes, not before.
    Bool::NO
}

unsafe extern "C-unwind" fn did_become_key(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    dim_title(window, false);
    with_events(window, |events| events.focused(true));
}

unsafe extern "C-unwind" fn did_resign_key(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    dim_title(window, true);
    with_events(window, |events| events.focused(false));
}

unsafe extern "C-unwind" fn did_change_backing(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    let (pixels, scale) = content_pixels(window);
    with_events(window, |events| {
        events.scale_changed(scale);
        events.resized(pixels);
    });
    report_band(window);
}

unsafe extern "C-unwind" fn did_miniaturize(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    with_events(window, |events| events.shown(false));
}

unsafe extern "C-unwind" fn did_deminiaturize(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    with_events(window, |events| events.shown(true));
}

unsafe extern "C-unwind" fn live_resize_began(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    with_events(window, |events| events.dragging(true));
}

unsafe extern "C-unwind" fn did_end_live_resize(_this: &AnyObject, _: Sel, note: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![note, object];
    with_events(window, |events| events.dragging(false));
}

fn delegate_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let superclass = class(c"NSObject")?;
            let mut builder = ClassBuilder::new(c"DSATaskWindowDelegate", superclass)
                .ok_or("the window delegate class is already defined")?;
            builder.add_method(
                sel!(windowDidResize:),
                did_resize as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowShouldClose:),
                should_close as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            builder.add_method(
                sel!(windowDidBecomeKey:),
                did_become_key as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowDidResignKey:),
                did_resign_key as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowDidChangeBackingProperties:),
                did_change_backing as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowDidMiniaturize:),
                did_miniaturize as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowDidDeminiaturize:),
                did_deminiaturize as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowWillStartLiveResize:),
                live_resize_began as unsafe extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(windowDidEndLiveResize:),
                did_end_live_resize as unsafe extern "C-unwind" fn(_, _, _),
            );
            for selector in [
                sel!(windowDidEnterFullScreen:),
                sel!(windowDidExitFullScreen:),
            ] {
                builder.add_method(
                    selector,
                    full_screen_changed as unsafe extern "C-unwind" fn(_, _, _),
                );
            }
            Ok(builder.register())
        })
        .clone()
}

/// One task's window.
pub struct TaskWindow {
    window: Retained<AnyObject>,
    view: Retained<AnyObject>,
    /// The window delegate; the window holds it only weakly.
    _delegate: Retained<AnyObject>,
    presenter: Presenter,
    /// The frame to go back to when picture in picture ends.
    before_pip: Option<CGRect>,
    /// The task's size, for picture in picture to keep its shape.
    task: (u32, u32),
    /// The back button and its target, which the button
    /// holds only weakly. `None` when AppKit could not make them.
    _back: Option<(Retained<AnyObject>, Retained<AnyObject>)>,
    /// The frame the window shows now: its texture and the task's part of it.
    shown: Option<(u64, (u32, u32))>,
    /// How large the window shows each of the task's pixels (window pixels per task pixels).
    frame_scale: (u32, u32),
    /// Whether the window shows an app overlay window rather than a task.
    overlay: bool,
}

thread_local! {
    /// Where this app's overlay windows lie on the desktop: one plane for all of them.
    static OVERLAY_PLANE: RefCell<dsa_host_core::compositor::overlay_windows::OverlayPlane> =
        RefCell::new(Default::default());
}

impl TaskWindow {
    /// Opens a window titled `title` for task `task_id` showing a task of `pixels`, centered on the main
    /// display. Must be called on the main thread.
    pub fn open(
        task_id: i32,
        title: &str,
        pixels: (u32, u32),
        events: Box<dyn WindowEvents>,
        input: Box<dyn InputSink>,
    ) -> Result<TaskWindow, String> {
        if !crate::objc::is_main_thread() {
            return Err("a task window can only be opened on the main thread".into());
        }
        unsafe {
            let window_class = class(c"NSWindow")?;
            let screen_class = class(c"NSScreen")?;
            let screen: *mut AnyObject = msg_send![screen_class, mainScreen];
            let scale: f64 = if screen.is_null() {
                1.0
            } else {
                msg_send![screen, backingScaleFactor]
            };
            let points = window_points(pixels, scale);
            let rect = CGRect {
                origin: CGPoint::default(),
                size: CGSize {
                    width: points.0.max(MIN_CONTENT_POINTS.0),
                    height: points.1.max(MIN_CONTENT_POINTS.1),
                },
            };
            let allocated: Allocated<AnyObject> = msg_send![window_class, alloc];
            let window: Option<Retained<AnyObject>> = msg_send![
                allocated,
                initWithContentRect: rect,
                styleMask: STYLE_TITLED | STYLE_CLOSABLE | STYLE_MINIATURIZABLE | STYLE_RESIZABLE
                    | STYLE_FULL_SIZE_CONTENT,
                backing: BACKING_BUFFERED,
                defer: false
            ];
            let window = window.ok_or("AppKit could not make a window")?;
            let _: () = msg_send![&*window, setReleasedWhenClosed: false];
            // The unified title bar: transparent, with the app drawn up to the window's top.
            let _: () = msg_send![&*window, setTitlebarAppearsTransparent: true];
            // The title bar parts draw the name and the back button.
            let _: () = msg_send![&*window, setTitleVisibility: TITLE_HIDDEN];
            let native_title = ns_string(title);
            let _: () = msg_send![&*window, setTitle: &*native_title];
            let min = CGSize {
                width: MIN_CONTENT_POINTS.0,
                height: MIN_CONTENT_POINTS.1,
            };
            let _: () = msg_send![&*window, setContentMinSize: min];
            let _: () = msg_send![&*window, setCollectionBehavior: BEHAVIOR_FULL_SCREEN_PRIMARY];

            let presenter = Presenter::new(scale)?;
            let view = view::make(task_id, rect, presenter.layer(), input)?;
            let _: () = msg_send![&*window, setContentView: &*view];
            let _: () = msg_send![&*window, makeFirstResponder: &*view];

            let delegate_class = delegate_class()?;
            let delegate: Option<Retained<AnyObject>> = msg_send![delegate_class, new];
            let delegate = delegate.ok_or("no window delegate could be made")?;
            let _: () = msg_send![&*window, setDelegate: &*delegate];
            EVENTS.with(|map| {
                map.borrow_mut()
                    .insert(Retained::as_ptr(&window) as usize, events)
            });

            let back = match add_back_button(&window, title) {
                Ok(back) => Some(back),
                Err(why) => {
                    eprintln!("dsa-app-window: task {task_id}'s window has no back button: {why}");
                    None
                }
            };

            let _: () = msg_send![&*window, center];
            let _: () = msg_send![&*window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
            // Tells the guest the band from the start, which also clears what a window of the
            // task said before this window host started.
            report_band(Retained::as_ptr(&window) as *mut AnyObject);
            Ok(TaskWindow {
                window,
                view,
                _delegate: delegate,
                presenter,
                before_pip: None,
                task: pixels,
                _back: back,
                shown: None,
                frame_scale: (1, 1),
                overlay: false,
            })
        }
    }

    /// The `NSWindow`.
    pub fn ns_window(&self) -> &AnyObject {
        &self.window
    }

    pub fn set_title(&self, title: &str) {
        let text = ns_string(title);
        unsafe {
            let _: () = msg_send![&*self.window, setTitle: &*text];
            TITLES.with(|titles| {
                if let Some(parts) = titles.borrow().get(&(self.ptr() as usize)) {
                    let _: () = msg_send![&*parts.label, setStringValue: &*text];
                }
            });
            relayout_title(self.ptr());
        }
    }

    /// Shows one frame of the task.
    pub fn present(&mut self, frame: FrameRef<'_>) -> Result<(), String> {
        if frame.content != (0, 0) {
            self.task = frame.content;
        }
        let content = frame.content;
        let shown = self.presenter.present(frame);
        self.read_band(content, |rows| {
            sample_bgra(
                frame.pixels,
                frame.stride as usize,
                content.0,
                content.1,
                rows,
            )
        });
        shown
    }

    /// Shows the frame in `surface` (texture `texture_id`) on the GPU, without reading it to
    /// memory: `content` pixels from its top left are the task.
    pub fn present_surface(
        &mut self,
        texture_id: u64,
        surface: &super::surface::Surface,
        content: (u32, u32),
    ) -> Result<(), String> {
        if content != (0, 0) {
            self.task = content;
        }
        self.presenter
            .present_surface(texture_id, surface, content)?;
        self.shown = Some((texture_id, content));
        self.read_band(content, |rows| {
            surface
                .read(|pixels, stride| sample_bgra(pixels, stride, content.0, content.1, rows))
                .unwrap_or_default()
        });
        Ok(())
    }

    /// The frame the window shows now: its texture and the task's part of it, from the top
    /// left. `None` before the first frame.
    pub fn shown_frame(&self) -> Option<(u64, (u32, u32))> {
        self.shown
    }

    /// How many client pixels the title bar band covers at the content's top: 0 with a
    /// standard title bar.
    pub fn band_height(&self) -> u32 {
        unsafe { band_of(self.ptr()) }.height
    }

    /// The rows of the task's frame the title bar band covers: [`Self::band_height`] in the
    /// task's pixels, as the window shows the frame at its frame scale. A frame read back is
    /// in the task's pixels, not the window's.
    pub fn band_rows_in_frame(&self) -> u32 {
        super::screenshot::band_rows(self.band_height(), self.frame_scale)
    }

    fn ptr(&self) -> *mut AnyObject {
        Retained::as_ptr(&self.window) as *mut AnyObject
    }

    /// The window's title bar: the unified one (transparent, with the app under it and the
    /// window's name in it), or, when the app draws its own title bar (`app_drawn`), the same
    /// without the name, with the app's `controls` in the band (client pixels) taking the
    /// pointer while the rest of it drags the window. An app set to start `below` the title
    /// bar gets AppKit's standard title bar above it. The content's new size and the band
    /// are reported.
    pub fn set_title_bar(&mut self, app_drawn: bool, below: bool, controls: Vec<Bounds>) {
        let chrome = Chrome::of(app_drawn, below);
        unsafe {
            let mask: usize = msg_send![&*self.window, styleMask];
            let was = mask & STYLE_FULL_SIZE_CONTENT != 0;
            let drawn = chrome == Chrome::AppDrawn;
            view::set_controls(&self.view, if drawn { controls } else { Vec::new() });
            // The unified title bar draws the name itself, beside the back button.
            let visibility = if chrome == Chrome::Standard {
                TITLE_VISIBLE
            } else {
                TITLE_HIDDEN
            };
            TITLES.with(|titles| {
                if let Some(parts) = titles.borrow_mut().get_mut(&(self.ptr() as usize)) {
                    parts.named = chrome == Chrome::Unified;
                }
            });
            let _: () = msg_send![&*self.window, setTitleVisibility: visibility];
            if was != chrome.full_size_content() {
                let mask = if chrome.full_size_content() {
                    mask | STYLE_FULL_SIZE_CONTENT
                } else {
                    mask & !STYLE_FULL_SIZE_CONTENT
                };
                // The window keeps its frame; its content grows under the title bar or
                // shrinks out from under it.
                let frame: CGRect = msg_send![&*self.window, frame];
                let _: () = msg_send![&*self.window, setStyleMask: mask];
                let _: () = msg_send![
                    &*self.window,
                    setTitlebarAppearsTransparent: chrome.full_size_content()
                ];
                let _: () = msg_send![&*self.window, setFrame: frame, display: true];
                let window = self.ptr();
                let (pixels, _) = content_pixels(window);
                with_events(window, |events| events.resized(pixels));
            }
            let window = self.ptr();
            report_band(window);
            let look = TONES.with(|tones| {
                tones
                    .borrow_mut()
                    .entry(window as usize)
                    .or_default()
                    .set_app_drawn(drawn)
            });
            if let Some(look) = look {
                apply_look(window, look);
            }
        }
    }

    /// The tone the app asked for its title bar's text and glyphs, or `None` when it said
    /// nothing, so the window reads it from the frames.
    pub fn set_caption_tone(&self, tone: Option<CaptionTone>) {
        let window = self.ptr();
        let look = TONES.with(|tones| {
            tones
                .borrow_mut()
                .entry(window as usize)
                .or_default()
                .set_reported(tone)
        });
        if let Some(look) = look {
            unsafe { apply_look(window, look) };
        }
    }

    /// Reads the band of the frame just shown (`content` task pixels of it, `read` giving its
    /// BGRA rows and their stride) and shows the tone and fill it gives.
    fn read_band(&self, content: (u32, u32), read: impl FnOnce(u32) -> BandSample) {
        let window = self.ptr();
        let band = BANDS.with(|bands| bands.borrow().get(&(window as usize)).map(|b| b.height));
        let rows = super::screenshot::band_rows(band.unwrap_or(0), self.frame_scale);
        if rows == 0 || content == (0, 0) {
            return;
        }
        let look = TONES.with(|tones| {
            tones
                .borrow_mut()
                .entry(window as usize)
                .or_default()
                .set_sample(read(rows))
        });
        if let Some(look) = look {
            unsafe { apply_look(window, look) };
        }
    }

    /// Shows the back button while the task can go back, and hides it on its first screen.
    pub fn set_back(&self, back: bool) {
        unsafe {
            TITLES.with(|titles| {
                if let Some(parts) = titles.borrow_mut().get_mut(&(self.ptr() as usize)) {
                    parts.back = back;
                }
            });
            report_band(self.ptr());
        }
    }

    /// Whether the window is minimized into the Dock.
    pub fn is_minimized(&self) -> bool {
        unsafe { msg_send![&*self.window, isMiniaturized] }
    }

    /// The display's backing scale changed.
    pub fn set_scale(&mut self, scale: f64) {
        self.presenter.set_scale(scale);
    }

    /// The window shows each of its task's pixels at `scale` (window pixels per task pixels)
    /// from now on, and measures pointer input at it: the task keeps its density on a
    /// display of another scale.
    pub fn set_frame_scale(&mut self, scale: (u32, u32)) {
        if self.frame_scale == scale {
            return;
        }
        self.frame_scale = scale;
        self.presenter.set_frame_scale(scale);
        view::set_frame_scale(&self.view, scale);
    }

    /// Gives the window's content `pixels`, the size that shows its task at the window's
    /// scale, as when the guest itself resizes its task.
    pub fn resize_to(&mut self, pixels: (u32, u32)) {
        self.task = to_task_size(pixels, self.frame_scale);
        unsafe {
            let scale: f64 = msg_send![&*self.window, backingScaleFactor];
            let points = window_points(pixels, scale);
            let size = CGSize {
                width: points.0,
                height: points.1,
            };
            let _: () = msg_send![&*self.window, setContentSize: size];
        }
    }

    /// Enters or leaves full screen in its own space.
    pub fn toggle_full_screen(&self) {
        unsafe {
            let _: () = msg_send![&*self.window, toggleFullScreen: std::ptr::null::<AnyObject>()];
        }
    }

    /// Whether the window is in picture in picture.
    pub fn is_picture_in_picture(&self) -> bool {
        self.before_pip.is_some()
    }

    /// Makes the window the window of an app overlay window (floating lyrics, a chat head):
    /// no title bar and no buttons, since its app decides when it goes; above the other
    /// windows on every space; and see-through where its app drew nothing. It stays so.
    /// Moving it by a handle and remembering its place are not on macOS yet.
    pub fn set_overlay(&mut self) {
        if std::mem::replace(&mut self.overlay, true) {
            return;
        }
        unsafe {
            let _: () = msg_send![&*self.window, setStyleMask: STYLE_BORDERLESS];
            let _: () = msg_send![&*self.window, setLevel: LEVEL_FLOATING];
            let _: () =
                msg_send![&*self.window, setCollectionBehavior: BEHAVIOR_CAN_JOIN_ALL_SPACES];
            let _: () = msg_send![&*self.window, setOpaque: false];
            if let Ok(colors) = class(c"NSColor") {
                let clear: *mut AnyObject = msg_send![colors, clearColor];
                let _: () = msg_send![&*self.window, setBackgroundColor: clear];
            }
            let _: () = msg_send![&*self.window, setHasShadow: false];
            let smallest = CGSize {
                width: 1.0,
                height: 1.0,
            };
            let _: () = msg_send![&*self.window, setContentMinSize: smallest];
        }
    }

    /// Puts an overlay's window where its app has the overlay: `guest` is the overlay's top
    /// left corner on the guest's display and `display` the display's size, in guest pixels.
    /// All of the app's overlays share one plane, first put where the overlay lies on the
    /// guest's display, and a move by the app moves the window the same distance at its
    /// scale. The pointer passes through the window of an overlay that takes no input.
    pub fn place_overlay(&mut self, guest: (i32, i32), display: (u32, u32), touchable: bool) {
        use dsa_host_core::compositor::overlay_windows::{first_place, kept_on};
        unsafe {
            let _: () = msg_send![&*self.window, setIgnoresMouseEvents: !touchable];
            let mut screen: *mut AnyObject = msg_send![&*self.window, screen];
            if screen.is_null() {
                let Ok(screens) = class(c"NSScreen") else {
                    return;
                };
                screen = msg_send![screens, mainScreen];
            }
            if screen.is_null() {
                return;
            }
            let visible: CGRect = msg_send![screen, visibleFrame];
            let backing: f64 = msg_send![&*self.window, backingScaleFactor];
            let backing = if backing > 0.0 { backing } else { 1.0 };
            let frame: CGRect = msg_send![&*self.window, frame];
            // In pixels from the visible frame's top left corner, as the rules count.
            let pixels = |points: f64| (points * backing).round();
            let area = (
                0,
                0,
                pixels(visible.size.width) as i32,
                pixels(visible.size.height) as i32,
            );
            let size = (
                pixels(frame.size.width).max(1.0) as u32,
                pixels(frame.size.height).max(1.0) as u32,
            );
            let (scale, task) = (self.frame_scale, self.task);
            let at = OVERLAY_PLANE.with(|plane| {
                let mut plane = plane.borrow_mut();
                let wanted = plane.place(guest, scale, || {
                    first_place(guest, task, display, size, area)
                });
                let at = kept_on(wanted, size, area);
                if at != wanted {
                    plane.moved_to(guest, at);
                }
                at
            });
            // AppKit counts from the bottom left.
            let origin = CGPoint {
                x: visible.origin.x + f64::from(at.0) / backing,
                y: visible.origin.y + visible.size.height
                    - f64::from(at.1) / backing
                    - frame.size.height,
            };
            let _: () = msg_send![&*self.window, setFrameOrigin: origin];
        }
    }

    /// Picture in picture: a small window in the screen's corner that floats above the
    /// others on every space, keeping the task's shape; or back to where it was.
    pub fn set_picture_in_picture(&mut self, on: bool) {
        unsafe {
            match (on, self.before_pip) {
                (true, None) => {
                    let frame: CGRect = msg_send![&*self.window, frame];
                    let screen: *mut AnyObject = msg_send![&*self.window, screen];
                    if screen.is_null() {
                        return;
                    }
                    let visible: CGRect = msg_send![screen, visibleFrame];
                    let rect = picture_in_picture(
                        Rect {
                            x: visible.origin.x,
                            y: visible.origin.y,
                            width: visible.size.width,
                            height: visible.size.height,
                        },
                        self.task,
                    );
                    let content = CGRect {
                        origin: CGPoint {
                            x: rect.x,
                            y: rect.y,
                        },
                        size: CGSize {
                            width: rect.width,
                            height: rect.height,
                        },
                    };
                    let target: CGRect = msg_send![&*self.window, frameRectForContentRect: content];
                    let _: () = msg_send![&*self.window, setLevel: LEVEL_FLOATING];
                    let _: () = msg_send![
                        &*self.window,
                        setCollectionBehavior: BEHAVIOR_FULL_SCREEN_PRIMARY | BEHAVIOR_CAN_JOIN_ALL_SPACES
                    ];
                    let aspect = CGSize {
                        width: f64::from(self.task.0.max(1)),
                        height: f64::from(self.task.1.max(1)),
                    };
                    let _: () = msg_send![&*self.window, setContentAspectRatio: aspect];
                    let _: () =
                        msg_send![&*self.window, setFrame: target, display: true, animate: true];
                    self.before_pip = Some(frame);
                }
                (false, Some(frame)) => {
                    let _: () = msg_send![&*self.window, setLevel: LEVEL_NORMAL];
                    let _: () = msg_send![&*self.window, setCollectionBehavior: BEHAVIOR_FULL_SCREEN_PRIMARY];
                    // Resize increments and an aspect ratio exclude each other, so setting
                    // increments of one point lifts the aspect lock.
                    let free = CGSize {
                        width: 1.0,
                        height: 1.0,
                    };
                    let _: () = msg_send![&*self.window, setContentResizeIncrements: free];
                    let _: () =
                        msg_send![&*self.window, setFrame: frame, display: true, animate: true];
                    self.before_pip = None;
                }
                _ => {}
            }
        }
    }

    /// The smallest and largest the window's content may be, in client pixels.
    pub fn set_limits(&self, min: Option<(u32, u32)>, max: Option<(u32, u32)>) {
        unsafe {
            let scale: f64 = msg_send![&*self.window, backingScaleFactor];
            let min = min.map(|m| window_points(m, scale)).unwrap_or((0.0, 0.0));
            let min = CGSize {
                width: min.0.max(MIN_CONTENT_POINTS.0),
                height: min.1.max(MIN_CONTENT_POINTS.1),
            };
            let max = max
                .map(|m| window_points(m, scale))
                .unwrap_or((f64::MAX, f64::MAX));
            let max = CGSize {
                width: max.0.max(min.width),
                height: max.1.max(min.height),
            };
            let _: () = msg_send![&*self.window, setContentMinSize: min];
            let _: () = msg_send![&*self.window, setContentMaxSize: max];
        }
    }

    /// Whether the user may resize the window: an app that asks for a fixed size gets a
    /// window without the resize edges and the zoom button.
    pub fn set_resizable(&self, resizable: bool) {
        unsafe {
            let mask: usize = msg_send![&*self.window, styleMask];
            let mask = if resizable {
                mask | STYLE_RESIZABLE
            } else {
                mask & !STYLE_RESIZABLE
            };
            let _: () = msg_send![&*self.window, setStyleMask: mask];
        }
    }

    /// Whether the window is in full screen now.
    pub fn is_full_screen(&self) -> bool {
        const STYLE_FULL_SCREEN: usize = 1 << 14;
        let mask: usize = unsafe { msg_send![&*self.window, styleMask] };
        mask & STYLE_FULL_SCREEN != 0
    }

    /// Enters or leaves full screen, as the app asks.
    pub fn set_full_screen(&self, on: bool) {
        if self.is_full_screen() != on {
            self.toggle_full_screen();
        }
    }

    /// Brings the window to the front and gives it the keyboard.
    pub fn activate(&self) {
        unsafe {
            let _: () =
                msg_send![&*self.window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        }
    }

    /// The input view, for the input method's cursor rectangle and for drag and drop.
    pub fn view(&self) -> &AnyObject {
        &self.view
    }
}

impl Drop for TaskWindow {
    fn drop(&mut self) {
        let window = Retained::as_ptr(&self.window) as usize;
        EVENTS.with(|map| map.borrow_mut().remove(&window));
        BANDS.with(|map| map.borrow_mut().remove(&window));
        TONES.with(|map| map.borrow_mut().remove(&window));
        FILLS.with(|map| map.borrow_mut().remove(&window));
        TITLES.with(|map| map.borrow_mut().remove(&window));
        view::forget(&self.view);
        unsafe {
            let _: () = msg_send![&*self.window, setDelegate: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.window, close];
        }
    }
}

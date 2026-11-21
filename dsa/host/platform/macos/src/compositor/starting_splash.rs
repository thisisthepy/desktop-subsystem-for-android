//! The window an app's window host shows while Android starts: the DSA logo (the app icon the
//! DSA bundle carries) centred on the splash background of the user's appearance, breathing
//! slowly, until the app's own window opens; then it fades out over it.
//!
//! The timing and the sizes are `dsa_host_core::compositor::boot_splash`, shared with every
//! host. Core Animation runs the breath: one repeating opacity animation on the logo's layer,
//! so the process draws nothing per frame, and macOS stops it while the window is hidden or
//! minimized. With "Reduce motion" on the logo stands still and the window goes at once. The
//! window's visible text is gone; its accessibility label still says Android is starting.

use std::cell::RefCell;

use dsa_host_core::compositor::boot_splash;
use dsa_host_core::compositor::splash;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;

use crate::objc::{class, ns_data, ns_string, CGPoint, CGRect, CGSize};

/// The DSA logo as the DSA bundle carries it for its icon.
const LOGO_ICNS: &[u8] = include_bytes!("../../packaging/icon/DSA.icns");

/// `NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable`.
const STYLE: usize = 1 | 2 | 4;

/// The largest logo image the bundle carries, in pixels on a side.
const LOGO_SOURCE: u32 = 1024;

thread_local! {
    static SHOWN: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// Whether the user turned on "Reduce motion".
unsafe fn reduce_motion() -> bool {
    let Ok(workspace) = class(c"NSWorkspace") else {
        return false;
    };
    let shared: *mut AnyObject = msg_send![workspace, sharedWorkspace];
    if shared.is_null() {
        return false;
    }
    msg_send![shared, accessibilityDisplayShouldReduceMotion]
}

/// Whether the user's appearance is dark.
unsafe fn dark() -> bool {
    (|| -> Option<bool> {
        let app: *mut AnyObject = msg_send![class(c"NSApplication").ok()?, sharedApplication];
        let appearance: *mut AnyObject = msg_send![app, effectiveAppearance];
        if appearance.is_null() {
            return None;
        }
        let name: *mut AnyObject = msg_send![appearance, name];
        Some(crate::objc::string_of(name).contains("Dark"))
    })()
    .unwrap_or(false)
}

/// A `CGColor` of `0xAARRGGBB`, opaque.
unsafe fn cg_color(argb: u32) -> Option<*mut AnyObject> {
    let channel = |shift: u32| f64::from((argb >> shift) & 0xFF) / 255.0;
    let color: *mut AnyObject = msg_send![
        class(c"NSColor").ok()?,
        colorWithSRGBRed: channel(16),
        green: channel(8),
        blue: channel(0),
        alpha: 1.0f64
    ];
    if color.is_null() {
        return None;
    }
    let cg: *mut AnyObject = msg_send![color, CGColor];
    Some(cg)
}

/// Shows the window, titled `title`, on the main thread. It stays until [`dismiss`].
pub fn show(title: &str) {
    // SAFETY: AppKit and Core Animation calls on the main thread, on objects this keeps alive.
    let shown = unsafe { open(title) };
    match shown {
        Ok(window) => SHOWN.with(|cell| *cell.borrow_mut() = Some(window)),
        Err(why) => eprintln!("dsa-app-window: the starting window could not be shown: {why}"),
    }
}

unsafe fn open(title: &str) -> Result<Retained<AnyObject>, String> {
    let app: *mut AnyObject = msg_send![class(c"NSApplication")?, sharedApplication];
    let (width, height) = boot_splash::WINDOW_DIP;
    let rect = CGRect {
        origin: CGPoint::default(),
        size: CGSize {
            width: f64::from(width),
            height: f64::from(height),
        },
    };
    let allocated: Allocated<AnyObject> = msg_send![class(c"NSWindow")?, alloc];
    let window: Option<Retained<AnyObject>> = msg_send![
        allocated,
        initWithContentRect: rect,
        styleMask: STYLE,
        backing: 2usize,
        defer: false
    ];
    let window = window.ok_or("AppKit could not make the starting window")?;
    let _: () = msg_send![&*window, setReleasedWhenClosed: false];
    let _: () = msg_send![&*window, setTitle: &*ns_string(title)];
    let scale: f64 = msg_send![&*window, backingScaleFactor];
    let scale = if scale > 0.0 { scale } else { 1.0 };

    let background = splash::background(dark());
    let content: *mut AnyObject = msg_send![&*window, contentView];
    if content.is_null() {
        return Err("the starting window has no content view".into());
    }
    let _: () = msg_send![content, setWantsLayer: true];
    let root: *mut AnyObject = msg_send![content, layer];
    if root.is_null() {
        return Err("the starting window's view has no layer".into());
    }
    if let Some(color) = cg_color(background) {
        let _: () = msg_send![root, setBackgroundColor: color];
    }
    // Screen readers hear what the window waits for; nothing of it is drawn.
    let label = ns_string(dsa_host_core::ui_text::caption::starting_android(
        crate::ui_language::current(),
    ));
    let _: () = msg_send![content, setAccessibilityLabel: &*label];
    let _: () = msg_send![&*window, setAccessibilityLabel: &*label];

    // The logo, the size the app's own splash shows its icon at.
    let dpi = (96.0 * scale).round() as u32;
    let pixels = (
        (f64::from(width) * scale) as u32,
        (f64::from(height) * scale) as u32,
    );
    let edge = f64::from(boot_splash::logo_edge(pixels, dpi, LOGO_SOURCE)) / scale;
    let image: Allocated<AnyObject> = msg_send![class(c"NSImage")?, alloc];
    let image: Option<Retained<AnyObject>> = msg_send![image, initWithData: &*ns_data(LOGO_ICNS)?];
    if let Some(image) = image {
        let logo: *mut AnyObject = msg_send![class(c"CALayer")?, layer];
        if !logo.is_null() {
            let frame = CGRect {
                origin: CGPoint {
                    x: (f64::from(width) - edge) / 2.0,
                    y: (f64::from(height) - edge) / 2.0,
                },
                size: CGSize {
                    width: edge,
                    height: edge,
                },
            };
            let _: () = msg_send![logo, setFrame: frame];
            let _: () = msg_send![logo, setContentsScale: scale];
            let _: () = msg_send![logo, setContentsGravity: &*ns_string("resizeAspect")];
            let _: () = msg_send![logo, setContents: &*image];
            if !reduce_motion() {
                breathe(logo)?;
            }
            let _: () = msg_send![root, addSublayer: logo];
        }
    }

    let _: () = msg_send![&*window, center];
    let _: () = msg_send![&*window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
    if !app.is_null() {
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
    }
    Ok(window)
}

/// The logo's breath: from full opacity to the shared floor and back, half a period each way,
/// eased in and out, for as long as the window is there.
unsafe fn breathe(logo: *mut AnyObject) -> Result<(), String> {
    let animation: *mut AnyObject = msg_send![
        class(c"CABasicAnimation")?,
        animationWithKeyPath: &*ns_string("opacity")
    ];
    if animation.is_null() {
        return Ok(());
    }
    let from: *mut AnyObject = msg_send![class(c"NSNumber")?, numberWithFloat: 1.0f32];
    let to: *mut AnyObject = msg_send![
        class(c"NSNumber")?,
        numberWithFloat: boot_splash::DIM_OPACITY
    ];
    let _: () = msg_send![animation, setFromValue: from];
    let _: () = msg_send![animation, setToValue: to];
    let _: () = msg_send![
        animation,
        setDuration: boot_splash::PULSE_PERIOD.as_secs_f64() / 2.0
    ];
    let _: () = msg_send![animation, setAutoreverses: true];
    let _: () = msg_send![animation, setRepeatCount: f32::INFINITY];
    let timing: *mut AnyObject = msg_send![
        class(c"CAMediaTimingFunction")?,
        functionWithName: &*ns_string("easeInEaseOut")
    ];
    let _: () = msg_send![animation, setTimingFunction: timing];
    let _: () = msg_send![logo, addAnimation: animation, forKey: &*ns_string("breath")];
    Ok(())
}

/// Fades the window out over the app's window, which has just opened, and lets it go.
/// Nothing of it is left afterwards. Does nothing when it is not shown.
pub fn dismiss() {
    let Some(window) = SHOWN.with(|cell| cell.borrow_mut().take()) else {
        return;
    };
    // SAFETY: AppKit calls on the main thread on a window this held.
    unsafe {
        let reduce = reduce_motion();
        let fade = boot_splash::CROSSFADE.as_secs_f64();
        if reduce {
            let _: () = msg_send![&*window, orderOut: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*window, close];
            return;
        }
        if let Ok(context) = class(c"NSAnimationContext") {
            let _: () = msg_send![context, beginGrouping];
            let current: *mut AnyObject = msg_send![context, currentContext];
            if !current.is_null() {
                let _: () = msg_send![current, setDuration: fade];
            }
            let animator: *mut AnyObject = msg_send![&*window, animator];
            if !animator.is_null() {
                let _: () = msg_send![animator, setAlphaValue: 0.0f64];
            }
            let _: () = msg_send![context, endGrouping];
        }
        // Closed once the fade is over; the run loop holds the window until then.
        let close: Sel = sel!(close);
        let _: () = msg_send![
            &*window,
            performSelector: close,
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: fade
        ];
    }
}

/// Runs the main thread's events for at most `seconds`, so the window stays responsive while
/// the launch waits for Android.
pub fn pump(seconds: f64) {
    // SAFETY: AppKit calls on the main thread.
    unsafe {
        let Ok(app_class) = class(c"NSApplication") else {
            return;
        };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        let Ok(date_class) = class(c"NSDate") else {
            return;
        };
        let until: *mut AnyObject = msg_send![date_class, dateWithTimeIntervalSinceNow: seconds];
        let mode = ns_string("kCFRunLoopDefaultMode");
        loop {
            let event: *mut AnyObject = msg_send![
                app,
                nextEventMatchingMask: u64::MAX,
                untilDate: until,
                inMode: &*mode,
                dequeue: true
            ];
            if event.is_null() {
                break;
            }
            let _: () = msg_send![app, sendEvent: event];
        }
    }
}

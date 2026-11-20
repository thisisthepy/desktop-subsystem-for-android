//! The small part of the Objective-C runtime the macOS host uses: classes by name, strings,
//! the geometry structures the frameworks pass by value, and work handed to the main thread.
//!
//! Every framework class is looked up at run time by name. A class that is missing (an older
//! macOS) is an error the caller reports, never a crash.

use std::ffi::{c_char, c_void, CStr, CString};

use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};

#[link(name = "Foundation", kind = "framework")]
extern "C" {}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}
#[link(name = "Metal", kind = "framework")]
extern "C" {}

extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn pthread_main_np() -> i32;
}

/// A point, in the frameworks' points.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

/// A size, in the frameworks' points.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

/// A rectangle: its origin (bottom left in AppKit) and its size.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// A range of UTF-16 code units, as `NSRange`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NSRange {
    pub location: usize,
    pub length: usize,
}

/// `NSNotFound`, the location of a range that is no range.
pub const NOT_FOUND: usize = isize::MAX as usize;

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl RefEncode for CGPoint {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl RefEncode for CGSize {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}
unsafe impl RefEncode for CGRect {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for NSRange {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}
unsafe impl RefEncode for NSRange {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

/// The class named `name`, or why it cannot be used.
pub fn class(name: &CStr) -> Result<&'static AnyClass, String> {
    AnyClass::get(name).ok_or_else(|| {
        format!(
            "this macOS has no {} class, which the host needs",
            name.to_string_lossy()
        )
    })
}

/// An `NSString` holding `text`. A NUL in `text`, which `NSString` cannot take from a C
/// string, becomes U+FFFD.
pub fn ns_string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text.replace('\0', "\u{fffd}")).unwrap_or_default();
    let class = class(c"NSString").expect("Foundation always has NSString");
    let string: Option<Retained<AnyObject>> =
        unsafe { msg_send![class, stringWithUTF8String: text.as_ptr()] };
    string.expect("an NSString from valid UTF-8")
}

/// The text of an `NSString`, or of an `NSAttributedString` (its `string`). Empty for nil.
// The pointer is an Objective-C object Cocoa handed to a callback, or nil, which is checked;
// messaging it is how every caller reads it, so the callers are not made unsafe.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn string_of(object: *mut AnyObject) -> String {
    if object.is_null() {
        return String::new();
    }
    unsafe {
        let attributed = class(c"NSAttributedString").ok();
        let object: *mut AnyObject = match attributed {
            Some(attributed) if msg_send![object, isKindOfClass: attributed] => {
                msg_send![object, string]
            }
            _ => object,
        };
        if object.is_null() {
            return String::new();
        }
        let utf8: *const c_char = msg_send![object, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }
}

/// An `NSURL` for a file path.
pub fn file_url(path: &std::path::Path) -> Result<Retained<AnyObject>, String> {
    let class = class(c"NSURL")?;
    let path = ns_string(&path.to_string_lossy());
    let url: Option<Retained<AnyObject>> = unsafe { msg_send![class, fileURLWithPath: &*path] };
    url.ok_or_else(|| "the path does not make a file URL".to_string())
}

/// An `NSData` holding a copy of `bytes`.
pub fn ns_data(bytes: &[u8]) -> Result<Retained<AnyObject>, String> {
    let class = class(c"NSData")?;
    let data: Option<Retained<AnyObject>> = unsafe {
        msg_send![class, dataWithBytes: bytes.as_ptr() as *const c_void, length: bytes.len()]
    };
    data.ok_or_else(|| "out of memory for a copy of the data".to_string())
}

/// The bytes an `NSData` holds.
// The pointer is an Objective-C object Cocoa handed to a callback, or nil, which is checked;
// messaging it is how every caller reads it, so the callers are not made unsafe.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn bytes_of(data: *mut AnyObject) -> Vec<u8> {
    if data.is_null() {
        return Vec::new();
    }
    unsafe {
        let length: usize = msg_send![data, length];
        let bytes: *const c_void = msg_send![data, bytes];
        if bytes.is_null() || length == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(bytes as *const u8, length).to_vec()
    }
}

/// Whether this is the process's main thread, the only one AppKit may be used from.
pub fn is_main_thread() -> bool {
    unsafe { pthread_main_np() == 1 }
}

extern "C" fn run_boxed(context: *mut c_void) {
    let work = unsafe { Box::from_raw(context as *mut Box<dyn FnOnce() + Send>) };
    // A panic in one piece of window work must not unwind into the run loop and take the
    // whole host down; it is reported and the run loop goes on.
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
        eprintln!("dsa: a piece of window work failed and was skipped");
    }
}

/// Runs `work` on the main thread: now if this is the main thread, otherwise on its next turn
/// of the run loop.
pub fn on_main(work: impl FnOnce() + Send + 'static) {
    if is_main_thread() {
        work();
        return;
    }
    let boxed: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(work));
    unsafe {
        dispatch_async_f(
            &_dispatch_main_q as *const c_void,
            Box::into_raw(boxed) as *mut c_void,
            run_boxed,
        );
    }
}

/// Brings this background program's panel (share, print, passkeys) to the front while it
/// stays an accessory app: no Dock icon and no menu bar. `app` is `NSApplication.shared`.
/// The policy is set again around the activation, so nothing that ran in between (a privacy
/// prompt, a framework's own activation) leaves the program a regular app, which macOS would
/// show in the Dock as a second DSA.
///
/// # Safety
///
/// `app` is the shared NSApplication or null, and this runs on the main thread.
pub unsafe fn activate_as_accessory(app: *mut AnyObject) {
    if app.is_null() {
        return;
    }
    // NSApplicationActivationPolicyAccessory.
    let _: bool = msg_send![app, setActivationPolicy: 1isize];
    let _: () = msg_send![app, activateIgnoringOtherApps: true];
    let _: bool = msg_send![app, setActivationPolicy: 1isize];
}

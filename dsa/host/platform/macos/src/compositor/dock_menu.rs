//! The window host's Dock menu: the app's launcher shortcuts (a messenger's "New chat"),
//! above the Dock's own items, as a launcher shows them on a long press.
//!
//! The shortcuts are the ones the window manager last wrote into the app's bundle
//! ([`crate::shell::app_stubs::SHORTCUTS_KEY`]). The bundle's `Info.plist` is read again each
//! time the menu opens, rather than the copy macOS loaded when the program started, so a
//! shortcut the app published since is there. Choosing one asks the service to start it in
//! Android, whose task then opens in a window of this program.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use dsa_host_core::host_api::HostApiClient;
use dsa_host_unix::socket::UnixLocalConnector;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::sel;

use super::window_host::BUILD;
use crate::objc::{class, ns_string, string_of};
use crate::shell::app_stubs::{shortcuts_in, StubShortcut};

/// How long the request to start a shortcut may take.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The app the menu is for, and the service's socket.
struct Place {
    package: String,
    host_api: PathBuf,
}

static PLACE: OnceLock<Place> = OnceLock::new();

/// The shortcuts the menu last showed, by their items' tags.
static SHOWN: Mutex<Vec<StubShortcut>> = Mutex::new(Vec::new());

/// The shortcuts listed in this program's bundle now.
fn shortcuts() -> Vec<StubShortcut> {
    let path = unsafe {
        let Ok(bundle_class) = class(c"NSBundle") else {
            return Vec::new();
        };
        let bundle: *mut AnyObject = msg_send![bundle_class, mainBundle];
        if bundle.is_null() {
            return Vec::new();
        }
        string_of(msg_send![bundle, bundlePath])
    };
    if path.is_empty() {
        return Vec::new();
    }
    std::fs::read_to_string(PathBuf::from(path).join("Contents/Info.plist"))
        .map(|plist| shortcuts_in(&plist))
        .unwrap_or_default()
}

/// Asks the service to start the shortcut `id` of the app, off the main thread.
fn start(id: String) {
    let Some(place) = PLACE.get() else {
        return;
    };
    let (package, host_api) = (place.package.clone(), place.host_api.clone());
    let _ = std::thread::Builder::new()
        .name("dsa-app-window-shortcut".into())
        .spawn(move || {
            let mut connector = UnixLocalConnector { path: host_api };
            let started = HostApiClient::connect(&mut connector, BUILD, TIMEOUT)
                .and_then(|mut client| client.launch_shortcut(&package, &id))
                .map_err(|e| e.to_string());
            if let Err(why) = started {
                eprintln!("dsa-app-window: the shortcut {id:?} of {package} did not start: {why}");
            }
        });
}

unsafe extern "C-unwind" fn dock_menu(
    _this: &AnyObject,
    _: Sel,
    _app: *mut AnyObject,
) -> *mut AnyObject {
    let built = std::panic::catch_unwind(|| unsafe { build_menu() });
    match built {
        Ok(Some(menu)) => Retained::autorelease_return(menu),
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C-unwind" fn choose(_this: &AnyObject, _: Sel, sender: *mut AnyObject) {
    if sender.is_null() {
        return;
    }
    let tag: isize = msg_send![sender, tag];
    let chosen = SHOWN.lock().ok().and_then(|shown| {
        usize::try_from(tag)
            .ok()
            .and_then(|at| shown.get(at).cloned())
    });
    if let Some(shortcut) = chosen {
        let _ = std::panic::catch_unwind(|| start(shortcut.id));
    }
}

/// The menu of the shortcuts, or `None` when the app has none.
unsafe fn build_menu() -> Option<Retained<AnyObject>> {
    let listed = shortcuts();
    if listed.is_empty() {
        return None;
    }
    let target = DELEGATE.with(|delegate| delegate.borrow().clone())?;
    let allocated: Allocated<AnyObject> = msg_send![class(c"NSMenu").ok()?, alloc];
    let menu: Option<Retained<AnyObject>> = msg_send![allocated, initWithTitle: &*ns_string("")];
    let menu = menu?;
    for (index, shortcut) in listed.iter().enumerate() {
        let allocated: Allocated<AnyObject> = msg_send![class(c"NSMenuItem").ok()?, alloc];
        let item: Option<Retained<AnyObject>> = msg_send![
            allocated,
            initWithTitle: &*ns_string(&shortcut.label),
            action: Some(sel!(chooseShortcut:)),
            keyEquivalent: &*ns_string("")
        ];
        let Some(item) = item else { continue };
        let _: () = msg_send![&*item, setTarget: &*target];
        let _: () = msg_send![&*item, setTag: index as isize];
        let _: () = msg_send![&*menu, addItem: &*item];
    }
    if let Ok(mut shown) = SHOWN.lock() {
        *shown = listed;
    }
    Some(menu)
}

fn delegate_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let mut builder = ClassBuilder::new(c"DSAAppWindowDelegate", class(c"NSObject")?)
                .ok_or("the window host's application delegate class is already defined")?;
            builder.add_method(
                sel!(applicationDockMenu:),
                dock_menu as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            builder.add_method(
                sel!(chooseShortcut:),
                choose as unsafe extern "C-unwind" fn(_, _, _),
            );
            Ok(builder.register())
        })
        .clone()
}

thread_local! {
    /// The application's delegate, which AppKit holds only weakly.
    static DELEGATE: std::cell::RefCell<Option<Retained<AnyObject>>> =
        const { std::cell::RefCell::new(None) };
}

/// Gives the application a delegate whose Dock menu lists `package`'s shortcuts, started
/// through the service at `host_api`. On the main thread, before the application runs.
pub fn install(package: &str, host_api: PathBuf) -> Result<(), String> {
    let _ = PLACE.set(Place {
        package: package.to_string(),
        host_api,
    });
    unsafe {
        let delegate: Option<Retained<AnyObject>> = msg_send![delegate_class()?, new];
        let delegate = delegate.ok_or("no application delegate could be made")?;
        let app: *mut AnyObject = msg_send![class(c"NSApplication")?, sharedApplication];
        let _: () = msg_send![app, setDelegate: &*delegate];
        DELEGATE.with(|held| *held.borrow_mut() = Some(delegate));
    }
    Ok(())
}

//! "Share on PC": what apps in the user's Android share, in the Mac's share menu.
//!
//! The follower watches the shares over the local API ([`dsa_host_core::share::serve`]),
//! copies the shared files into the user's caches folder, and opens the Mac's share menu
//! (`NSSharingServicePicker`) with the same text, links and files, so the user can send them
//! with AirDrop, Mail, Messages or any app that shares. The menu needs a view to open from:
//! a small window of the lender's own, which cannot be seen, in the middle of the main screen.
//! Whether the user picked a way to share or closed the menu goes back to Android.

use std::path::PathBuf;

/// The folder shared files are copied to: `DSA/Shares` in the user's caches folder.
pub fn default_spool_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| {
            home.join("Library")
                .join("Caches")
                .join("DSA")
                .join("Shares")
        })
}

#[cfg(target_os = "macos")]
pub use mac::{serve, SharingPicker};

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use dsa_host_core::share::{self, ShareContent, ShareOutcome, ShareUi};
    use dsa_transport::Connector;
    use objc2::msg_send;
    use objc2::rc::{autoreleasepool, Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::sel;

    use super::default_spool_dir;
    use crate::devices::Log;
    use crate::objc::{class, file_url, ns_string, on_main, CGPoint, CGRect, CGSize};

    /// What the follower calls itself to the service.
    const BUILD: &str = "dsa-devices share";

    /// How long the share menu may stay open before the share counts as closed.
    const MENU_LIMIT: Duration = Duration::from_secs(30 * 60);

    /// `NSWindowStyleMaskBorderless`.
    const BORDERLESS: usize = 0;
    /// `NSBackingStoreBuffered`.
    const BUFFERED: usize = 2;
    /// `NSRectEdgeMinY`.
    const EDGE_MIN_Y: usize = 1;

    /// Shows what the user's Android shares until `stop`.
    pub fn serve(mut connector: Box<dyn Connector>, stop: Arc<AtomicBool>, log: Log) {
        let Some(spool) = default_spool_dir() else {
            log("share: the home folder is not known, so nothing can be shared with the Mac");
            return;
        };
        let mut ui = SharingPicker;
        let line = move |line: &str| log(line);
        share::serve(&mut *connector, BUILD, &mut ui, &spool, &stop, &line);
    }

    /// Where the open menu's choice goes: shared (true) or closed (false).
    static CHOSEN: Mutex<Option<Sender<bool>>> = Mutex::new(None);

    unsafe extern "C-unwind" fn did_choose(
        _this: &AnyObject,
        _: Sel,
        _picker: *mut AnyObject,
        service: *mut AnyObject,
    ) {
        if let Ok(mut chosen) = CHOSEN.lock() {
            if let Some(chosen) = chosen.take() {
                let _ = chosen.send(!service.is_null());
            }
        }
    }

    fn delegate_class() -> Result<&'static AnyClass, String> {
        static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
        CLASS
            .get_or_init(|| unsafe {
                let mut b = ClassBuilder::new(c"DSASharePickerDelegate", class(c"NSObject")?)
                    .ok_or("the share menu's delegate class is already defined")?;
                b.add_method(
                    sel!(sharingServicePicker:didChooseSharingService:),
                    did_choose as unsafe extern "C-unwind" fn(_, _, _, _),
                );
                Ok(b.register())
            })
            .clone()
    }

    /// The window and the delegate of the open menu, kept until it closes.
    struct Open {
        window: Retained<AnyObject>,
        _picker: Retained<AnyObject>,
        _delegate: Retained<AnyObject>,
    }

    // Only touched on the main thread, where `on_main` runs.
    unsafe impl Send for Open {}

    static OPEN: Mutex<Option<Open>> = Mutex::new(None);

    /// The Mac's share menu.
    pub struct SharingPicker;

    impl ShareUi for SharingPicker {
        fn share(&mut self, content: &ShareContent) -> ShareOutcome {
            let (chosen, choice) = channel();
            if let Ok(mut slot) = CHOSEN.lock() {
                *slot = Some(chosen);
            }
            let (started, start) = channel();
            let content = content.clone();
            on_main(move || {
                let result = autoreleasepool(|_| unsafe { show(&content) });
                let _ = started.send(result);
            });
            match start.recv() {
                Ok(Ok(())) => {}
                Ok(Err(why)) => return ShareOutcome::Failed(why),
                Err(_) => return ShareOutcome::Failed("the share menu could not open".into()),
            }
            let outcome = match choice.recv_timeout(MENU_LIMIT) {
                Ok(true) => ShareOutcome::Shared,
                _ => ShareOutcome::Canceled,
            };
            on_main(|| {
                if let Ok(mut open) = OPEN.lock() {
                    if let Some(open) = open.take() {
                        unsafe {
                            let _: () = msg_send![&*open.window, orderOut: std::ptr::null_mut::<AnyObject>()];
                        }
                    }
                }
            });
            outcome
        }
    }

    /// The items the share menu offers: the files, the links and the text.
    unsafe fn items(content: &ShareContent) -> Result<Retained<AnyObject>, String> {
        let array: Allocated<AnyObject> = msg_send![class(c"NSMutableArray")?, alloc];
        let array: Option<Retained<AnyObject>> = msg_send![array, init];
        let array = array.ok_or("the share menu's items could not be made")?;
        for file in &content.files {
            let url = file_url(file)?;
            let _: () = msg_send![&*array, addObject: &*url];
        }
        let url_class = class(c"NSURL")?;
        for link in &content.links {
            let url: Option<Retained<AnyObject>> =
                msg_send![url_class, URLWithString: &*ns_string(link)];
            if let Some(url) = url {
                let _: () = msg_send![&*array, addObject: &*url];
            }
        }
        if let Some(text) = &content.text {
            let _: () = msg_send![&*array, addObject: &*ns_string(text)];
        }
        Ok(array)
    }

    /// Opens the share menu from a window that cannot be seen. Runs on the main thread.
    unsafe fn show(content: &ShareContent) -> Result<(), String> {
        let items = items(content)?;
        let screen: *mut AnyObject = msg_send![class(c"NSScreen")?, mainScreen];
        let frame: CGRect = if screen.is_null() {
            CGRect {
                origin: CGPoint { x: 400.0, y: 400.0 },
                size: CGSize {
                    width: 0.0,
                    height: 0.0,
                },
            }
        } else {
            msg_send![screen, visibleFrame]
        };
        let rect = CGRect {
            origin: CGPoint {
                x: frame.origin.x + frame.size.width / 2.0,
                y: frame.origin.y + frame.size.height / 2.0,
            },
            size: CGSize {
                width: 1.0,
                height: 1.0,
            },
        };
        let window: Allocated<AnyObject> = msg_send![class(c"NSWindow")?, alloc];
        let window: Option<Retained<AnyObject>> = msg_send![
            window,
            initWithContentRect: rect,
            styleMask: BORDERLESS,
            backing: BUFFERED,
            defer: false
        ];
        let window = window.ok_or("the share menu's window could not be made")?;
        let _: () = msg_send![&*window, setReleasedWhenClosed: false];
        let _: () = msg_send![&*window, setOpaque: false];
        let _: () = msg_send![&*window, setAlphaValue: 0.0f64];
        let _: () = msg_send![&*window, setLevel: 3isize];
        let _: () = msg_send![&*window, orderFrontRegardless];

        let app: *mut AnyObject = msg_send![class(c"NSApplication")?, sharedApplication];
        crate::objc::activate_as_accessory(app);

        let picker: Allocated<AnyObject> = msg_send![class(c"NSSharingServicePicker")?, alloc];
        let picker: Option<Retained<AnyObject>> = msg_send![picker, initWithItems: &*items];
        let picker = picker.ok_or("the share menu could not be made")?;
        let delegate: Allocated<AnyObject> = msg_send![delegate_class()?, alloc];
        let delegate: Option<Retained<AnyObject>> = msg_send![delegate, init];
        let delegate = delegate.ok_or("the share menu's delegate could not be made")?;
        let _: () = msg_send![&*picker, setDelegate: &*delegate];

        let view: *mut AnyObject = msg_send![&*window, contentView];
        if view.is_null() {
            return Err("the share menu's window has no view".into());
        }
        let bounds: CGRect = msg_send![view, bounds];
        let _: () = msg_send![&*picker, showRelativeToRect: bounds, ofView: view, preferredEdge: EDGE_MIN_Y];
        if let Ok(mut open) = OPEN.lock() {
            *open = Some(Open {
                window,
                _picker: picker,
                _delegate: delegate,
            });
        }
        Ok(())
    }
}

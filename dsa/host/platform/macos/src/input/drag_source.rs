//! An app's drag out of its task window, as an AppKit dragging session.
//!
//! Each file the app drags is a file promise (`NSFilePromiseProvider`): the target (Finder,
//! the desktop, Mail) names the folder after the drop, and the promise is written there by
//! streaming the file from the guest in pieces over the control channel. DSA keeps no copy of
//! its own. Text is a pasteboard item of plain text. The task view is the dragging source;
//! when the session ends, and once every promise a drop asked for has been written
//! ([`PromiseTracker`]), the drag ends in the guest, which gives back its access to the app's
//! files.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use dsa_host_core::drag::{DragContent, DragOffer};
use dsa_host_core::host_api::HostApiClient;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::sel;

use super::drag_out::PromiseTracker;
use crate::objc::{class, ns_string, string_of, CGPoint, CGRect, CGSize};

/// `NSDragOperationCopy`.
pub const DRAG_COPY: usize = 1;
/// How long a dropped drag waits for its promises to be written before it ends anyway.
const PROMISE_WAIT: Duration = Duration::from_secs(120);
/// The size of the picture a dragged item shows, in points.
const ICON_POINTS: f64 = 48.0;

/// One drag out of a window, as long as it lasts.
struct DragState {
    offer: DragOffer,
    client: Mutex<HostApiClient>,
    tracker: Mutex<PromiseTracker>,
    changed: Condvar,
    /// The promise providers of the drag, retained until it ends, as raw pointers.
    providers: Mutex<Vec<usize>>,
}

impl DragState {
    /// Streams file `item` of the drag to `path`; a file that cannot be read whole is removed.
    fn write(&self, item: u32, path: &Path) -> Result<(), String> {
        let written = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(path)?;
            let mut client = self.client.lock().unwrap_or_else(|e| e.into_inner());
            let mut content = DragContent::new(self.offer.drag_id, item, |read| {
                client.read_drag(read).map_err(|e| e.to_string())
            });
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let n = content.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buffer[..n])?;
            }
            file.flush()
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(path);
            return Err(format!(
                "{} could not be copied from Android: {error}",
                path.display()
            ));
        }
        Ok(())
    }

    fn settled(&self) {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settled();
        self.changed.notify_all();
    }

    /// The session ended: waits, on a thread of its own, for the promises a drop asked for,
    /// then ends the drag in the guest and lets the providers go.
    fn session_ended(self: Arc<Self>, dropped: bool) {
        std::thread::spawn(move || {
            let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            tracker.session_ended(dropped);
            let (tracker, _) = self
                .changed
                .wait_timeout_while(tracker, PROMISE_WAIT, |tracker| tracker.outcome().is_none())
                .unwrap_or_else(|e| e.into_inner());
            let dropped = tracker.outcome().unwrap_or(dropped);
            drop(tracker);
            let ended = self
                .client
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .end_drag(self.offer.drag_id, dropped);
            if let Err(error) = ended {
                eprintln!("dsa-app-window: the drag ended, but Android was not told: {error}");
            }
            let providers =
                std::mem::take(&mut *self.providers.lock().unwrap_or_else(|e| e.into_inner()));
            let mut map = PROVIDERS.lock().unwrap_or_else(|e| e.into_inner());
            for provider in providers {
                map.remove(&provider);
                // SAFETY: retained by `begin`, released once here.
                drop(unsafe { Retained::from_raw(provider as *mut AnyObject) });
            }
        });
    }
}

/// Each promise provider of a drag that lasts: its drag, its item, and the file's name.
type Providers = HashMap<usize, (Arc<DragState>, u32, String)>;
static PROVIDERS: LazyLock<Mutex<Providers>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// The drag each task view is the source of, by view.
static VIEW_DRAGS: LazyLock<Mutex<HashMap<usize, Arc<DragState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

unsafe extern "C-unwind" fn file_name(
    _this: &AnyObject,
    _: Sel,
    provider: *mut AnyObject,
    _file_type: *mut AnyObject,
) -> *mut AnyObject {
    let name = PROVIDERS
        .lock()
        .ok()
        .and_then(|map| {
            map.get(&(provider as usize))
                .map(|(_, _, name)| name.clone())
        })
        .unwrap_or_else(|| "file".into());
    Retained::autorelease_return(ns_string(&name))
}

unsafe extern "C-unwind" fn write_promise(
    _this: &AnyObject,
    _: Sel,
    provider: *mut AnyObject,
    url: *mut AnyObject,
    handler: *mut block2::Block<dyn Fn(*mut AnyObject)>,
) {
    let found = PROVIDERS.lock().ok().and_then(|map| {
        map.get(&(provider as usize))
            .map(|(drag, item, _)| (Arc::clone(drag), *item))
    });
    let path: *mut AnyObject = if url.is_null() {
        std::ptr::null_mut()
    } else {
        msg_send![url, path]
    };
    let path = PathBuf::from(string_of(path));
    let result = match found {
        Some((drag, item)) => {
            let result = drag.write(item, &path);
            drag.settled();
            result
        }
        None => Err("the drag is over".to_string()),
    };
    let error: *mut AnyObject = match result {
        Ok(()) => std::ptr::null_mut(),
        Err(why) => {
            eprintln!("dsa-app-window: {why}");
            match class(c"NSError") {
                Ok(error_class) => msg_send![
                    error_class,
                    errorWithDomain: &*ns_string("DSA"),
                    code: 1isize,
                    userInfo: std::ptr::null::<AnyObject>()
                ],
                Err(_) => std::ptr::null_mut(),
            }
        }
    };
    if let Some(handler) = handler.as_ref() {
        handler.call((error,));
    }
}

unsafe extern "C-unwind" fn operation_queue(
    _this: &AnyObject,
    _: Sel,
    _provider: *mut AnyObject,
) -> *mut AnyObject {
    // Writing a promise streams the file from the guest, so it runs off the main thread, on
    // one queue kept for the life of the program.
    static QUEUE: OnceLock<usize> = OnceLock::new();
    *QUEUE.get_or_init(|| {
        let queue: Option<Retained<AnyObject>> = match class(c"NSOperationQueue") {
            Ok(queue_class) => msg_send![queue_class, new],
            Err(_) => None,
        };
        queue
            .map(|queue| Retained::into_raw(queue) as usize)
            .unwrap_or(0)
    }) as *mut AnyObject
}

fn delegate_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let mut b = ClassBuilder::new(c"DSAFilePromiseDelegate", class(c"NSObject")?)
                .ok_or("the file promise delegate class is already defined")?;
            b.add_method(
                sel!(filePromiseProvider:fileNameForType:),
                file_name as unsafe extern "C-unwind" fn(_, _, _, _) -> _,
            );
            b.add_method(
                sel!(filePromiseProvider:writePromiseToURL:completionHandler:),
                write_promise as unsafe extern "C-unwind" fn(_, _, _, _, _),
            );
            b.add_method(
                sel!(operationQueueForFilePromiseProvider:),
                operation_queue as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            Ok(b.register())
        })
        .clone()
}

/// The one delegate of every file promise. A provider holds its delegate weakly, so it is
/// kept for the life of the program.
fn delegate() -> Result<*mut AnyObject, String> {
    static DELEGATE: OnceLock<usize> = OnceLock::new();
    let class = delegate_class()?;
    let delegate = *DELEGATE.get_or_init(|| unsafe {
        let made: Option<Retained<AnyObject>> = msg_send![class, new];
        made.map(|made| Retained::into_raw(made) as usize)
            .unwrap_or(0)
    });
    if delegate == 0 {
        return Err("AppKit could not make the file promise delegate".into());
    }
    Ok(delegate as *mut AnyObject)
}

/// A dragging item of `writer`, shown as the icon of `extension` at `point` in `view`.
unsafe fn dragging_item(
    writer: &AnyObject,
    extension: &str,
    point: CGPoint,
    offset: f64,
) -> Result<Retained<AnyObject>, String> {
    let item_class = class(c"NSDraggingItem")?;
    let allocated: Allocated<AnyObject> = msg_send![item_class, alloc];
    let item: Option<Retained<AnyObject>> = msg_send![allocated, initWithPasteboardWriter: writer];
    let item = item.ok_or("AppKit could not make a dragging item")?;
    let workspace: *mut AnyObject = msg_send![class(c"NSWorkspace")?, sharedWorkspace];
    let icon: *mut AnyObject = msg_send![workspace, iconForFileType: &*ns_string(extension)];
    let frame = CGRect {
        origin: CGPoint {
            x: point.x - ICON_POINTS / 2.0 + offset,
            y: point.y - ICON_POINTS / 2.0 - offset,
        },
        size: CGSize {
            width: ICON_POINTS,
            height: ICON_POINTS,
        },
    };
    let _: () = msg_send![&*item, setDraggingFrame: frame, contents: icon];
    Ok(item)
}

/// Starts a dragging session from `view` of what `offer` carries; `client` reads its files and
/// ends it. Must be called on the main thread, while the press that left the window is held.
/// An error ends nothing; the caller ends the drag in the guest.
pub fn begin(view: &AnyObject, offer: DragOffer, client: HostApiClient) -> Result<(), String> {
    let files: Vec<(u32, String)> = offer
        .files()
        .map(|(index, item)| (index, item.name.clone()))
        .collect();
    let text = offer.text();
    let state = Arc::new(DragState {
        tracker: Mutex::new(PromiseTracker::new(files.len())),
        offer,
        client: Mutex::new(client),
        changed: Condvar::new(),
        providers: Mutex::new(Vec::new()),
    });
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            return Err("the window has closed".into());
        }
        let in_window: CGPoint = msg_send![window, mouseLocationOutsideOfEventStream];
        let point: CGPoint =
            msg_send![view, convertPoint: in_window, fromView: std::ptr::null::<AnyObject>()];
        let items: Option<Retained<AnyObject>> = msg_send![class(c"NSMutableArray")?, new];
        let items = items.ok_or("AppKit could not make an array")?;
        let mut offset = 0.0;
        if !files.is_empty() {
            let delegate = delegate()?;
            let provider_class = class(c"NSFilePromiseProvider")?;
            for (index, name) in &files {
                let allocated: Allocated<AnyObject> = msg_send![provider_class, alloc];
                let provider: Option<Retained<AnyObject>> = msg_send![
                    allocated,
                    initWithFileType: &*ns_string("public.data"),
                    delegate: delegate
                ];
                let provider = provider.ok_or("AppKit could not make a file promise")?;
                let extension = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
                let item = dragging_item(&provider, extension, point, offset)?;
                offset += 4.0;
                let _: () = msg_send![&*items, addObject: &*item];
                let raw = Retained::into_raw(provider) as usize;
                state
                    .providers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(raw);
                PROVIDERS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(raw, (Arc::clone(&state), *index, name.clone()));
            }
        }
        if let Some(text) = text {
            let pasteboard_item: Option<Retained<AnyObject>> =
                msg_send![class(c"NSPasteboardItem")?, new];
            let pasteboard_item =
                pasteboard_item.ok_or("AppKit could not make a pasteboard item")?;
            let _: bool = msg_send![
                &*pasteboard_item,
                setString: &*ns_string(&text),
                forType: &*ns_string("public.utf8-plain-text")
            ];
            let item = dragging_item(&pasteboard_item, "txt", point, offset)?;
            let _: () = msg_send![&*items, addObject: &*item];
        }
        let app: *mut AnyObject = msg_send![class(c"NSApplication")?, sharedApplication];
        let event: *mut AnyObject = msg_send![app, currentEvent];
        if event.is_null() {
            return Err("there is no mouse event to start the drag from".into());
        }
        VIEW_DRAGS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(view as *const AnyObject as usize, Arc::clone(&state));
        let session: *mut AnyObject = msg_send![
            view,
            beginDraggingSessionWithItems: &*items,
            event: event,
            source: view
        ];
        if session.is_null() {
            VIEW_DRAGS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(view as *const AnyObject as usize));
            return Err("AppKit did not start the drag".into());
        }
    }
    Ok(())
}

/// The task view's `draggingSession:sourceOperationMaskForDraggingContext:`: a copy, inside
/// the app or out of it.
///
/// # Safety
///
/// AppKit calls this as an Objective-C method on a task view; it reads none of its pointers.
pub unsafe extern "C-unwind" fn source_operations(
    _this: &AnyObject,
    _: Sel,
    _session: *mut AnyObject,
    _context: isize,
) -> usize {
    DRAG_COPY
}

/// The task view's `draggingSession:endedAtPoint:operation:`.
///
/// # Safety
///
/// AppKit calls this as an Objective-C method with `this` a live task view; the session
/// pointer is never dereferenced.
pub unsafe extern "C-unwind" fn session_ended(
    this: &AnyObject,
    _: Sel,
    _session: *mut AnyObject,
    _point: CGPoint,
    operation: usize,
) {
    let state = VIEW_DRAGS
        .lock()
        .ok()
        .and_then(|mut drags| drags.remove(&(this as *const AnyObject as usize)));
    if let Some(state) = state {
        state.session_ended(operation != 0);
    }
}

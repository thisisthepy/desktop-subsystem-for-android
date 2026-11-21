//! The clipboard and drag and drop (`wl_data_device`).
//!
//! The desktop's clipboard reaches the guest whenever it changes while a task window has the
//! keyboard, which is when Wayland lets a program read it. What the guest copies becomes the
//! desktop's clipboard: text under every name for text, an image as `image/png`. Files
//! dropped on a task window are handed to the window's app. What an app drags out of its
//! window becomes a drag on the desktop, of files (`text/uri-list`, streamed from the guest
//! when a target asks) and text.

use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::drag::{DragOffer, LeaveWatch};
use dsa_host_core::host_api::HostApiClient;

use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::DndAction,
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};

use super::{task_of, Event, State};
use dsa_host_core::clipboard::Clip;

use crate::shell::clipboard::{host_image, host_text, selection_type, IMAGE_TYPE, TEXT_TYPES};
use crate::shell::file_drag;
use crate::shell::file_drop::{paths, URI_LIST};

/// The types an offer has, as it announces them.
pub type OfferTypes = Mutex<Vec<String>>;

#[derive(Default)]
pub struct DataState {
    /// The offer being dragged over a task window, with the task.
    drag: Option<(WlDataOffer, i32)>,
    /// The source of what the guest copied last, while it is the desktop's clipboard.
    source: Option<WlDataSource>,
    /// What the guest copied while no task window had the keyboard, to set when one has.
    waiting: Option<Clip>,
    /// The desktop's clipboard offer, read when a task window has the keyboard.
    selection: Option<WlDataOffer>,
    /// When a press of the left button leaves a task window.
    pub(crate) leave: LeaveWatch,
    /// The serial of the last press of the left button in a task window.
    pub(crate) press_serial: u32,
}

impl DataState {
    /// `source` is the desktop's clipboard now; the one before it is destroyed.
    pub(crate) fn replace_source(&mut self, source: WlDataSource) {
        if let Some(old) = self.source.replace(source) {
            old.destroy();
        }
    }

    /// The desktop took the clipboard from `source`.
    pub(crate) fn source_cancelled(&mut self, source: &WlDataSource) {
        if self.source.as_ref() == Some(source) {
            self.source = None;
        }
    }
}

/// How long the files of a dropped drag stay, for the target to copy them.
const DROPPED_FILES_KEPT: Duration = Duration::from_secs(120);

/// An app's drag out of its window, as the desktop's drag source carries it.
pub struct DragOutSource {
    offer: DragOffer,
    client: Mutex<HostApiClient>,
    /// The folder the files are streamed into, and their paths, once a target asked.
    files: Mutex<Option<(PathBuf, Vec<PathBuf>)>>,
    ended: AtomicBool,
}

impl DragOutSource {
    /// The `text/uri-list` of the drag's files, streaming them from the guest the first time.
    fn uri_list(&self) -> Result<String, String> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if files.is_none() {
            let folder = file_drag::drag_folder(&file_drag::drags_root(), self.offer.drag_id)
                .map_err(|e| format!("no folder for the dragged files: {e}"))?;
            let mut client = self.client.lock().unwrap_or_else(|e| e.into_inner());
            let paths = file_drag::materialize(&folder, &self.offer, |read| {
                client.read_drag(read).map_err(|e| e.to_string())
            });
            match paths {
                Ok(paths) => *files = Some((folder, paths)),
                Err(why) => {
                    let _ = std::fs::remove_dir_all(&folder);
                    return Err(why);
                }
            }
        }
        Ok(files
            .as_ref()
            .map(|(_, paths)| file_drag::uri_list(paths))
            .unwrap_or_default())
    }

    /// Ends the drag in the guest once, then removes its files: at once when it was
    /// cancelled, after [`DROPPED_FILES_KEPT`] when it was dropped.
    fn end(&self, dropped: bool) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        let ended = self
            .client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .end_drag(self.offer.drag_id, dropped);
        if let Err(why) = ended {
            super::super::session::log(&format!("the drag ended, but Android was not told: {why}"));
        }
        let folder = self
            .files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .map(|(folder, _)| folder);
        if let Some(folder) = folder {
            if dropped {
                std::thread::sleep(DROPPED_FILES_KEPT);
            }
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// A pipe for an offer to write into: (read end, write end).
fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` takes the two new descriptors, owned from here on.
    unsafe {
        use std::os::fd::FromRawFd;
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return None;
        }
        Some((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])))
    }
}

/// The most bytes read of an offer of text: four for each byte of UTF-8 that may cross, and
/// one past it, so text that is too long is seen to be.
const TEXT_LIMIT: u64 = dsa_host_core::clipboard::MAX_CLIPBOARD_BYTES as u64 * 4 + 1;

/// The most bytes read of an offer of an image: one past the longest PNG that crosses.
const IMAGE_LIMIT: u64 = dsa_host_core::clipboard::MAX_IMAGE_BYTES as u64 + 1;

/// Reads at most `limit` bytes of what an offer writes, on a thread of its own so the desktop
/// never waits on us (or we on ourselves, when the offer is our own).
fn read_offer(
    offer: &WlDataOffer,
    mime: &str,
    connection: &Connection,
    limit: u64,
    done: impl FnOnce(Vec<u8>) + Send + 'static,
) {
    let Some((read, write)) = pipe() else {
        return;
    };
    offer.receive(mime.to_string(), write.as_fd());
    drop(write);
    let _ = connection.flush();
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(read);
        let mut bytes = Vec::new();
        if (&mut file).take(limit).read_to_end(&mut bytes).is_ok() {
            done(bytes);
        }
    });
}

impl State {
    /// Starts a drag on the desktop of what an app drags out of task `task_id`'s window,
    /// from the press `serial`. The drag ends in the guest when the desktop's ends.
    pub fn start_drag_out(
        &mut self,
        task_id: i32,
        offer: DragOffer,
        client: HostApiClient,
        serial: u32,
    ) {
        let surface = self
            .windows
            .get(&task_id)
            .map(|window| window.surface.clone());
        let source_data = Arc::new(DragOutSource {
            offer,
            client: Mutex::new(client),
            files: Mutex::new(None),
            ended: AtomicBool::new(false),
        });
        let (Some(manager), Some(device), Some(surface)) =
            (&self.data_manager, &self.data_device, surface)
        else {
            std::thread::spawn(move || source_data.end(false));
            return;
        };
        let has_files = source_data.offer.files().next().is_some();
        let has_text = source_data.offer.text().is_some();
        let source = manager.create_data_source(&self.qh, Arc::clone(&source_data));
        if has_files {
            source.offer(URI_LIST.to_string());
        }
        if has_text {
            for mime in TEXT_TYPES {
                source.offer(mime.to_string());
            }
        }
        if source.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        device.start_drag(Some(&source), &surface, None, serial);
    }

    /// A task window got the keyboard: the desktop's clipboard may be read now, and what the
    /// guest copied meanwhile becomes the desktop's.
    pub(crate) fn data_focus_changed(&mut self) {
        if let Some(clip) = self.data_state.waiting.take() {
            self.set_clipboard(clip);
        }
        self.read_selection();
    }

    fn read_selection(&mut self) {
        if self.seat_state.keyboard_focus.is_none() {
            return;
        }
        let Some(offer) = self.data_state.selection.clone() else {
            return;
        };
        let types = offer
            .data::<OfferTypes>()
            .and_then(|types| types.lock().ok().map(|types| types.clone()))
            .unwrap_or_default();
        let Some((mime, image)) =
            selection_type(&types).map(|(mime, image)| (mime.to_string(), image))
        else {
            return;
        };
        let events = self.events.clone();
        let limit = if image { IMAGE_LIMIT } else { TEXT_LIMIT };
        read_offer(&offer, &mime, &self.connection, limit, move |bytes| {
            let clip = if image {
                host_image(bytes).map(|png| Clip::Image(png.into()))
            } else {
                host_text(bytes).map(Clip::Text)
            };
            if let Some(clip) = clip {
                events.send(Event::HostClipboard(clip));
            }
        });
    }

    /// The guest copied `clip`: it becomes the desktop's clipboard, text under every name for
    /// text, an image as `image/png`.
    pub fn set_clipboard(&mut self, clip: Clip) {
        let (Some(manager), Some(device)) = (&self.data_manager, &self.data_device) else {
            return;
        };
        if self.seat_state.keyboard_focus.is_none() {
            self.data_state.waiting = Some(clip);
            return;
        }
        let image = matches!(clip, Clip::Image(_));
        let source = manager.create_data_source(&self.qh, Arc::new(clip));
        if image {
            source.offer(IMAGE_TYPE.to_string());
        } else {
            for mime in TEXT_TYPES {
                source.offer(mime.to_string());
            }
        }
        device.set_selection(Some(&source), self.serial);
        if let Some(old) = self.data_state.source.replace(source) {
            old.destroy();
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut state.data_state.selection, id) {
                    old.destroy();
                }
                // What the guest copied comes back as the desktop's selection too; the
                // session drops text that only echoes the guest's own.
                state.read_selection();
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                id,
                ..
            } => {
                let (Some(offer), Some(task_id)) = (id, task_of(&surface)) else {
                    return;
                };
                let has_files = offer
                    .data::<OfferTypes>()
                    .and_then(|types| types.lock().ok().map(|t| t.iter().any(|t| t == URI_LIST)))
                    .unwrap_or(false);
                if has_files {
                    offer.accept(serial, Some(URI_LIST.to_string()));
                    if offer.version() >= 3 {
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                    }
                    state.data_state.drag = Some((offer, task_id));
                } else {
                    offer.accept(serial, None);
                }
            }
            wl_data_device::Event::Leave => {
                if let Some((offer, _)) = state.data_state.drag.take() {
                    offer.destroy();
                }
            }
            wl_data_device::Event::Drop => {
                let Some((offer, task_id)) = state.data_state.drag.take() else {
                    return;
                };
                let package = state
                    .windows
                    .get(&task_id)
                    .map(|window| window.model.package.clone())
                    .unwrap_or_default();
                let events = state.events.clone();
                let finished = offer.clone();
                read_offer(
                    &offer,
                    URI_LIST,
                    &state.connection,
                    TEXT_LIMIT,
                    move |bytes| {
                        let list = String::from_utf8_lossy(&bytes);
                        let paths = paths(&list);
                        if !paths.is_empty() {
                            events.send(Event::Dropped {
                                task_id,
                                package,
                                paths,
                            });
                        }
                    },
                );
                if finished.version() >= 3 {
                    finished.finish();
                }
                finished.destroy();
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Mutex::new(Vec::new())),
    ]);
}

impl Dispatch<WlDataOffer, OfferTypes> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        types: &OfferTypes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            if let Ok(mut types) = types.lock() {
                types.push(mime_type);
            }
        }
    }
}

impl Dispatch<WlDataSource, Arc<Clip>> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        clip: &Arc<Clip>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let clip = Arc::clone(clip);
                std::thread::spawn(move || {
                    let mut file = std::fs::File::from(fd);
                    let bytes: &[u8] = match &*clip {
                        Clip::Text(text) => text.as_bytes(),
                        Clip::Image(png) => png,
                    };
                    let _ = file.write_all(bytes);
                });
            }
            wl_data_source::Event::Cancelled => {
                if state.data_state.source.as_ref() == Some(source) {
                    state.data_state.source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, Arc<DragOutSource>> for State {
    fn event(
        _: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        drag: &Arc<DragOutSource>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let drag = Arc::clone(drag);
                std::thread::spawn(move || {
                    let bytes = if mime_type == URI_LIST {
                        match drag.uri_list() {
                            Ok(list) => list.into_bytes(),
                            Err(why) => {
                                super::super::session::log(&why);
                                Vec::new()
                            }
                        }
                    } else {
                        drag.offer.text().unwrap_or_default().into_bytes()
                    };
                    let mut file = std::fs::File::from(fd);
                    let _ = file.write_all(&bytes);
                });
            }
            wl_data_source::Event::Cancelled => {
                let drag = Arc::clone(drag);
                std::thread::spawn(move || drag.end(false));
                source.destroy();
            }
            wl_data_source::Event::DndFinished => {
                let drag = Arc::clone(drag);
                std::thread::spawn(move || drag.end(true));
                source.destroy();
            }
            _ => {}
        }
    }
}

//! Each task's frames, from the renderer to its window host.
//!
//! The renderer's frames come per guest display. The manager's routes say which task and
//! which window host each display's frames are for; frames of any other display are dropped.
//! Each routed task has two shared surfaces ([`FrameSlots`]): the renderer writes one while
//! the window host shows the other, and the window host is told of each new surface
//! (`FrameTexture`, its Mach port sent alongside) and of each frame in it (`Frame`).
//!
//! A surface is at least as large as the frames written into it and is made again only when
//! a frame does not fit, so resizing a window does not remake it; each frame says how much of
//! the surface it fills. A task whose route is withdrawn (its window closed or minimized)
//! lets go of its surfaces at once, and the renderer of what it held for that display.
//!
//! The renderer runs in a process of its own (`dsa-gfx`, see `crate::gfx`): its requests for
//! a frame's surface, and its word that a frame is in it, reach [`Presenter::target_for`] and
//! [`FrameSink::copied`] over its connection.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::window_hosts::HostId;
use dsa_protocol::window_host::types::envelope::Body;
use dsa_protocol::window_host::types::{Frame, FrameTexture};

use super::frame_slots::{Announce, FrameSlots, SLOTS};
use super::hosts::{HostLink, Links};
use super::log;
use super::manager::Route;
use crate::compositor::renderer::FrameSink;
use crate::compositor::surface::Surface;
use crate::compositor::surface_port::SurfacePorts;

/// How long the making of a surface waits for the window host's Mach port.
const PORT_WAIT: Duration = Duration::from_millis(200);

/// The largest frame taken, in pixels on a side. A size beyond it is a broken guest.
const MAX_SIDE: u32 = 16384;

struct TaskSurface {
    surface: Surface,
    texture_id: u64,
}

/// One routed display: its task, the window host showing it, and the task's surfaces.
struct Shown {
    task_id: i32,
    link: Arc<HostLink>,
    slots: FrameSlots,
    surfaces: [Option<TaskSurface>; SLOTS],
    /// The slot the renderer copies into on the GPU now.
    copying: Option<usize>,
}

#[derive(Default)]
struct State {
    displays: HashMap<u32, Shown>,
    next_texture: u64,
}

/// Where the renderer's frames go.
pub struct Presenter {
    state: Mutex<State>,
    links: Links,
    ports: Option<Arc<SurfacePorts>>,
    /// Told of each display no longer shown, so the renderer lets go of what it held for it.
    #[allow(clippy::type_complexity)]
    forget: Mutex<Option<Box<dyn Fn(u32) + Send + Sync>>>,
    /// How many frames reached a routed display: after a GPU reset, a guest that draws none
    /// has a compositor that did not recover.
    frames_shown: AtomicU64,
}

impl Presenter {
    pub fn new(links: Links, ports: Option<Arc<SurfacePorts>>) -> Presenter {
        Presenter {
            state: Mutex::new(State {
                displays: HashMap::new(),
                next_texture: 1,
            }),
            links,
            ports,
            forget: Mutex::new(None),
            frames_shown: AtomicU64::new(0),
        }
    }

    /// How many frames have reached a routed display so far.
    pub fn frames_shown(&self) -> u64 {
        self.frames_shown.load(Ordering::Relaxed)
    }

    /// Tells `forget` of each display no longer shown: the renderer (in its own process)
    /// lets go of what it holds for it.
    pub fn on_forget(&self, forget: Box<dyn Fn(u32) + Send + Sync>) {
        *self.forget.lock().unwrap_or_else(|e| e.into_inner()) = Some(forget);
    }

    /// The window manager's Mach service, when it has one: surfaces go to the renderer
    /// process through it too.
    pub fn ports(&self) -> Option<&Arc<SurfacePorts>> {
        self.ports.as_ref()
    }

    /// The `IOSurfaceRef` to put display `display_id`'s next frame in: of `exact`ly
    /// `width` x `height` pixels for a copy on the GPU, at least that for a frame written by the
    /// CPU. Null drops the frame. The surface stays valid until the display gets another or is
    /// no longer shown.
    pub fn target_for(&self, display_id: u32, width: u32, height: u32, exact: bool) -> *mut c_void {
        if !valid_size(width, height) {
            return std::ptr::null_mut();
        }
        let mut guard = self.lock();
        let State {
            displays,
            next_texture,
        } = &mut *guard;
        let Some(shown) = displays.get_mut(&display_id) else {
            return std::ptr::null_mut();
        };
        let slot = shown.slots.begin();
        match self.surface(next_texture, shown, slot, (width, height), exact) {
            Some(target) => {
                let raw = target.surface.as_raw();
                shown.copying = Some(slot);
                raw
            }
            None => {
                shown.slots.abandon();
                std::ptr::null_mut()
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// From now on each display's frames go as `routes` say. A display no longer routed lets
    /// go of its surfaces.
    pub fn set_routes(&self, routes: &[Route]) {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut forgotten = Vec::new();
        {
            let mut state = self.lock();
            state.displays.retain(|display_id, shown| {
                let keep = routes.iter().any(|route| {
                    route.display_id == *display_id
                        && route.task_id == shown.task_id
                        && route.host == shown.link.id()
                });
                if !keep {
                    forgotten.push(*display_id);
                }
                keep
            });
            for route in routes {
                if state.displays.contains_key(&route.display_id) {
                    continue;
                }
                let Some(link) = links.get(&route.host) else {
                    continue;
                };
                let shown = Shown {
                    task_id: route.task_id,
                    link: Arc::clone(link),
                    slots: FrameSlots::new(),
                    surfaces: [None, None],
                    copying: None,
                };
                state.displays.insert(route.display_id, shown);
            }
        }
        if let Some(forget) = self
            .forget
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            for display_id in forgotten {
                forget(display_id);
            }
        }
    }

    /// The window host showed frame `sequence` of `task_id`: the frame that waited goes.
    pub fn presented(&self, host: HostId, task_id: i32, sequence: u64, now: Instant) {
        let mut state = self.lock();
        let found = state
            .displays
            .values_mut()
            .find(|shown| shown.task_id == task_id && shown.link.id() == host);
        if let Some(shown) = found {
            if let Some(announce) = shown.slots.presented(sequence, now) {
                announce_frame(shown, announce);
            }
        }
    }

    /// Looks for window hosts that are late to answer: their next frame goes without it.
    pub fn tick(&self, now: Instant) {
        let mut state = self.lock();
        for shown in state.displays.values_mut() {
            if let Some(announce) = shown.slots.overdue(now) {
                announce_frame(shown, announce);
            }
        }
    }

    /// A window host went away: its Mach port is of no more use.
    pub fn host_gone(&self, host: HostId, process_id: u32) {
        self.lock()
            .displays
            .retain(|_, shown| shown.link.id() != host);
        if let Some(ports) = &self.ports {
            ports.forget(process_id);
        }
    }

    /// The surface of `slot` for `shown`, made (and announced) when it is missing or a frame
    /// of `size` does not fit it. The renderer copies on the GPU only into a surface of
    /// `exact`ly its color buffer's size, which changes only with the guest's display.
    fn surface<'a>(
        &self,
        next_texture: &mut u64,
        shown: &'a mut Shown,
        slot: usize,
        size: (u32, u32),
        exact: bool,
    ) -> Option<&'a TaskSurface> {
        let fits = shown.surfaces[slot].as_ref().is_some_and(|have| {
            let (width, height) = have.surface.size();
            if exact {
                (width, height) == size
            } else {
                width >= size.0 && height >= size.1
            }
        });
        if !fits {
            shown.surfaces[slot] = None;
            let texture_id = *next_texture;
            *next_texture += 1;
            let made = self.make_surface(&shown.link, shown.task_id, texture_id, size);
            match made {
                Ok(surface) => shown.surfaces[slot] = Some(surface),
                Err(why) => {
                    log(&format!(
                        "task {}'s frames cannot be shown: {why}",
                        shown.task_id
                    ));
                    return None;
                }
            }
        }
        shown.surfaces[slot].as_ref()
    }

    /// A surface of `size` for `task_id`, handed to its window host as texture `texture_id`:
    /// as a Mach port where the window host has said hello to the window manager's Mach
    /// service, else as a global surface found by its ID.
    fn make_surface(
        &self,
        link: &HostLink,
        task_id: i32,
        texture_id: u64,
        (width, height): (u32, u32),
    ) -> Result<TaskSurface, String> {
        let mut handle = None;
        let mut surface = None;
        if let Some(ports) = &self.ports {
            let private = Surface::create(width, height, false)?;
            if ports.send(link.process_id(), texture_id, &private, PORT_WAIT) {
                handle = Some(0);
                surface = Some(private);
            }
        }
        let surface = match surface {
            Some(surface) => surface,
            None => {
                let global = Surface::create(width, height, true)?;
                handle = Some(u64::from(global.id()));
                global
            }
        };
        let announced = link.send(Body::FrameTexture(FrameTexture {
            task_id,
            texture_id,
            handle: handle.unwrap_or(0),
            width,
            height,
        }));
        if !announced {
            return Err("its window has closed".into());
        }
        Ok(TaskSurface {
            surface,
            texture_id,
        })
    }
}

/// Tells the window host that frame `announce` is in its surface.
fn announce_frame(shown: &Shown, announce: Announce) {
    let Some(surface) = &shown.surfaces[announce.slot] else {
        return;
    };
    shown.link.send(Body::Frame(Frame {
        task_id: shown.task_id,
        texture_id: surface.texture_id,
        sequence: announce.sequence,
        width: announce.content.0,
        height: announce.content.1,
    }));
}

fn valid_size(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && width <= MAX_SIDE && height <= MAX_SIDE
}

impl FrameSink for Presenter {
    fn pixels(&self, display_id: u32, width: u32, height: u32, rgba: &[u8]) {
        if !valid_size(width, height) {
            return;
        }
        let mut guard = self.lock();
        let State {
            displays,
            next_texture,
        } = &mut *guard;
        let Some(shown) = displays.get_mut(&display_id) else {
            return;
        };
        let slot = shown.slots.begin();
        let written = match self.surface(next_texture, shown, slot, (width, height), false) {
            Some(target) => target.surface.write_rgba(width, height, rgba),
            None => Err(String::new()),
        };
        match written {
            Ok(()) => {
                self.frames_shown.fetch_add(1, Ordering::Relaxed);
                let now = Instant::now();
                if let Some(announce) = shown.slots.written(slot, (width, height), now) {
                    announce_frame(shown, announce);
                }
            }
            Err(why) => {
                shown.slots.abandon();
                if !why.is_empty() {
                    log(&format!(
                        "a frame of task {} was dropped: {why}",
                        shown.task_id
                    ));
                }
            }
        }
    }

    fn target(&self, display_id: u32, width: u32, height: u32) -> *mut c_void {
        self.target_for(display_id, width, height, true)
    }

    fn copied(&self, display_id: u32, width: u32, height: u32) {
        let mut state = self.lock();
        let Some(shown) = state.displays.get_mut(&display_id) else {
            return;
        };
        let Some(slot) = shown.copying.take() else {
            return;
        };
        self.frames_shown.fetch_add(1, Ordering::Relaxed);
        if let Some(announce) = shown.slots.written(slot, (width, height), Instant::now()) {
            announce_frame(shown, announce);
        }
    }
}

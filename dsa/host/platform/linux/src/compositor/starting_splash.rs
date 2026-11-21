//! The window `dsa-compositor --launch` shows while Android starts: the Linux host's DSA logo
//! ([`super::starting_logo`]) centred on the splash background of the desktop's colour
//! scheme, breathing slowly, until Android is ready and the app is asked for; then it fades
//! out.
//!
//! The timing and the sizes are `dsa_host_core::compositor::boot_splash`, shared with every
//! host. Frames are paced by the compositor's frame callbacks, so none is drawn while the
//! window is hidden, minimized or on another workspace, and none at all once the logo stands
//! still (animations turned off on the desktop). The window shows no words, not even a title;
//! a screen reader hears "Starting Android..." from its frame on the accessibility bus. It
//! stays until the app's window shows its first frame, then fades out over it.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::boot_splash::{self, BootSplash};
use dsa_host_core::compositor::splash::Icon;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

/// `xdg_toplevel`'s `suspended` state: the window is not seen at all.
const SUSPENDED: u32 = 9;

/// The window shown while Android starts; it fades out and closes when dropped.
pub struct StartingWindow {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StartingWindow {
    /// Shows the window for `package`'s app, on a thread of its own. When the desktop has no
    /// Wayland display, nothing is shown and the launch goes on.
    pub fn show(package: &str) -> StartingWindow {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::clone(&stop);
        let package = package.to_string();
        let thread = std::thread::Builder::new()
            .name("dsa-starting-window".into())
            .spawn(move || {
                if let Err(why) = run(&package, &shared) {
                    super::session::log(&format!("the starting window is not shown: {why}"));
                }
            })
            .ok();
        StartingWindow { stop, thread }
    }
}

impl Drop for StartingWindow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct State {
    configured: bool,
    /// The size the desktop asked for, in logical units; 0 leaves it to the window.
    asked: (u32, u32),
    scale: u32,
    suspended: bool,
    closed: bool,
    frame_due: bool,
    dirty: bool,
}

/// One `ARGB8888` buffer, mapped into this process.
struct Buffer {
    buffer: WlBuffer,
    pool: WlShmPool,
    map: *mut u8,
    len: usize,
    size: (u32, u32),
    _fd: OwnedFd,
}

impl Buffer {
    fn new(
        shm: &WlShm,
        qh: &QueueHandle<State>,
        size: (u32, u32),
        free: Arc<AtomicBool>,
    ) -> Result<Buffer, String> {
        let (width, height) = (size.0.max(1), size.1.max(1));
        let stride = width as usize * 4;
        let len = stride * height as usize;
        // SAFETY: a new anonymous file, owned from here on, sized and mapped whole.
        let (fd, map) = unsafe {
            let raw = libc::memfd_create(c"dsa-starting".as_ptr(), libc::MFD_CLOEXEC);
            if raw < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let fd = OwnedFd::from_raw_fd(raw);
            if libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let map = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            );
            if map == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error().to_string());
            }
            (fd, map.cast::<u8>())
        };
        let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh,
            free,
        );
        Ok(Buffer {
            buffer,
            pool,
            map,
            len,
            size: (width, height),
            _fd: fd,
        })
    }

    /// Copies `pixels` (premultiplied BGRA, as large as the buffer) in, each at `opacity`.
    fn write(&mut self, pixels: &[u8], opacity: f32) {
        let len = pixels.len().min(self.len);
        // SAFETY: the map is `self.len` bytes and stays mapped while `self` lives; the
        // compositor does not read the buffer while it is not attached.
        let target = unsafe { std::slice::from_raw_parts_mut(self.map, len) };
        if opacity >= 1.0 {
            target.copy_from_slice(&pixels[..len]);
            return;
        }
        let weight = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
        for (out, value) in target.iter_mut().zip(pixels) {
            *out = ((u32::from(*value) * weight) >> 8) as u8;
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        // SAFETY: unmaps what `new` mapped, once.
        unsafe {
            libc::munmap(self.map.cast(), self.len);
        }
    }
}

/// The key of the starting window's frame among the provider's windows. The process has no
/// task windows, and the provider watches no tree for it.
const ACCESS_KEY: i32 = 0;

/// The starting window as an AT-SPI application, the way the compositor shows its windows:
/// one active frame named "Starting Android..." in `language`, with no buttons and no tree.
/// It joins the accessibility bus only while the session's accessibility is on.
fn announce(
    language: dsa_host_core::ui_language::UiLanguage,
) -> Arc<crate::accessibility::watch::Shared> {
    use crate::accessibility::objects::{starting_window_facts, Access};
    use crate::accessibility::watch::Shared;
    let shared = Shared::new(
        Access::new(language, dsa_host_core::accessibility::PRESENCE_GRACE),
        None,
        Arc::new(|_| {}),
        Arc::new(|line: &str| super::session::log(line)),
    );
    crate::accessibility::bus::start(Arc::clone(&shared));
    shared.sync(ACCESS_KEY, starting_window_facts(language));
    shared
}

fn run(package: &str, stop: &AtomicBool) -> Result<(), String> {
    let connection = Connection::connect_to_env()
        .map_err(|error| format!("cannot reach the Wayland display: {error}"))?;
    let (globals, mut queue) = registry_queue_init::<State>(&connection)
        .map_err(|error| format!("the Wayland display did not list its globals: {error}"))?;
    let qh = queue.handle();
    let compositor: WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .map_err(|_| "the Wayland display has no wl_compositor".to_string())?;
    let shm: WlShm = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "the Wayland display has no wl_shm".to_string())?;
    let wm_base: XdgWmBase = globals
        .bind(&qh, 1..=6, ())
        .map_err(|_| "the Wayland display has no xdg_wm_base".to_string())?;

    let (dark, reduce_motion) = crate::devices::settings::starting_look();
    let language = crate::interface_language::ui_language();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(crate::shell::desktop_entry::app_id(package));
    // No title: a decorated window shows no words. A screen reader hears what the window
    // waits for from its frame on the accessibility bus instead.
    let access = announce(language);
    surface.commit();

    let free = Arc::new(AtomicBool::new(true));
    let mut state = State {
        configured: false,
        asked: (0, 0),
        scale: 1,
        suspended: false,
        closed: false,
        frame_due: true,
        dirty: true,
    };
    let started = Instant::now();
    let mut fade: Option<Instant> = None;
    let mut splash: Option<(BootSplash, (u32, u32))> = None;
    let mut buffer: Option<Buffer> = None;
    let mut pixels = Vec::new();
    let mut last_draw = Instant::now()
        .checked_sub(boot_splash::FRAME_INTERVAL)
        .unwrap_or_else(Instant::now);
    loop {
        if fade.is_none() && (stop.load(Ordering::Acquire) || state.closed) {
            if reduce_motion || !state.configured {
                break;
            }
            fade = Some(Instant::now());
            state.frame_due = true;
        }
        let fade_left =
            fade.map(|since| boot_splash::crossfade_opacity(since.elapsed(), reduce_motion));
        if fade.is_some_and(|since| boot_splash::crossfade_done(since.elapsed(), reduce_motion)) {
            break;
        }
        let moving = fade.is_some() || !reduce_motion;
        let draw = state.configured
            && free.load(Ordering::Acquire)
            && (state.dirty
                || (moving
                    && state.frame_due
                    && !state.suspended
                    && last_draw.elapsed() >= boot_splash::FRAME_INTERVAL));
        if draw {
            let scale = state.scale.max(1);
            let dpi = 96 * scale;
            let logical = if state.asked.0 > 0 && state.asked.1 > 0 {
                state.asked
            } else {
                boot_splash::WINDOW_DIP
            };
            let view = (logical.0 * scale, logical.1 * scale);
            if splash.as_ref().map(|(_, at)| *at) != Some(view) {
                let edge = boot_splash::logo_edge(view, dpi, u32::MAX);
                let logo = super::starting_logo::draw(edge);
                let icon = Icon {
                    size: (edge, edge),
                    pixels: &logo,
                };
                splash = Some((BootSplash::new(view, dark, Some(icon), dpi), view));
                buffer = None;
            }
            if buffer.is_none() {
                buffer = Some(Buffer::new(&shm, &qh, view, Arc::clone(&free))?);
            }
            let (frames, _) = splash.as_ref().expect("made above");
            let target = buffer.as_mut().expect("made above");
            frames.frame_into(
                boot_splash::logo_opacity(started.elapsed(), reduce_motion),
                &mut pixels,
            );
            target.write(&pixels, fade_left.unwrap_or(1.0));
            surface.set_buffer_scale(scale as i32);
            surface.attach(Some(&target.buffer), 0, 0);
            surface.damage_buffer(0, 0, target.size.0 as i32, target.size.1 as i32);
            if moving {
                surface.frame(&qh, ());
                state.frame_due = false;
            }
            free.store(false, Ordering::Release);
            surface.commit();
            state.dirty = false;
            last_draw = Instant::now();
        }
        connection
            .flush()
            .map_err(|error| format!("the Wayland display went away: {error}"))?;
        // Woken by the desktop's events (a frame callback above all), or after a moment to
        // look at `stop`.
        let wait = if fade.is_some() || (moving && state.frame_due) {
            boot_splash::FRAME_INTERVAL
        } else {
            Duration::from_millis(100)
        };
        if let Some(guard) = queue.prepare_read() {
            let mut fds = [libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            // SAFETY: one valid pollfd entry.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, wait.as_millis() as i32) };
            if ready > 0 {
                let _ = guard.read();
            }
        }
        queue
            .dispatch_pending(&mut state)
            .map_err(|error| format!("a Wayland event could not be handled: {error}"))?;
        if fade.is_some() {
            // The fade is stepped by time: a window that is not seen still goes.
            state.frame_due = true;
        }
    }
    drop(buffer);
    access.close(ACCESS_KEY);
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    let _ = connection.flush();
    Ok(())
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);

impl Dispatch<WlBuffer, Arc<AtomicBool>> for State {
    fn event(
        _: &mut State,
        _: &WlBuffer,
        event: wl_buffer::Event,
        free: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_buffer::Event::Release = event {
            free.store(true, Ordering::Release);
        }
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(
        state: &mut State,
        _: &WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            let factor = factor.max(1) as u32;
            if factor != state.scale {
                state.scale = factor;
                state.dirty = true;
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut State,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_due = true;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut State,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut State,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
            state.dirty = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut State,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let asked = (width.max(0) as u32, height.max(0) as u32);
                if asked != state.asked {
                    state.asked = asked;
                    state.dirty = true;
                }
                let suspended = states
                    .chunks_exact(4)
                    .any(|s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]) == SUSPENDED);
                if state.suspended && !suspended {
                    state.frame_due = true;
                }
                state.suspended = suspended;
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

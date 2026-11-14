//! The Wayland client: one `xdg_toplevel` per Android task, and the seat's input.
//!
//! Each task window is a toplevel with the app's `app_id` (its launcher entry) and title. It
//! shows the task's frames one to one in shared memory buffers, with the title bar drawn over
//! the app's top inset as on Windows (the app's icon or a back arrow, its name and the caption
//! buttons), and asks the desktop for client-side decorations so no second title bar stacks
//! above it. The icon's cell and a right click on the title bar open the window's menu, a
//! popup with the window's and Android's items. What the user does goes out as [`Event`]s:
//! reports for the window rules, input for the guest, the desktop's clipboard and dropped
//! files.
//!
//! The widgets placed on the desktop are surfaces of their own on the desktop's layer
//! (`widgets`), which the platform-neutral widget desk drives.

mod data;
mod gpu;
mod menu;
pub mod pip;
mod seat;
mod shm;
mod widgets;
mod window;

use std::collections::{BTreeMap, HashMap};
use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::window_hosts::FromHost;
use dsa_host_core::input::InputEvent;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_data_device::WlDataDevice,
    wl_data_device_manager::WlDataDeviceManager,
    wl_keyboard::WlKeyboard,
    wl_output,
    wl_pointer::WlPointer,
    wl_registry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_surface::WlSurface,
    wl_touch::WlTouch,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3, zwp_text_input_v3::ZwpTextInputV3,
};
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use crate::input::xkb::Keyboard;

pub use window::{AppLook, Window};

/// What the windows report, for the session to act on.
#[derive(Debug, Clone)]
pub enum Event {
    /// A window's report for the window rules.
    Report(FromHost),
    /// Input for the guest.
    Input(InputEvent),
    /// The desktop's clipboard has this text.
    HostClipboard(String),
    /// Pointer input on a task's picture-in-picture window outside Wayland (X11).
    PipInput {
        task_id: i32,
        input: dsa_host_core::input::WindowInput,
    },
    /// The window menu of a picture-in-picture window outside Wayland (X11): Alt+Space, or
    /// input on the open menu.
    X11Menu(crate::compositor::x11_pip::MenuInput),
    /// Input on a widget's X11 desktop window.
    X11Widget {
        widget_id: i32,
        input: crate::compositor::x11_pip::WidgetInput,
    },
    /// Files were dropped on a task's window.
    Dropped {
        task_id: i32,
        package: String,
        paths: Vec<PathBuf>,
    },
    /// A press of the left button left a task's window (`out`), maybe carrying an app's drag;
    /// `serial` is the press's, which a drag on the desktop starts with.
    DragOut {
        out: dsa_host_core::drag::DragOut,
        serial: u32,
    },
}

/// Wakes the session's loop when another thread has something for it.
pub trait Wake: Send + Sync {
    fn wake(&self);
}

/// Where the windows' events go.
#[derive(Clone)]
pub struct Events {
    sender: Sender<Event>,
    waker: Arc<dyn Wake>,
}

impl Events {
    pub fn new(sender: Sender<Event>, waker: Arc<dyn Wake>) -> Events {
        Events { sender, waker }
    }

    pub fn send(&self, event: Event) {
        if self.sender.send(event).is_ok() {
            self.waker.wake();
        }
    }
}

/// The state every Wayland event is dispatched to.
pub struct State {
    pub(crate) qh: QueueHandle<State>,
    pub(crate) connection: Connection,
    pub(crate) compositor: WlCompositor,
    pub(crate) shm: WlShm,
    pub(crate) wm_base: XdgWmBase,
    pub(crate) decorations: Option<ZxdgDecorationManagerV1>,
    pub(crate) data_manager: Option<WlDataDeviceManager>,
    pub(crate) text_input_manager: Option<ZwpTextInputManagerV3>,
    pub(crate) cursor_shapes: Option<WpCursorShapeManagerV1>,
    pub(crate) tablet_manager: Option<ZwpTabletManagerV2>,
    pub(crate) dmabuf: Option<ZwpLinuxDmabufV1>,
    pub(crate) viewporter: Option<WpViewporter>,
    pub(crate) subcompositor: Option<WlSubcompositor>,
    /// Each display's GPU images, as the desktop's buffers, by display and slot.
    pub(crate) gpu_buffers: HashMap<(u32, u32), gpu::GpuBuffer>,
    /// What reads GPU frames back in a layout the CPU cannot read, made the first time one
    /// is needed.
    pub(crate) dmabuf_reader: gpu::Reader,
    pub(crate) layer_shell: Option<ZwlrLayerShellV1>,
    /// Picture-in-picture windows through XWayland, where the desktop has no layer shell.
    pub(crate) x11: Option<crate::compositor::x11_pip::X11Pip>,
    pub(crate) seat: Option<WlSeat>,
    pub(crate) pointer: Option<WlPointer>,
    pub(crate) cursor_device: Option<WpCursorShapeDeviceV1>,
    pub(crate) keyboard: Option<WlKeyboard>,
    pub(crate) touch: Option<WlTouch>,
    pub(crate) data_device: Option<WlDataDevice>,
    pub(crate) text_input: Option<ZwpTextInputV3>,
    /// Whether the input method is on for the window that has the keyboard: off where the
    /// guest's focused view takes no text.
    pub(crate) text_input_enabled: bool,
    /// Each output's scale and size in pixels.
    pub(crate) outputs: HashMap<u32, (i32, (i32, i32))>,
    /// Each output's current refresh rate, in millihertz.
    pub(crate) output_refresh: HashMap<u32, i32>,
    /// Records the fastest output's rate for the service, which sets the guest display's.
    pub(crate) refresh_recorder: crate::monitor_refresh::RefreshRecorder,
    /// Records the main output's DPI for the service, which starts Android at its density.
    pub(crate) density_recorder: crate::monitor_density::DensityRecorder,
    pub(crate) windows: BTreeMap<i32, Window>,
    pub(crate) seat_state: seat::SeatState,
    pub(crate) data_state: data::DataState,
    pub(crate) xkb: Option<Keyboard>,
    pub(crate) events: Events,
    /// The serial of the user's last input, which moving a window or setting the clipboard
    /// needs.
    pub(crate) serial: u32,
    /// What each app's windows show of it, by package, for the windows that open later.
    pub(crate) app_looks: HashMap<String, AppLook>,
    /// Text for the title bars' names and the window menu; `None` without Pango.
    pub(crate) text: Option<crate::compositor::text::Text>,
    /// The window menu while it is open.
    pub(crate) menu: Option<menu::Menu>,
    /// The widget windows on the desktop.
    pub(crate) widgets: widgets::Widgets,
    /// Whether the session reads Korean, for the window menu's words.
    pub(crate) korean: bool,
}

/// The connection to the desktop and everything shown on it.
pub struct Desktop {
    queue: EventQueue<State>,
    pub state: State,
}

impl Desktop {
    /// Connects to the Wayland display the session names (`WAYLAND_DISPLAY`).
    pub fn connect(events: Events) -> Result<Desktop, String> {
        let connection = Connection::connect_to_env()
            .map_err(|error| format!("cannot reach the Wayland display: {error}"))?;
        let (globals, queue) = registry_queue_init::<State>(&connection)
            .map_err(|error| format!("the Wayland display did not list its globals: {error}"))?;
        let qh = queue.handle();
        let missing = |what: &str| format!("the Wayland display has no {what}");
        let compositor: WlCompositor = globals
            .bind(&qh, 4..=6, ())
            .map_err(|_| missing("wl_compositor"))?;
        let shm: WlShm = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| missing("wl_shm"))?;
        let wm_base: XdgWmBase = globals
            .bind(&qh, 1..=6, ())
            .map_err(|_| missing("xdg_wm_base (xdg-shell)"))?;
        let seat: Option<WlSeat> = globals.bind(&qh, 1..=8, ()).ok();
        let decorations = globals.bind(&qh, 1..=1, ()).ok();
        let data_manager = globals.bind(&qh, 1..=3, ()).ok();
        let text_input_manager = globals.bind(&qh, 1..=1, ()).ok();
        let cursor_shapes = globals.bind(&qh, 1..=1, ()).ok();
        let tablet_manager = globals.bind(&qh, 1..=1, ()).ok();
        let dmabuf = globals.bind(&qh, 3..=4, ()).ok();
        let viewporter = globals.bind(&qh, 1..=1, ()).ok();
        let subcompositor = globals.bind(&qh, 1..=1, ()).ok();
        let layer_shell: Option<ZwlrLayerShellV1> = globals.bind(&qh, 1..=4, ()).ok();
        let x11 = if layer_shell.is_none() {
            crate::compositor::x11_pip::X11Pip::connect(events.clone())
        } else {
            None
        };
        for global in globals.contents().clone_list() {
            if global.interface == wl_output::WlOutput::interface().name {
                let _ = globals.registry().bind::<wl_output::WlOutput, _, _>(
                    global.name,
                    global.version.min(4),
                    &qh,
                    global.name,
                );
            }
        }
        let xkb = Keyboard::new().ok();
        let text = match crate::compositor::text::Text::load() {
            Ok(text) => Some(text),
            Err(why) => {
                crate::compositor::session::log(&format!(
                    "title bars show no names, and windows use the desktop's window menu: {why}"
                ));
                None
            }
        };
        let korean =
            crate::compositor::window_menu::reads_korean_in(|name| std::env::var(name).ok());
        let widgets = widgets::Widgets::new(layer_shell.is_some(), x11.is_some(), korean);
        let mut state = State {
            qh: qh.clone(),
            connection: connection.clone(),
            compositor,
            shm,
            wm_base,
            decorations,
            data_manager,
            text_input_manager,
            cursor_shapes,
            tablet_manager,
            dmabuf,
            viewporter,
            subcompositor,
            gpu_buffers: HashMap::new(),
            dmabuf_reader: gpu::Reader::default(),
            layer_shell,
            x11,
            seat: None,
            pointer: None,
            cursor_device: None,
            keyboard: None,
            touch: None,
            data_device: None,
            text_input: None,
            text_input_enabled: false,
            outputs: HashMap::new(),
            output_refresh: HashMap::new(),
            refresh_recorder: crate::monitor_refresh::RefreshRecorder::new(
                dsa_host_unix::paths::current_user_tmp(dsa_host_unix::paths::Layout::Linux).ok(),
            ),
            density_recorder: crate::monitor_density::DensityRecorder::new(
                dsa_host_unix::paths::current_user_tmp(dsa_host_unix::paths::Layout::Linux).ok(),
            ),
            windows: BTreeMap::new(),
            seat_state: seat::SeatState::default(),
            data_state: data::DataState::default(),
            xkb,
            events,
            serial: 0,
            app_looks: HashMap::new(),
            text,
            menu: None,
            widgets,
            korean,
        };
        if let Some(seat) = seat {
            state.seat_ready(seat);
        }
        Ok(Desktop { queue, state })
    }

    /// The file descriptor to wait on for the desktop's events.
    pub fn fd(&self) -> std::os::fd::RawFd {
        self.state.connection.as_fd().as_raw_fd()
    }

    /// Reads and handles whatever the desktop sent, waiting at most `timeout` alongside
    /// `other` (a file descriptor another thread wakes the loop with). Returns whether
    /// `other` became readable.
    pub fn wait(&mut self, other: std::os::fd::RawFd, timeout: Duration) -> Result<bool, String> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("a Wayland event could not be handled: {error}"))?;
        self.state
            .connection
            .flush()
            .map_err(|error| format!("the Wayland display went away: {error}"))?;
        let Some(guard) = self.queue.prepare_read() else {
            return Ok(false);
        };
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: other,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: two valid pollfd entries.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
        if ready > 0 && fds[0].revents & libc::POLLIN != 0 {
            let _ = guard.read();
        } else {
            drop(guard);
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("a Wayland event could not be handled: {error}"))?;
        Ok(ready > 0 && fds[1].revents & libc::POLLIN != 0)
    }

    /// Runs what is due: key repeat, a widget's long press, and the size of a widget being
    /// resized.
    pub fn tick(&mut self, now: Instant) {
        self.state.repeat_due(now);
        self.state.widget_tick(now);
    }

    /// Sends the requests made since the last flush.
    pub fn flush(&mut self) {
        let _ = self.state.connection.flush();
    }
}

impl State {
    pub(crate) fn report(&self, report: FromHost) {
        self.events.send(Event::Report(report));
    }

    pub(crate) fn input(&self, input: impl Into<InputEvent>) {
        self.events.send(Event::Input(input.into()));
    }

    /// The scale of the output with global name `name`.
    pub(crate) fn output_scale(&self, name: u32) -> u32 {
        self.outputs
            .get(&name)
            .map(|(scale, _)| (*scale).max(1) as u32)
            .unwrap_or(1)
    }

    /// The size of the first output, in logical units: the area a picture-in-picture
    /// window is placed against.
    pub(crate) fn work_area(&self) -> (i32, i32) {
        self.outputs
            .values()
            .next()
            .map(|(scale, (width, height))| {
                let scale = (*scale).max(1);
                (width / scale, height / scale)
            })
            .unwrap_or((1920, 1080))
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == wl_output::WlOutput::interface().name => {
                let _ = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, name);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.outputs.remove(&name).is_some() {
                    state.record_density();
                }
                if state.output_refresh.remove(&name).is_some() {
                    state.record_refresh();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entry = state.outputs.entry(*name).or_insert((1, (0, 0)));
        match event {
            wl_output::Event::Scale { factor } => {
                entry.0 = factor;
                state.record_density();
            }
            wl_output::Event::Mode {
                flags: wayland_client::WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(wl_output::Mode::Current) => {
                entry.1 = (width, height);
                // An output that sends no scale has a scale of 1, known once it has a mode.
                state.record_density();
                if state.output_refresh.insert(*name, refresh) != Some(refresh) {
                    state.record_refresh();
                }
            }
            _ => {}
        }
    }
}

impl State {
    /// Records the fastest output's refresh rate for the service.
    fn record_refresh(&mut self) {
        let rates: Vec<i32> = self.output_refresh.values().copied().collect();
        if let Err(why) = self.refresh_recorder.record(rates) {
            crate::compositor::session::log(&why);
        }
    }

    /// Records the main output's DPI for the service.
    fn record_density(&mut self) {
        let outputs: Vec<(u32, i32)> = self
            .outputs
            .iter()
            .map(|(name, (scale, _))| (*name, *scale))
            .collect();
        if let Err(why) = self.density_recorder.record(outputs) {
            crate::compositor::session::log(&why);
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore wayland_client::protocol::wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore ZxdgDecorationManagerV1);
delegate_noop!(State: ignore ZxdgToplevelDecorationV1);
delegate_noop!(State: ignore WlDataDeviceManager);
delegate_noop!(State: ignore ZwpTextInputManagerV3);
delegate_noop!(State: ignore WpCursorShapeManagerV1);
delegate_noop!(State: ignore WpCursorShapeDeviceV1);
delegate_noop!(State: ignore ZwpTabletManagerV2);

impl Dispatch<WlBuffer, Arc<AtomicBool>> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        busy: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            busy.store(false, Ordering::Release);
        }
    }
}

/// The task a surface shows, from the user data it was made with.
pub(crate) fn task_of(surface: &WlSurface) -> Option<i32> {
    surface.data::<i32>().copied()
}

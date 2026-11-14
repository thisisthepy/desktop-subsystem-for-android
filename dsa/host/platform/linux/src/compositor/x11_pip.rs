//! Picture-in-picture windows through XWayland, for desktops whose Wayland side offers no way
//! to keep a window above the others or place it (GNOME).
//!
//! Each task in picture-in-picture gets an X11 window with `_NET_WM_STATE_ABOVE`, placed in
//! the bottom right corner of the screen. Frames read back go in with `PutImage`; frames on the
//! GPU go in without a copy through the CPU: each dmabuf becomes a pixmap (DRI3), and Render
//! composites it onto the window, which also puts its R, G, B order into the window's. The
//! window's pointer input goes to the task as from its Wayland window.
//!
//! Alt+Space in the window opens DSA's window menu as an override-redirect window that takes
//! the pointer and the keyboard while it is open. Its Move and Size ask the window manager for
//! its keyboard move and size (`_NET_WM_MOVERESIZE`), and More window options for its own
//! window menu (`_GTK_SHOW_WINDOW_MENU`), each only when the window manager lists it in
//! `_NET_SUPPORTED`; one that does not is logged and nothing else happens.
//!
//! The same connection holds the widget windows on such a desktop
//! (`crate::compositor::widget_windows`): each is an X11 window of the desktop type
//! (`_NET_WM_WINDOW_TYPE_DESKTOP`, as the desktop icons' window is) with a 32-bit visual, so
//! a widget's see-through corners stay see-through, kept below (`_NET_WM_STATE_BELOW`), on
//! every workspace (`_NET_WM_STATE_STICKY`) and in neither the task bar nor the pager. Its
//! input goes to the widget windows with the pointer's place on the screen, which a window
//! being moved does not change under it. The widget's menu is the window menu's
//! override-redirect window.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use dsa_host_core::compositor::window_hosts::FromHost;
use dsa_host_core::input::WindowInput;
use x11rb::connection::Connection;
use x11rb::protocol::dri3::ConnectionExt as _;
use x11rb::protocol::render::{ConnectionExt as _, PictOp};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _,
    CreateGCAux, CreateWindowAux, EventMask, GrabMode, ImageFormat, InputFocus, KeyButMask,
    PropMode, VisualClass, WindowClass,
};
use x11rb::protocol::Event as XEvent;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::frames::{Frame, GpuFrame, GpuImage};
use super::wayland::{Event, Events};

/// The margin from the screen's corner, in pixels.
const MARGIN: i32 = 16;

struct PipWindow {
    window: u32,
    gc: u32,
    picture: u32,
}

/// The X11 connection that holds the picture-in-picture windows.
pub struct X11Pip {
    connection: Arc<RustConnection>,
    root: u32,
    root_depth: u8,
    root_visual: u32,
    screen_size: (u16, u16),
    above: [u32; 2],
    delete_window: u32,
    windows: HashMap<i32, PipWindow>,
    by_window: Arc<Mutex<HashMap<u32, i32>>>,
    /// Each display's GPU images as pixmaps with a Render picture, by display and slot.
    images: HashMap<(u32, u32), (u32, u32)>,
    /// The Render format of `R, G, B, A` bytes (the renderer's dmabufs).
    abgr_format: Option<u32>,
    /// The Render format of the windows' visual.
    window_format: Option<u32>,
    dri3: bool,
    /// The window menu's window while it is open, which the event thread reads.
    menu_window: Arc<Mutex<Option<u32>>>,
    /// The window menu's window and its graphics context.
    menu: Option<(u32, u32)>,
    wm: WmAtoms,
    /// Each widget's desktop window, and the widget of each window, which the event thread
    /// reads.
    widgets: HashMap<i32, WidgetWindow>,
    widget_of: Arc<Mutex<HashMap<u32, i32>>>,
    /// The screen's 32-bit visual and a colormap of it, for see-through widget windows.
    argb: Option<(u32, u32)>,
    desktop: DesktopAtoms,
}

/// A widget's desktop window, its graphics context and its size.
struct WidgetWindow {
    window: u32,
    gc: u32,
    depth: u8,
    size: (u16, u16),
}

/// What a widget's window is told it is.
struct DesktopAtoms {
    /// `_NET_WM_WINDOW_TYPE` and `_NET_WM_WINDOW_TYPE_DESKTOP`.
    window_type: Option<(u32, u32)>,
    /// `_NET_WM_STATE`, and the states of a widget's window: below, sticky, in neither the
    /// task bar nor the pager.
    state: Option<u32>,
    states: Vec<u32>,
    /// `_MOTIF_WM_HINTS`, which asks for no frame.
    motif: Option<u32>,
}

/// Input on a widget's desktop window: where the pointer is in the window and on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetInput {
    Entered {
        local: (i32, i32),
        root: (i32, i32),
    },
    Left,
    Moved {
        local: (i32, i32),
        root: (i32, i32),
    },
    Button {
        detail: u8,
        down: bool,
        local: (i32, i32),
        root: (i32, i32),
    },
    /// A key went down, by its evdev code.
    Key(u32),
    /// The window took the keyboard, or lost it.
    Focus(bool),
    /// The window must be drawn again.
    Exposed,
}

/// What the window manager is asked through, and what it says it supports.
struct WmAtoms {
    moveresize: Option<u32>,
    show_menu: Option<u32>,
    change_state: Option<u32>,
    maximized: Option<(u32, u32)>,
    /// `_NET_SUPPORTED` on the root window.
    supported: Vec<u32>,
}

impl WmAtoms {
    /// `atom`, when the window manager lists it as supported.
    fn supported(&self, atom: Option<u32>) -> Option<u32> {
        atom.filter(|atom| self.supported.contains(atom))
    }
}

/// Input on the window menu of a picture-in-picture window, or the key that opens it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MenuInput {
    /// Alt+Space in a task's picture-in-picture window.
    Open { task_id: i32 },
    /// The pointer moved to `position` on the menu (its pixels, maybe outside it).
    Moved((f64, f64)),
    /// A button went down or up at `position` on the menu.
    Button { down: bool, position: (f64, f64) },
    /// A key went down, by its evdev code.
    Key(u32),
    /// The pointer left the menu.
    Left,
    /// The menu's window must be drawn again.
    Exposed,
}

/// X11's key codes are evdev's plus 8.
const X11_KEYCODE_OFFSET: u8 = 8;

fn atom(connection: &RustConnection, name: &[u8]) -> Option<u32> {
    Some(connection.intern_atom(false, name).ok()?.reply().ok()?.atom)
}

impl X11Pip {
    /// Connects to the X server `DISPLAY` names (XWayland), and reads its pointer input on a
    /// thread of its own. `None` when there is no X server.
    pub fn connect(events: Events) -> Option<X11Pip> {
        std::env::var_os("DISPLAY")?;
        let (connection, screen) = x11rb::connect(None).ok()?;
        let connection = Arc::new(connection);
        let screen = &connection.setup().roots[screen];
        let (root, root_depth, root_visual) = (screen.root, screen.root_depth, screen.root_visual);
        let screen_size = (screen.width_in_pixels, screen.height_in_pixels);
        let argb_visual = screen
            .allowed_depths
            .iter()
            .filter(|depth| depth.depth == 32)
            .flat_map(|depth| &depth.visuals)
            .find(|visual| visual.class == VisualClass::TRUE_COLOR)
            .map(|visual| visual.visual_id);
        let state = atom(&connection, b"_NET_WM_STATE")?;
        let above = atom(&connection, b"_NET_WM_STATE_ABOVE")?;
        let delete_window = atom(&connection, b"WM_DELETE_WINDOW")?;
        let dri3 = connection
            .dri3_query_version(1, 2)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some();
        let (abgr_format, window_format) = connection
            .render_query_pict_formats()
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|formats| {
                let abgr = formats
                    .formats
                    .iter()
                    .find(|f| {
                        f.depth == 32
                            && f.direct.red_shift == 0
                            && f.direct.green_shift == 8
                            && f.direct.blue_shift == 16
                            && f.direct.alpha_mask == 0xff
                            && f.direct.alpha_shift == 24
                    })
                    .map(|f| f.id);
                let window = formats
                    .screens
                    .iter()
                    .flat_map(|screen| &screen.depths)
                    .flat_map(|depth| &depth.visuals)
                    .find(|visual| visual.visual == root_visual)
                    .map(|visual| visual.format);
                (abgr, window)
            })
            .unwrap_or((None, None));
        let by_window = Arc::new(Mutex::new(HashMap::new()));
        let menu_window = Arc::new(Mutex::new(None));
        let widget_of = Arc::new(Mutex::new(HashMap::new()));
        {
            let connection = connection.clone();
            let by_window = by_window.clone();
            let menu_window = menu_window.clone();
            let widget_of = widget_of.clone();
            std::thread::spawn(move || {
                read_events(
                    &connection,
                    &by_window,
                    &menu_window,
                    &widget_of,
                    delete_window,
                    &events,
                )
            });
        }
        let argb = argb_visual.and_then(|visual| {
            let colormap = connection.generate_id().ok()?;
            connection
                .create_colormap(ColormapAlloc::NONE, colormap, root, visual)
                .ok()?;
            Some((visual, colormap))
        });
        let window_type = atom(&connection, b"_NET_WM_WINDOW_TYPE")
            .zip(atom(&connection, b"_NET_WM_WINDOW_TYPE_DESKTOP"));
        let states = [
            &b"_NET_WM_STATE_BELOW"[..],
            b"_NET_WM_STATE_STICKY",
            b"_NET_WM_STATE_SKIP_TASKBAR",
            b"_NET_WM_STATE_SKIP_PAGER",
        ]
        .iter()
        .filter_map(|name| atom(&connection, name))
        .collect();
        let desktop = DesktopAtoms {
            window_type,
            state: Some(state),
            states,
            motif: atom(&connection, b"_MOTIF_WM_HINTS"),
        };
        let wm = WmAtoms {
            moveresize: atom(&connection, b"_NET_WM_MOVERESIZE"),
            show_menu: atom(&connection, b"_GTK_SHOW_WINDOW_MENU"),
            change_state: atom(&connection, b"WM_CHANGE_STATE"),
            maximized: atom(&connection, b"_NET_WM_STATE_MAXIMIZED_VERT")
                .zip(atom(&connection, b"_NET_WM_STATE_MAXIMIZED_HORZ")),
            supported: atom(&connection, b"_NET_SUPPORTED")
                .and_then(|supported| {
                    connection
                        .get_property(false, root, supported, AtomEnum::ATOM, 0, 4096)
                        .ok()?
                        .reply()
                        .ok()
                })
                .and_then(|reply| reply.value32().map(|atoms| atoms.collect()))
                .unwrap_or_default(),
        };
        Some(X11Pip {
            connection,
            root,
            root_depth,
            root_visual,
            screen_size,
            above: [state, above],
            delete_window,
            windows: HashMap::new(),
            by_window,
            images: HashMap::new(),
            abgr_format,
            window_format,
            dri3,
            menu_window,
            menu: None,
            wm,
            widgets: HashMap::new(),
            widget_of,
            argb,
            desktop,
        })
    }

    /// Opens widget `widget_id`'s desktop window at `origin` on the screen, `size` pixels,
    /// below the other windows, named `title` for the window manager. Returns whether it
    /// opened.
    pub fn open_widget(
        &mut self,
        widget_id: i32,
        origin: (i32, i32),
        size: (i32, i32),
        title: &str,
    ) -> bool {
        if self.widgets.contains_key(&widget_id) {
            return true;
        }
        let c = &*self.connection;
        let (Ok(window), Ok(gc)) = (c.generate_id(), c.generate_id()) else {
            return false;
        };
        let width = size.0.clamp(1, i32::from(u16::MAX)) as u16;
        let height = size.1.clamp(1, i32::from(u16::MAX)) as u16;
        let mask = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::ENTER_WINDOW
            | EventMask::LEAVE_WINDOW
            | EventMask::KEY_PRESS
            | EventMask::FOCUS_CHANGE
            | EventMask::EXPOSURE;
        // A 32-bit visual keeps the widget's see-through pixels see-through; without one the
        // window is opaque where the widget is clear.
        let (depth, visual, aux) = match self.argb {
            Some((visual, colormap)) => (
                32,
                visual,
                CreateWindowAux::new()
                    .background_pixel(0)
                    .border_pixel(0)
                    .colormap(colormap)
                    .event_mask(mask),
            ),
            None => (
                self.root_depth,
                self.root_visual,
                CreateWindowAux::new().background_pixel(0).event_mask(mask),
            ),
        };
        let x = origin.0.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let y = origin.1.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let created = c.create_window(
            depth,
            window,
            self.root,
            x,
            y,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &aux,
        );
        if created.is_err() {
            return false;
        }
        let _ = c.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );
        if let Some((window_type, desktop)) = self.desktop.window_type {
            let _ = c.change_property32(
                PropMode::REPLACE,
                window,
                window_type,
                AtomEnum::ATOM,
                &[desktop],
            );
        }
        if let Some(state) = self.desktop.state {
            let _ = c.change_property32(
                PropMode::REPLACE,
                window,
                state,
                AtomEnum::ATOM,
                &self.desktop.states,
            );
        }
        if let Some(motif) = self.desktop.motif {
            // Decorations only (flag 2), and none of them.
            let _ = c.change_property32(PropMode::REPLACE, window, motif, motif, &[2, 0, 0, 0, 0]);
        }
        let _ = c.create_gc(gc, window, &CreateGCAux::new());
        let _ = c.map_window(window);
        // Some window managers place a new window themselves; it goes where the widget is.
        let _ = c.configure_window(
            window,
            &ConfigureWindowAux::new().x(i32::from(x)).y(i32::from(y)),
        );
        let _ = c.flush();
        if let Ok(mut widget_of) = self.widget_of.lock() {
            widget_of.insert(window, widget_id);
        }
        self.widgets.insert(
            widget_id,
            WidgetWindow {
                window,
                gc,
                depth,
                size: (width, height),
            },
        );
        true
    }

    /// Moves and sizes widget `widget_id`'s window.
    pub fn place_widget(&mut self, widget_id: i32, origin: (i32, i32), size: (i32, i32)) {
        let Some(widget) = self.widgets.get_mut(&widget_id) else {
            return;
        };
        let width = size.0.clamp(1, i32::from(u16::MAX)) as u16;
        let height = size.1.clamp(1, i32::from(u16::MAX)) as u16;
        widget.size = (width, height);
        let _ = self.connection.configure_window(
            widget.window,
            &ConfigureWindowAux::new()
                .x(origin.0)
                .y(origin.1)
                .width(u32::from(width))
                .height(u32::from(height)),
        );
        let _ = self.connection.flush();
    }

    /// Shows `pixels` (premultiplied BGRA rows, `size`) in widget `widget_id`'s window.
    pub fn show_widget(&self, widget_id: i32, size: (u32, u32), pixels: &[u8]) {
        let Some(widget) = self.widgets.get(&widget_id) else {
            return;
        };
        if pixels.len() < size.0 as usize * size.1 as usize * 4 {
            return;
        }
        let _ = self.connection.put_image(
            ImageFormat::Z_PIXMAP,
            widget.window,
            widget.gc,
            size.0.min(u32::from(u16::MAX)) as u16,
            size.1.min(u32::from(u16::MAX)) as u16,
            0,
            0,
            0,
            widget.depth,
            pixels,
        );
        let _ = self.connection.flush();
    }

    /// Gives widget `widget_id`'s window the keyboard, while the widget is edited.
    pub fn focus_widget(&self, widget_id: i32) {
        let Some(widget) = self.widgets.get(&widget_id) else {
            return;
        };
        let _ =
            self.connection
                .set_input_focus(InputFocus::PARENT, widget.window, x11rb::CURRENT_TIME);
        let _ = self.connection.flush();
    }

    /// Closes widget `widget_id`'s window.
    pub fn close_widget(&mut self, widget_id: i32) {
        let Some(widget) = self.widgets.remove(&widget_id) else {
            return;
        };
        if let Ok(mut widget_of) = self.widget_of.lock() {
            widget_of.remove(&widget.window);
        }
        let c = &*self.connection;
        let _ = c.free_gc(widget.gc);
        let _ = c.destroy_window(widget.window);
        let _ = c.flush();
    }

    /// The screen's size, in pixels.
    pub fn screen_size(&self) -> (i32, i32) {
        (i32::from(self.screen_size.0), i32::from(self.screen_size.1))
    }

    /// Opens the task's picture-in-picture window, `size` pixels, in the screen's bottom
    /// right corner, above the other windows.
    pub fn open(&mut self, task_id: i32, size: (u32, u32), title: &str) -> bool {
        if self.windows.contains_key(&task_id) {
            return true;
        }
        let c = &*self.connection;
        let (Ok(window), Ok(gc), Ok(picture)) = (c.generate_id(), c.generate_id(), c.generate_id())
        else {
            return false;
        };
        let width = size.0.clamp(1, u32::from(u16::MAX)) as u16;
        let height = size.1.clamp(1, u32::from(u16::MAX)) as u16;
        let x = (i32::from(self.screen_size.0) - i32::from(width) - MARGIN).max(0) as i16;
        let y = (i32::from(self.screen_size.1) - i32::from(height) - MARGIN).max(0) as i16;
        let mask = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::LEAVE_WINDOW
            | EventMask::STRUCTURE_NOTIFY
            // Alt+Space, for the window menu.
            | EventMask::KEY_PRESS;
        let created = c.create_window(
            self.root_depth,
            window,
            self.root,
            x,
            y,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            self.root_visual,
            &CreateWindowAux::new().event_mask(mask).background_pixel(0),
        );
        if created.is_err() {
            return false;
        }
        let _ = c.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );
        let _ = c.change_property32(
            PropMode::REPLACE,
            window,
            self.above[0],
            AtomEnum::ATOM,
            &[self.above[1]],
        );
        if let Some(protocols) = atom(c, b"WM_PROTOCOLS") {
            let _ = c.change_property32(
                PropMode::REPLACE,
                window,
                protocols,
                AtomEnum::ATOM,
                &[self.delete_window],
            );
        }
        let _ = c.create_gc(gc, window, &CreateGCAux::new());
        if let Some(format) = self.window_format {
            let _ = c.render_create_picture(picture, window, format, &Default::default());
        }
        let _ = c.map_window(window);
        // Some window managers place a new window themselves; it goes to the corner again.
        let _ = c.configure_window(
            window,
            &ConfigureWindowAux::new().x(i32::from(x)).y(i32::from(y)),
        );
        // Ask the window manager to keep it above, for one that reads the state only then.
        let _ = c.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ClientMessageEvent::new(32, window, self.above[0], [1, self.above[1], 0, 1, 0]),
        );
        let _ = c.flush();
        if let Ok(mut by_window) = self.by_window.lock() {
            by_window.insert(window, task_id);
        }
        self.windows.insert(
            task_id,
            PipWindow {
                window,
                gc,
                picture,
            },
        );
        true
    }

    pub fn close(&mut self, task_id: i32) {
        let Some(pip) = self.windows.remove(&task_id) else {
            return;
        };
        if let Ok(mut by_window) = self.by_window.lock() {
            by_window.remove(&pip.window);
        }
        let c = &*self.connection;
        if self.window_format.is_some() {
            let _ = c.render_free_picture(pip.picture);
        }
        let _ = c.free_gc(pip.gc);
        let _ = c.destroy_window(pip.window);
        let _ = c.flush();
    }

    fn fit(&self, pip: &PipWindow, size: (u32, u32)) {
        let _ = self.connection.configure_window(
            pip.window,
            &ConfigureWindowAux::new().width(size.0).height(size.1),
        );
    }

    /// Shows a frame read back to memory (`XRGB8888`, which is a 24-bit Z pixmap's layout).
    pub fn show_pixels(&mut self, task_id: i32, frame: &Frame) {
        let Some(pip) = self.windows.get(&task_id) else {
            return;
        };
        self.fit(pip, (frame.width, frame.height));
        let _ = self.connection.put_image(
            ImageFormat::Z_PIXMAP,
            pip.window,
            pip.gc,
            frame.width as u16,
            frame.height as u16,
            0,
            0,
            0,
            24,
            &frame.pixels,
        );
        let _ = self.connection.flush();
    }

    /// Takes a GPU image of a display as a pixmap, for the frames that will be in it.
    pub fn import(&mut self, image: &GpuImage, fd: OwnedFd) {
        let (true, Some(format)) = (self.dri3, self.abgr_format) else {
            return;
        };
        let c = &*self.connection;
        let (Ok(pixmap), Ok(picture)) = (c.generate_id(), c.generate_id()) else {
            return;
        };
        let size = image.stride * image.height;
        let imported = c.dri3_pixmap_from_buffer(
            pixmap,
            self.root,
            size,
            image.width as u16,
            image.height as u16,
            image.stride as u16,
            32,
            32,
            fd,
        );
        if imported.is_err() {
            return;
        }
        let _ = c.render_create_picture(picture, pixmap, format, &Default::default());
        if let Some((old_pixmap, old_picture)) = self
            .images
            .insert((image.display_id, image.slot), (pixmap, picture))
        {
            let _ = c.render_free_picture(old_picture);
            let _ = c.free_pixmap(old_pixmap);
        }
    }

    /// Shows a frame in one of a display's GPU images.
    pub fn show_gpu(&mut self, task_id: i32, display_id: u32, frame: GpuFrame) {
        let Some(pip) = self.windows.get(&task_id) else {
            return;
        };
        let (Some((_, source)), Some(_)) = (
            self.images.get(&(display_id, frame.slot)),
            self.window_format,
        ) else {
            return;
        };
        self.fit(pip, (frame.width, frame.height));
        let _ = self.connection.render_composite(
            PictOp::SRC,
            *source,
            x11rb::NONE,
            pip.picture,
            0,
            0,
            0,
            0,
            0,
            0,
            frame.width as u16,
            frame.height as u16,
        );
        let _ = self.connection.flush();
    }

    /// Where the task's window's top left corner is on the screen.
    pub fn window_origin(&self, task_id: i32) -> Option<(i32, i32)> {
        let pip = self.windows.get(&task_id)?;
        let reply = self
            .connection
            .translate_coordinates(pip.window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some((i32::from(reply.dst_x), i32::from(reply.dst_y)))
    }

    /// Opens the window menu: `pixels` (BGRA rows, opaque) of `size`, with its top left corner
    /// at `at` on the screen, moved in where it would leave the screen. It takes the pointer
    /// and the keyboard while it is open, so a click elsewhere or Escape closes it. Returns
    /// whether it opened.
    pub fn open_menu(&mut self, pixels: &[u8], size: (u32, u32), at: (i32, i32)) -> bool {
        self.close_menu();
        let c = &*self.connection;
        let (Ok(window), Ok(gc)) = (c.generate_id(), c.generate_id()) else {
            return false;
        };
        let width = size.0.clamp(1, u32::from(u16::MAX)) as u16;
        let height = size.1.clamp(1, u32::from(u16::MAX)) as u16;
        let x =
            at.0.min(i32::from(self.screen_size.0) - i32::from(width))
                .max(0);
        let y =
            at.1.min(i32::from(self.screen_size.1) - i32::from(height))
                .max(0);
        let mask = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::LEAVE_WINDOW
            | EventMask::KEY_PRESS
            | EventMask::EXPOSURE;
        let created = c.create_window(
            self.root_depth,
            window,
            self.root,
            x as i16,
            y as i16,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            self.root_visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .event_mask(mask)
                .background_pixel(0x00f9_f9f9),
        );
        if created.is_err() {
            return false;
        }
        let _ = c.create_gc(gc, window, &CreateGCAux::new());
        let _ = c.map_window(window);
        if let Ok(mut menu) = self.menu_window.lock() {
            *menu = Some(window);
        }
        self.menu = Some((window, gc));
        self.paint_menu(pixels, size);
        let pointer = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::LEAVE_WINDOW;
        let _ = c.grab_pointer(
            false,
            window,
            pointer,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        );
        let _ = c.grab_keyboard(
            false,
            window,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        );
        let _ = c.flush();
        true
    }

    /// Draws the open window menu again, as `pixels` (BGRA rows) of `size`.
    pub fn paint_menu(&self, pixels: &[u8], size: (u32, u32)) {
        let Some((window, gc)) = self.menu else {
            return;
        };
        if pixels.len() < size.0 as usize * size.1 as usize * 4 {
            return;
        }
        let _ = self.connection.put_image(
            ImageFormat::Z_PIXMAP,
            window,
            gc,
            size.0 as u16,
            size.1 as u16,
            0,
            0,
            0,
            24,
            pixels,
        );
        let _ = self.connection.flush();
    }

    /// Closes the window menu, if it is open, and gives the pointer and keyboard back.
    pub fn close_menu(&mut self) {
        let Some((window, gc)) = self.menu.take() else {
            return;
        };
        if let Ok(mut menu) = self.menu_window.lock() {
            *menu = None;
        }
        let c = &*self.connection;
        let _ = c.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = c.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = c.free_gc(gc);
        let _ = c.destroy_window(window);
        let _ = c.flush();
    }

    /// Asks the window manager about the task's window with a client message to the root
    /// window, as the Extended Window Manager Hints say. False when there is no window.
    fn ask_wm(&self, task_id: i32, kind: u32, data: [u32; 5]) -> bool {
        let Some(pip) = self.windows.get(&task_id) else {
            return false;
        };
        let _ = self.connection.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ClientMessageEvent::new(32, pip.window, kind, data),
        );
        let _ = self.connection.flush();
        true
    }

    /// Has the window manager move or size the task's window (`_NET_WM_MOVERESIZE`), in
    /// `direction`, from where the pointer is. Returns why it cannot, when it cannot.
    pub fn move_resize(&self, task_id: i32, direction: u32) -> Result<(), String> {
        let kind = self
            .wm
            .supported(self.wm.moveresize)
            .ok_or("the window manager does not move windows for them (_NET_WM_MOVERESIZE)")?;
        let at = self
            .connection
            .query_pointer(self.root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or((0, 0), |reply| {
                (i32::from(reply.root_x), i32::from(reply.root_y))
            });
        let data = crate::compositor::window_menu::moveresize_message(at, direction, 0);
        self.ask_wm(task_id, kind, data)
            .then_some(())
            .ok_or_else(|| "the window is gone".into())
    }

    /// Asks the window manager for its own window menu of the task's window at `at` on the
    /// screen (`_GTK_SHOW_WINDOW_MENU`). Returns why it cannot, when it cannot.
    pub fn show_window_menu(&self, task_id: i32, at: (i32, i32)) -> Result<(), String> {
        let kind = self
            .wm
            .supported(self.wm.show_menu)
            .ok_or("the window manager has no window menu to open (_GTK_SHOW_WINDOW_MENU)")?;
        let data = crate::compositor::window_menu::show_window_menu_message(at);
        self.ask_wm(task_id, kind, data)
            .then_some(())
            .ok_or_else(|| "the window is gone".into())
    }

    /// Asks the window manager to minimize the task's window (`WM_CHANGE_STATE`, iconic).
    pub fn minimize(&self, task_id: i32) {
        const ICONIC_STATE: u32 = 3;
        if let Some(kind) = self.wm.change_state {
            self.ask_wm(task_id, kind, [ICONIC_STATE, 0, 0, 0, 0]);
        }
    }

    /// Asks the window manager to maximize the task's window, or to restore it.
    pub fn maximize(&self, task_id: i32, maximized: bool) {
        if let Some((vertical, horizontal)) = self.wm.maximized {
            let data = [u32::from(maximized), vertical, horizontal, 1, 0];
            self.ask_wm(task_id, self.above[0], data);
        }
    }
}

/// What an event on the window menu's window means for the menu; `None` for one it ignores.
fn menu_input(event: &XEvent) -> Option<MenuInput> {
    Some(match event {
        XEvent::MotionNotify(e) => MenuInput::Moved((f64::from(e.event_x), f64::from(e.event_y))),
        XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => MenuInput::Button {
            down: matches!(event, XEvent::ButtonPress(_)),
            position: (f64::from(e.event_x), f64::from(e.event_y)),
        },
        XEvent::KeyPress(e) => MenuInput::Key(u32::from(e.detail.checked_sub(X11_KEYCODE_OFFSET)?)),
        XEvent::LeaveNotify(_) => MenuInput::Left,
        XEvent::Expose(_) => MenuInput::Exposed,
        _ => return None,
    })
}

/// The window an event happened on, for the events the menu takes.
fn event_window(event: &XEvent) -> Option<u32> {
    Some(match event {
        XEvent::MotionNotify(e) => e.event,
        XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => e.event,
        XEvent::KeyPress(e) => e.event,
        XEvent::LeaveNotify(e) => e.event,
        XEvent::Expose(e) => e.window,
        _ => return None,
    })
}

/// What an event on a widget's desktop window means for the widget; `None` for one it
/// ignores.
fn widget_input(event: &XEvent) -> Option<WidgetInput> {
    let at = |x: i16, y: i16| (i32::from(x), i32::from(y));
    Some(match event {
        XEvent::EnterNotify(e) => WidgetInput::Entered {
            local: at(e.event_x, e.event_y),
            root: at(e.root_x, e.root_y),
        },
        XEvent::LeaveNotify(_) => WidgetInput::Left,
        XEvent::MotionNotify(e) => WidgetInput::Moved {
            local: at(e.event_x, e.event_y),
            root: at(e.root_x, e.root_y),
        },
        XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => WidgetInput::Button {
            detail: e.detail,
            down: matches!(event, XEvent::ButtonPress(_)),
            local: at(e.event_x, e.event_y),
            root: at(e.root_x, e.root_y),
        },
        XEvent::KeyPress(e) => {
            WidgetInput::Key(u32::from(e.detail.checked_sub(X11_KEYCODE_OFFSET)?))
        }
        XEvent::FocusIn(_) => WidgetInput::Focus(true),
        XEvent::FocusOut(_) => WidgetInput::Focus(false),
        XEvent::Expose(_) => WidgetInput::Exposed,
        _ => return None,
    })
}

/// The window an event on a widget's window happened on.
fn widget_event_window(event: &XEvent) -> Option<u32> {
    Some(match event {
        XEvent::EnterNotify(e) | XEvent::LeaveNotify(e) => e.event,
        XEvent::FocusIn(e) | XEvent::FocusOut(e) => e.event,
        other => return event_window(other),
    })
}

fn read_events(
    connection: &RustConnection,
    by_window: &Mutex<HashMap<u32, i32>>,
    menu_window: &Mutex<Option<u32>>,
    widget_of: &Mutex<HashMap<u32, i32>>,
    delete_window: u32,
    events: &Events,
) {
    let task_of = |window: u32| {
        by_window
            .lock()
            .ok()
            .and_then(|map| map.get(&window).copied())
    };
    let widget_of = |window: u32| {
        widget_of
            .lock()
            .ok()
            .and_then(|map| map.get(&window).copied())
    };
    let button = crate::input::pointer::x11_button;
    while let Ok(event) = connection.wait_for_event() {
        // The window menu, while it is open, takes every event on its window (and, grabbed,
        // every press and key).
        let menu = menu_window.lock().ok().and_then(|menu| *menu);
        if menu.is_some() && event_window(&event) == menu {
            if let Some(input) = menu_input(&event) {
                events.send(Event::X11Menu(input));
            }
            continue;
        }
        if let Some(widget_id) = widget_event_window(&event).and_then(widget_of) {
            if let Some(input) = widget_input(&event) {
                events.send(Event::X11Widget { widget_id, input });
            }
            continue;
        }
        let (task_id, input) = match event {
            XEvent::KeyPress(e) => {
                let Some(task_id) = task_of(e.event) else {
                    continue;
                };
                let state = u16::from(e.state);
                let alt = state & u16::from(KeyButMask::MOD1) != 0;
                let ctrl = state & u16::from(KeyButMask::CONTROL) != 0;
                let code = u32::from(e.detail.saturating_sub(X11_KEYCODE_OFFSET));
                if crate::compositor::window_menu::opens_window_menu(code, alt, ctrl) {
                    events.send(Event::X11Menu(MenuInput::Open { task_id }));
                }
                continue;
            }
            XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => {
                let Some(task_id) = task_of(e.event) else {
                    continue;
                };
                let down = matches!(event, XEvent::ButtonPress(_));
                let position = (f64::from(e.event_x), f64::from(e.event_y));
                let input = match e.detail {
                    4..=7 if down => {
                        let notches = match e.detail {
                            4 => (0.0, 1.0),
                            5 => (0.0, -1.0),
                            6 => (-1.0, 0.0),
                            _ => (1.0, 0.0),
                        };
                        WindowInput::Wheel { position, notches }
                    }
                    detail => match button(detail) {
                        Some(button) => WindowInput::MouseButton {
                            button,
                            down,
                            position,
                        },
                        None => continue,
                    },
                };
                (task_id, input)
            }
            XEvent::MotionNotify(e) => {
                let Some(task_id) = task_of(e.event) else {
                    continue;
                };
                let position = (f64::from(e.event_x), f64::from(e.event_y));
                (task_id, WindowInput::MouseMove { position })
            }
            XEvent::LeaveNotify(e) => {
                let Some(task_id) = task_of(e.event) else {
                    continue;
                };
                (task_id, WindowInput::MouseLeave)
            }
            XEvent::ClientMessage(e) if e.data.as_data32()[0] == delete_window => {
                if let Some(task_id) = task_of(e.window) {
                    events.send(Event::Report(FromHost::Closed { task_id }));
                }
                continue;
            }
            _ => continue,
        };
        events.send(Event::PipInput { task_id, input });
    }
}

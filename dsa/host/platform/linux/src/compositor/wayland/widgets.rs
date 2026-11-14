//! The widget windows on the Wayland desktop: one surface per widget on the desktop's layer,
//! which the widget desk (`dsa_host_core::widget_windows::desk`) drives.
//!
//! Each widget is a layer surface on the bottom layer where the desktop has a layer shell, an
//! X11 desktop window through XWayland where it has none (GNOME), else an ordinary toplevel
//! (see `crate::compositor::widget_windows` for what each can and cannot do). The desk says
//! where each window is and what it shows; this module makes the surfaces, puts the desk's
//! pictures in their buffers, and passes the seat's input on to the desk in its pixels.
//!
//! A layer surface is placed by its margins from the output's top left corner, so a move is a
//! change of margins. The desktop gives the pointer on the surface from where it had put it,
//! which lags behind a move on its way, so each move is followed by a `wl_display.sync`, and
//! the pointer is measured from the place the desktop had when it gave it.
//!
//! The widget's menu is a popup DSA draws ([`crate::compositor::widget_menu`]): a popup of the
//! widget's layer surface or toplevel, or the window menu's X11 window for an X11 widget.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use dsa_host_core::compositor::caption::scaled;
use dsa_host_core::widget_windows::desk::{Anchor, Button, Desk, Effect};
use dsa_host_core::widget_windows::paint::Image;
use dsa_host_core::widget_windows::{WindowCommand, WindowReport};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::xdg::shell::client::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor as PopupAnchor, ConstraintAdjustment, Gravity,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{
        self, Anchor as LayerAnchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
    },
};

use super::shm::ShmBuffer;
use super::State;
use crate::compositor::session::log;
use crate::compositor::title_bar::Coverage;
use crate::compositor::widget_menu::{paint_widget_menu, rows, WidgetMenuLayout, CHECK};
use crate::compositor::widget_windows::{
    button, cursor_name, desk_local, dpi_of_scale, key, logical_place, logical_size, padded,
    shell_note, widget_shell, x11_button, WidgetShell,
};
use crate::compositor::window_menu::TEXT_AT_100;
use crate::compositor::x11_pip::{MenuInput, WidgetInput};

/// The user data of a widget's surfaces: the widget's ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetSurface(pub i32);

/// The user data of the widget menu's surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetMenuSurface;

/// The user data of the sync that follows a move of a widget's layer surface: the widget and
/// the place, in logical pixels, the desktop has once it is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetMoved {
    widget_id: i32,
    origin: (i32, i32),
}

/// What shows a widget.
enum Shell {
    Layer(ZwlrLayerSurfaceV1),
    Toplevel {
        xdg_surface: XdgSurface,
        toplevel: XdgToplevel,
    },
    X11,
}

/// One widget's window.
struct WidgetWindow {
    shell: Shell,
    /// The Wayland surface, but for an X11 window.
    surface: Option<WlSurface>,
    viewport: Option<WpViewport>,
    buffers: Vec<ShmBuffer>,
    configured: bool,
    /// The scale of the output it is on.
    scale: u32,
    /// The place the desktop has it at, in logical pixels, which the pointer is measured from.
    applied: (i32, i32),
    /// Whether the window takes the keyboard (while its widget is edited).
    keyboard: bool,
    /// Whether the desktop is moving the toplevel, after the desk asked for a move.
    moving: bool,
    /// The size the desk gave it last, in its pixels.
    size: (i32, i32),
}

/// What shows the widget's menu.
// At most one menu is open, so the popup stays inline rather than boxed.
#[allow(clippy::large_enum_variant)]
enum MenuShell {
    Popup {
        surface: WlSurface,
        xdg_surface: XdgSurface,
        popup: XdgPopup,
        buffer: Option<ShmBuffer>,
        configured: bool,
    },
    X11,
}

/// The widget's open menu.
struct WidgetMenu {
    widget_id: i32,
    layout: WidgetMenuLayout,
    labels: Vec<Option<Coverage>>,
    check: Option<Coverage>,
    highlighted: Option<usize>,
    shell: MenuShell,
    scale: u32,
    /// Whether a release may choose the item under the pointer: after a press in the menu, or
    /// once the pointer moved in it, so the release of the press that opened it chooses
    /// nothing.
    armed: bool,
    first: Option<(f64, f64)>,
}

/// What the pointer is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum On {
    Widget(i32),
    Menu,
}

/// The widget windows of the desktop.
pub struct Widgets {
    pub desk: Desk,
    shell: WidgetShell,
    windows: BTreeMap<i32, WidgetWindow>,
    menu: Option<WidgetMenu>,
    pointer: Option<On>,
    /// Where the pointer is on the surface it is on, in logical pixels.
    position: (f64, f64),
    /// Each touch on a widget: its widget.
    touches: HashMap<i32, i32>,
    keyboard: Option<i32>,
    /// The serial of the last press on a widget, which a popup grab and a move take.
    press: u32,
    /// The windows to draw again.
    dirty: BTreeSet<i32>,
    noted: bool,
}

impl Widgets {
    /// The widget windows of a desktop with a layer shell or not, and an X server or not, in
    /// `korean` or English.
    pub fn new(layer_shell: bool, x11: bool, korean: bool) -> Widgets {
        let decode: dsa_host_core::widget_windows::desk::Decode = Box::new(|png: &[u8]| {
            dsa_host_core::png::decode(png)
                .map(|image| Image::from_rgba(image.width, image.height, &image.pixels))
        });
        Widgets {
            desk: Desk::new(
                crate::compositor::widget_windows::look(false, 0),
                96,
                korean,
                decode,
            ),
            shell: widget_shell(layer_shell, x11),
            windows: BTreeMap::new(),
            menu: None,
            pointer: None,
            position: (0.0, 0.0),
            touches: HashMap::new(),
            keyboard: None,
            press: 0,
            dirty: BTreeSet::new(),
            noted: false,
        }
    }
}

impl State {
    /// The scale of the main output, which a new widget window opens at.
    fn main_scale(&self) -> u32 {
        self.outputs
            .values()
            .next()
            .map(|(scale, _)| (*scale).max(1) as u32)
            .unwrap_or(1)
    }

    /// Carries out what the widgets' watch asks, and what it leads to.
    pub fn widget_command(&mut self, command: WindowCommand) {
        if !self.widgets.noted {
            self.widgets.noted = true;
            if let Some(note) = shell_note(self.widgets.shell) {
                log(note);
            }
        }
        let scale = self.main_scale();
        self.widgets.desk.dpi = dpi_of_scale(scale);
        let areas = match (self.widgets.shell, &self.x11) {
            (WidgetShell::X11, Some(x11)) => {
                let (width, height) = x11.screen_size();
                vec![(0, 0, width, height)]
            }
            _ => self
                .outputs
                .values()
                .next()
                .map(|(_, (width, height))| vec![(0, 0, *width, *height)])
                .unwrap_or_default(),
        };
        self.widgets.desk.set_areas(areas);
        self.widgets.desk.command(command, Instant::now());
        self.widget_effects();
    }

    /// The desktop's dark theme and accent are `dark` and `accent` now.
    pub fn widget_look(&mut self, dark: bool, accent: u32) {
        self.widgets.desk.look = crate::compositor::widget_windows::look(dark, accent);
        let ids = self.widgets.desk.ids();
        self.widgets.dirty.extend(ids);
        self.draw_widgets();
    }

    /// What the widget windows have for the service.
    pub fn widget_reports(&mut self) -> Vec<WindowReport> {
        self.widgets.desk.take_reports()
    }

    /// What is due for the widget windows at `now`.
    pub(crate) fn widget_tick(&mut self, now: Instant) {
        if self.widgets.windows.is_empty() {
            return;
        }
        self.widgets.desk.tick(now);
        self.widget_effects();
    }

    /// Carries out the desk's effects, then draws the windows they changed.
    fn widget_effects(&mut self) {
        let effects = self.widgets.desk.take_effects();
        for effect in effects {
            match effect {
                Effect::Open {
                    widget_id,
                    origin,
                    size,
                } => self.open_widget(widget_id, origin, size),
                Effect::Place {
                    widget_id,
                    origin,
                    size,
                } => self.place_widget(widget_id, origin, size),
                Effect::Redraw { widget_id } => {
                    self.widgets.dirty.insert(widget_id);
                }
                Effect::Close { widget_id } => self.close_widget(widget_id),
                Effect::Menu {
                    widget_id,
                    anchor,
                    entries,
                    ..
                } => self.open_widget_menu(widget_id, anchor, &entries),
                Effect::Focus { widget_id } => self.focus_widget(widget_id, true),
                Effect::Log(line) => log(&line),
            }
        }
        // A window whose widget is no longer edited gives the keyboard back.
        let done: Vec<i32> = self
            .widgets
            .windows
            .iter()
            .filter(|(id, window)| {
                window.keyboard && !self.widgets.desk.window(**id).is_some_and(|w| w.editing())
            })
            .map(|(id, _)| *id)
            .collect();
        for widget_id in done {
            self.focus_widget(widget_id, false);
        }
        self.draw_widgets();
    }

    fn open_widget(&mut self, widget_id: i32, origin: (i32, i32), size: (i32, i32)) {
        if self.widgets.windows.contains_key(&widget_id) {
            self.place_widget(widget_id, origin, size);
            return;
        }
        let title = self
            .widgets
            .desk
            .window(widget_id)
            .map(|w| w.widget.provider.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "Widget".to_string());
        let scale = self.main_scale();
        let (x, y) = (
            logical_place(origin.0, scale),
            logical_place(origin.1, scale),
        );
        let (width, height) = (logical_size(size.0, scale), logical_size(size.1, scale));
        let window = match (self.widgets.shell, &self.layer_shell, &mut self.x11) {
            (WidgetShell::Layer, Some(shell), _) => {
                let surface = self
                    .compositor
                    .create_surface(&self.qh, WidgetSurface(widget_id));
                let layer = shell.get_layer_surface(
                    &surface,
                    None,
                    Layer::Bottom,
                    "dsa-widget".into(),
                    &self.qh,
                    WidgetSurface(widget_id),
                );
                layer.set_anchor(LayerAnchor::Top | LayerAnchor::Left);
                // Placed from the output's corner, whatever panels it has.
                layer.set_exclusive_zone(-1);
                layer.set_margin(y, 0, 0, x);
                layer.set_size(width as u32, height as u32);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                surface.commit();
                WidgetWindow {
                    shell: Shell::Layer(layer),
                    surface: Some(surface),
                    viewport: None,
                    buffers: Vec::new(),
                    configured: false,
                    scale,
                    applied: (x, y),
                    keyboard: false,
                    moving: false,
                    size,
                }
            }
            (WidgetShell::X11, _, Some(x11)) => {
                if !x11.open_widget(widget_id, origin, size, &title) {
                    log(&format!("the window of widget {widget_id} did not open"));
                    return;
                }
                WidgetWindow {
                    shell: Shell::X11,
                    surface: None,
                    viewport: None,
                    buffers: Vec::new(),
                    configured: true,
                    scale: 1,
                    applied: origin,
                    keyboard: false,
                    moving: false,
                    size,
                }
            }
            _ => {
                let surface = self
                    .compositor
                    .create_surface(&self.qh, WidgetSurface(widget_id));
                let xdg_surface =
                    self.wm_base
                        .get_xdg_surface(&surface, &self.qh, WidgetSurface(widget_id));
                let toplevel = xdg_surface.get_toplevel(&self.qh, WidgetSurface(widget_id));
                toplevel.set_title(title);
                toplevel.set_app_id("io.github.thisisthepy.DSA.Widget".into());
                if let Some(manager) = &self.decorations {
                    let decoration = manager.get_toplevel_decoration(&toplevel, &self.qh, ());
                    decoration.set_mode(
                        wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode::ClientSide,
                    );
                }
                xdg_surface.set_window_geometry(0, 0, width, height);
                surface.commit();
                WidgetWindow {
                    shell: Shell::Toplevel {
                        xdg_surface,
                        toplevel,
                    },
                    surface: Some(surface),
                    viewport: None,
                    buffers: Vec::new(),
                    configured: false,
                    scale,
                    applied: (x, y),
                    keyboard: false,
                    moving: false,
                    size,
                }
            }
        };
        self.widgets.windows.insert(widget_id, window);
        self.widgets.dirty.insert(widget_id);
    }

    fn place_widget(&mut self, widget_id: i32, origin: (i32, i32), size: (i32, i32)) {
        let Some(window) = self.widgets.windows.get_mut(&widget_id) else {
            return;
        };
        let scale = window.scale;
        // A new place at the same size is a move; one with a new size is an edit beginning or
        // ending, or a resize, which keeps the window's corner where the desktop has it.
        let moving = window.size == size;
        window.size = size;
        match &window.shell {
            Shell::Layer(layer) => {
                let (x, y) = (
                    logical_place(origin.0, scale),
                    logical_place(origin.1, scale),
                );
                layer.set_margin(y, 0, 0, x);
                layer.set_size(
                    logical_size(size.0, scale) as u32,
                    logical_size(size.1, scale) as u32,
                );
                if let Some(surface) = &window.surface {
                    surface.commit();
                }
                // The pointer is measured from the old place until the desktop has the new.
                let data = WidgetMoved {
                    widget_id,
                    origin: (x, y),
                };
                self.connection.display().sync(&self.qh, data);
            }
            Shell::X11 => {
                if let Some(x11) = &mut self.x11 {
                    x11.place_widget(widget_id, origin, size);
                }
                window.applied = origin;
            }
            Shell::Toplevel { toplevel, .. } => {
                // A toplevel cannot place itself: a move the desk starts is the desktop's own,
                // from the press that started it.
                let (x, y) = (
                    logical_place(origin.0, scale),
                    logical_place(origin.1, scale),
                );
                if (x, y) != window.applied && moving && !window.moving {
                    if let Some(seat) = &self.seat {
                        toplevel._move(seat, self.widgets.press);
                        window.moving = true;
                    }
                }
                window.applied = (x, y);
            }
        }
        self.widgets.dirty.insert(widget_id);
    }

    fn close_widget(&mut self, widget_id: i32) {
        if self
            .widgets
            .menu
            .as_ref()
            .is_some_and(|menu| menu.widget_id == widget_id)
        {
            self.close_widget_menu(false);
        }
        let Some(window) = self.widgets.windows.remove(&widget_id) else {
            return;
        };
        if self.widgets.pointer == Some(On::Widget(widget_id)) {
            self.widgets.pointer = None;
        }
        if self.widgets.keyboard == Some(widget_id) {
            self.widgets.keyboard = None;
        }
        self.widgets.touches.retain(|_, id| *id != widget_id);
        self.widgets.dirty.remove(&widget_id);
        if let Some(viewport) = window.viewport {
            viewport.destroy();
        }
        match window.shell {
            Shell::Layer(layer) => layer.destroy(),
            Shell::Toplevel {
                xdg_surface,
                toplevel,
            } => {
                toplevel.destroy();
                xdg_surface.destroy();
            }
            Shell::X11 => {
                if let Some(x11) = &mut self.x11 {
                    x11.close_widget(widget_id);
                }
            }
        }
        if let Some(surface) = window.surface {
            surface.destroy();
        }
    }

    /// The widget's window takes the keyboard while it is edited, so Escape ends the edit, or
    /// gives it back.
    fn focus_widget(&mut self, widget_id: i32, on: bool) {
        let Some(window) = self.widgets.windows.get_mut(&widget_id) else {
            return;
        };
        if window.keyboard == on {
            return;
        }
        window.keyboard = on;
        match &window.shell {
            Shell::Layer(layer) => {
                // On demand: the keyboard comes with the press that lifted the widget, and goes
                // with a click elsewhere. Layer shells before version 4 have no such mode.
                if layer.version() >= 4 {
                    layer.set_keyboard_interactivity(if on {
                        KeyboardInteractivity::OnDemand
                    } else {
                        KeyboardInteractivity::None
                    });
                    if let Some(surface) = &window.surface {
                        surface.commit();
                    }
                }
            }
            Shell::X11 => {
                if on {
                    if let Some(x11) = &self.x11 {
                        x11.focus_widget(widget_id);
                    }
                }
            }
            Shell::Toplevel { .. } => {}
        }
    }

    /// Draws every widget window whose picture changed.
    fn draw_widgets(&mut self) {
        let dirty = std::mem::take(&mut self.widgets.dirty);
        let now = Instant::now();
        for widget_id in dirty {
            let Some(picture) = self.widgets.desk.picture(widget_id, now) else {
                continue;
            };
            let Some(window) = self.widgets.windows.get_mut(&widget_id) else {
                continue;
            };
            if !window.configured {
                // Drawn once the desktop has configured it.
                continue;
            }
            if let Shell::X11 = window.shell {
                if let Some(x11) = &self.x11 {
                    x11.show_widget(widget_id, (picture.width, picture.height), &picture.pixels);
                }
                continue;
            }
            let Some(surface) = window.surface.clone() else {
                continue;
            };
            let scale = window.scale.max(1);
            let logical = (
                logical_size(picture.width as i32, scale),
                logical_size(picture.height as i32, scale),
            );
            // With a viewport the picture goes in as it is, shown at the logical size;
            // without one it is padded to whole logical pixels at the output's scale.
            if window.viewport.is_none() {
                if let Some(viewporter) = &self.viewporter {
                    window.viewport = Some(viewporter.get_viewport(&surface, &self.qh, ()));
                }
            }
            let picture = match &window.viewport {
                Some(_) => picture,
                None => padded(&picture, scale),
            };
            let size = (picture.width, picture.height);
            window.buffers.retain(|buffer| buffer.size == size);
            let free = window.buffers.iter().position(|buffer| !buffer.is_busy());
            let index = match free {
                Some(index) => index,
                None => match ShmBuffer::with_format(&self.shm, &self.qh, size, Format::Argb8888) {
                    Ok(buffer) => {
                        window.buffers.push(buffer);
                        window.buffers.len() - 1
                    }
                    Err(why) => {
                        log(&format!(
                            "widget {widget_id}: no buffer for its picture: {why}"
                        ));
                        continue;
                    }
                },
            };
            let buffer = &mut window.buffers[index];
            buffer.write(&picture.pixels);
            buffer.set_busy();
            match &window.viewport {
                Some(viewport) => {
                    surface.set_buffer_scale(1);
                    viewport.set_destination(logical.0, logical.1);
                }
                None => surface.set_buffer_scale(scale as i32),
            }
            if let Shell::Toplevel { xdg_surface, .. } = &window.shell {
                xdg_surface.set_window_geometry(0, 0, logical.0, logical.1);
            }
            surface.attach(Some(&buffer.buffer), 0, 0);
            surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
            surface.commit();
        }
    }

    /// A point on widget `widget_id`'s surface, in logical pixels, in the desk's pixels of its
    /// window.
    fn widget_local(&self, widget_id: i32, surface: (f64, f64)) -> (i32, i32) {
        let Some(window) = self.widgets.windows.get(&widget_id) else {
            return (0, 0);
        };
        let desk_origin = self
            .widgets
            .desk
            .window(widget_id)
            .map_or((0, 0), |w| w.origin);
        match window.shell {
            Shell::Layer(_) => desk_local(window.applied, surface, window.scale, desk_origin),
            // A toplevel's place is the desktop's: the point is on the window as given.
            Shell::Toplevel { .. } | Shell::X11 => {
                let scale = f64::from(window.scale.max(1));
                (
                    (surface.0 * scale).floor() as i32,
                    (surface.1 * scale).floor() as i32,
                )
            }
        }
    }

    /// The cursor over `local` in widget `widget_id`'s window.
    fn widget_cursor(&mut self, widget_id: i32, local: (i32, i32)) {
        let shape = match self.widgets.desk.hit(widget_id, local).map(cursor_name) {
            Some("w-resize") => Shape::WResize,
            Some("e-resize") => Shape::EResize,
            Some("n-resize") => Shape::NResize,
            Some("s-resize") => Shape::SResize,
            Some("nw-resize") => Shape::NwResize,
            Some("ne-resize") => Shape::NeResize,
            Some("sw-resize") => Shape::SwResize,
            Some("se-resize") => Shape::SeResize,
            _ => Shape::Default,
        };
        self.set_shape(shape);
    }

    /// The pointer came onto `surface` at `position` (logical pixels). Returns whether it is a
    /// widget's or the widget menu's, which the seat then leaves alone.
    pub(crate) fn widget_pointer_enter(
        &mut self,
        surface: &WlSurface,
        position: (f64, f64),
    ) -> bool {
        if surface.data::<WidgetMenuSurface>().is_some() {
            self.widgets.pointer = Some(On::Menu);
            self.set_shape(Shape::Default);
            self.widget_menu_pointer(position);
            return true;
        }
        let Some(WidgetSurface(widget_id)) = surface.data::<WidgetSurface>().copied() else {
            self.widgets.pointer = None;
            return false;
        };
        self.widgets.pointer = Some(On::Widget(widget_id));
        self.widgets.position = position;
        let local = self.widget_local(widget_id, position);
        self.widgets.desk.entered(widget_id, local);
        self.widget_cursor(widget_id, local);
        self.widget_effects();
        true
    }

    /// The pointer left the surface it was on. Returns whether that was a widget's.
    pub(crate) fn widget_pointer_leave(&mut self) -> bool {
        match self.widgets.pointer.take() {
            Some(On::Widget(widget_id)) => {
                // The desktop's own move of a toplevel takes the pointer, and its release
                // never comes: the move is over.
                let moved = self
                    .widgets
                    .windows
                    .get_mut(&widget_id)
                    .is_some_and(|window| std::mem::take(&mut window.moving));
                if moved {
                    self.widgets.desk.cancel(widget_id);
                }
                self.widgets.desk.left(widget_id);
                self.widget_effects();
                true
            }
            Some(On::Menu) => {
                self.widget_menu_highlight(None);
                true
            }
            None => false,
        }
    }

    /// The pointer moved to `position` (logical pixels) on the surface it is on. Returns
    /// whether that is a widget's.
    pub(crate) fn widget_pointer_motion(&mut self, position: (f64, f64)) -> bool {
        match self.widgets.pointer {
            Some(On::Widget(widget_id)) => {
                self.widgets.position = position;
                let local = self.widget_local(widget_id, position);
                self.widgets.desk.moved(widget_id, local);
                self.widget_cursor(widget_id, local);
                self.widget_effects();
                true
            }
            Some(On::Menu) => {
                self.widget_menu_pointer(position);
                true
            }
            None => false,
        }
    }

    /// A button went down or up on the surface the pointer is on. Returns whether that is a
    /// widget's.
    pub(crate) fn widget_pointer_button(&mut self, serial: u32, code: u32, down: bool) -> bool {
        match self.widgets.pointer {
            Some(On::Widget(widget_id)) => {
                let Some(button) = button(code) else {
                    return true;
                };
                let local = self.widget_local(widget_id, self.widgets.position);
                if down {
                    self.widgets.press = serial;
                    // A press on a widget closes its open menu first, as a click on its window
                    // does on Windows; the edit goes on.
                    if self.widgets.menu.is_some() {
                        self.close_widget_menu(false);
                    }
                    self.widgets
                        .desk
                        .down(widget_id, local, button, false, Instant::now());
                } else {
                    if let Some(window) = self.widgets.windows.get_mut(&widget_id) {
                        window.moving = false;
                    }
                    self.widgets.desk.up(widget_id, local, button);
                }
                self.widget_effects();
                true
            }
            Some(On::Menu) => {
                let position = self.widgets.position;
                self.widget_menu_button(position, down);
                true
            }
            None => false,
        }
    }

    /// A finger touched `surface` at `position` (logical pixels). Returns whether that is a
    /// widget's or the widget menu's.
    pub(crate) fn widget_touch_down(
        &mut self,
        surface: &WlSurface,
        id: i32,
        position: (f64, f64),
        serial: u32,
    ) -> bool {
        if surface.data::<WidgetMenuSurface>().is_some() {
            if let Some(index) = self.widget_menu_item_at(position) {
                self.widget_menu_choose(index);
            }
            return true;
        }
        let Some(WidgetSurface(widget_id)) = surface.data::<WidgetSurface>().copied() else {
            return false;
        };
        self.widgets.press = serial;
        if self.widgets.menu.is_some() {
            self.close_widget_menu(false);
        }
        // One finger at a time presses a widget, as one mouse does.
        if self.widgets.touches.values().any(|id| *id == widget_id) {
            return true;
        }
        self.widgets.touches.insert(id, widget_id);
        let local = self.widget_local(widget_id, position);
        self.widgets
            .desk
            .down(widget_id, local, Button::Primary, true, Instant::now());
        self.widget_effects();
        true
    }

    /// Touch `id` moved to `position` (logical pixels on its surface). Returns whether it is
    /// on a widget.
    pub(crate) fn widget_touch_motion(&mut self, id: i32, position: (f64, f64)) -> bool {
        let Some(widget_id) = self.widgets.touches.get(&id).copied() else {
            return false;
        };
        let local = self.widget_local(widget_id, position);
        self.widgets.desk.moved(widget_id, local);
        self.widgets.position = position;
        self.widget_effects();
        true
    }

    /// Touch `id` went up. Returns whether it was on a widget.
    pub(crate) fn widget_touch_up(&mut self, id: i32) -> bool {
        let Some(widget_id) = self.widgets.touches.remove(&id) else {
            return false;
        };
        let local = self.widget_local(widget_id, self.widgets.position);
        self.widgets.desk.up(widget_id, local, Button::Primary);
        self.widget_effects();
        true
    }

    /// A pen is at `position` (logical pixels) on widget `widget_id`'s surface, touching it
    /// now and before as `(down, was_down)` say, or `left` it.
    pub(crate) fn widget_pen(
        &mut self,
        widget_id: i32,
        position: (f64, f64),
        (down, was_down): (bool, bool),
        left: bool,
    ) {
        let local = self.widget_local(widget_id, position);
        self.widgets.position = position;
        if down && !was_down {
            self.widgets.press = self.serial;
            if self.widgets.menu.is_some() {
                self.close_widget_menu(false);
            }
            self.widgets
                .desk
                .down(widget_id, local, Button::Primary, true, Instant::now());
        } else if was_down && !down {
            self.widgets.desk.up(widget_id, local, Button::Primary);
        } else if left {
            self.widgets.desk.left(widget_id);
        } else {
            self.widgets.desk.moved(widget_id, local);
        }
        self.widget_effects();
    }

    /// The desktop took every touch back.
    pub(crate) fn widget_touch_cancel(&mut self) {
        let touched: Vec<i32> = self.widgets.touches.drain().map(|(_, id)| id).collect();
        for widget_id in touched {
            self.widgets.desk.cancel(widget_id);
        }
        self.widget_effects();
    }

    /// The keyboard came to `surface`. Returns whether that is a widget's or its menu's.
    pub(crate) fn widget_keyboard_enter(&mut self, surface: &WlSurface) -> bool {
        if surface.data::<WidgetMenuSurface>().is_some() {
            return true;
        }
        let Some(WidgetSurface(widget_id)) = surface.data::<WidgetSurface>().copied() else {
            return false;
        };
        self.widgets.keyboard = Some(widget_id);
        self.widgets.desk.focus(widget_id, true);
        self.widget_effects();
        true
    }

    /// The keyboard left the surface it was on.
    pub(crate) fn widget_keyboard_leave(&mut self) {
        if let Some(widget_id) = self.widgets.keyboard.take() {
            self.widgets.desk.focus(widget_id, false);
            self.widget_effects();
        }
    }

    /// A key went down or up. Returns whether a widget window or the widget menu took it.
    pub(crate) fn widget_key(&mut self, code: u32, down: bool) -> bool {
        if self.widgets.menu.is_some() {
            if down {
                self.widget_menu_key(code);
            }
            return true;
        }
        let Some(widget_id) = self.widgets.keyboard else {
            return false;
        };
        if down {
            if let Some(key) = key(code) {
                self.widgets.desk.key(widget_id, key);
                self.widget_effects();
            }
        }
        true
    }

    /// Input on a widget's X11 window.
    pub fn x11_widget_input(&mut self, widget_id: i32, input: WidgetInput) {
        // The pointer is measured on the screen, which a move of the window does not change.
        let local_of = |state: &State, root: (i32, i32)| {
            let origin = state
                .widgets
                .desk
                .window(widget_id)
                .map_or((0, 0), |w| w.origin);
            (root.0 - origin.0, root.1 - origin.1)
        };
        match input {
            WidgetInput::Entered { root, .. } => {
                let local = local_of(self, root);
                self.widgets.desk.entered(widget_id, local);
            }
            WidgetInput::Left => self.widgets.desk.left(widget_id),
            WidgetInput::Moved { root, .. } => {
                let local = local_of(self, root);
                self.widgets.desk.moved(widget_id, local);
            }
            WidgetInput::Button {
                detail, down, root, ..
            } => {
                let Some(button) = x11_button(detail) else {
                    return;
                };
                let local = local_of(self, root);
                if down {
                    if self.widgets.menu.is_some() {
                        self.close_widget_menu(false);
                    }
                    self.widgets
                        .desk
                        .down(widget_id, local, button, false, Instant::now());
                } else {
                    self.widgets.desk.up(widget_id, local, button);
                }
            }
            WidgetInput::Key(code) => {
                if let Some(key) = key(code) {
                    self.widgets.desk.key(widget_id, key);
                }
            }
            WidgetInput::Focus(focused) => self.widgets.desk.focus(widget_id, focused),
            WidgetInput::Exposed => {
                self.widgets.dirty.insert(widget_id);
            }
        }
        self.widget_effects();
    }

    /// Whether the X11 window menu open now is a widget's.
    pub(crate) fn widget_menu_is_x11(&self) -> bool {
        self.widgets
            .menu
            .as_ref()
            .is_some_and(|menu| matches!(menu.shell, MenuShell::X11))
    }

    /// Input on the X11 window menu while it is a widget's.
    pub(crate) fn x11_widget_menu_input(&mut self, input: MenuInput) {
        match input {
            MenuInput::Open { .. } => {}
            MenuInput::Moved(position) => self.widget_menu_pointer(position),
            MenuInput::Left => self.widget_menu_highlight(None),
            MenuInput::Exposed => self.draw_widget_menu(),
            MenuInput::Key(code) => self.widget_menu_key(code),
            MenuInput::Button { down, position } => {
                // The menu holds the pointer, so a press off it comes here too, and closes it.
                let inside = self.widgets.menu.as_ref().is_some_and(|menu| {
                    position.0 >= 0.0
                        && position.1 >= 0.0
                        && position.0 < f64::from(menu.layout.width)
                        && position.1 < f64::from(menu.layout.height)
                });
                if down && !inside {
                    self.close_widget_menu(true);
                } else {
                    self.widget_menu_button(position, down);
                }
            }
        }
    }

    /// Opens widget `widget_id`'s menu where `anchor` says, from the last press.
    fn open_widget_menu(
        &mut self,
        widget_id: i32,
        anchor: Anchor,
        entries: &[dsa_host_core::widget_windows::menu::Entry],
    ) {
        self.close_widget_menu(false);
        // One menu is open at a time; on X11 both use the one menu window.
        self.close_menu();
        let Some(window) = self.widgets.windows.get(&widget_id) else {
            return;
        };
        let scale = window.scale.max(1);
        let dpi = dpi_of_scale(scale);
        let rows = rows(entries);
        let Some(text) = self.text.as_ref() else {
            log("widgets: the menu cannot be drawn without Pango; Remove the widget in DSA Settings");
            self.widgets.desk.menu_closed(widget_id, None, false);
            self.widget_effects();
            return;
        };
        let px = f64::from(scaled(TEXT_AT_100, dpi));
        let labels: Vec<Option<Coverage>> = rows
            .iter()
            .map(|row| row.label().and_then(|label| text.line(label, px, None)))
            .collect();
        let check = text.line(CHECK, px, None);
        let widths: Vec<u32> = labels
            .iter()
            .map(|label| label.as_ref().map_or(0, |label| label.width))
            .collect();
        let layout = WidgetMenuLayout::new(rows, &widths, dpi);
        let desk_origin = self
            .widgets
            .desk
            .window(widget_id)
            .map_or((0, 0), |w| w.origin);
        let shell = match &window.shell {
            Shell::X11 => {
                let at = match anchor {
                    Anchor::Below((left, _, _, bottom)) => (left, bottom),
                    Anchor::At(x, y) => (x, y),
                };
                let pixels = paint_widget_menu(&layout, &labels, check.as_ref(), None, 96);
                let size = (layout.width, layout.height);
                let opened = self
                    .x11
                    .as_mut()
                    .is_some_and(|x11| x11.open_menu(&pixels, size, at));
                if !opened {
                    self.widgets.desk.menu_closed(widget_id, None, false);
                    self.widget_effects();
                    return;
                }
                MenuShell::X11
            }
            shell => {
                let Some(seat) = self.seat.clone() else {
                    self.widgets.desk.menu_closed(widget_id, None, false);
                    self.widget_effects();
                    return;
                };
                // The anchor, from the desk's screen pixels to the surface's logical pixels.
                let to_surface = |(x, y): (i32, i32)| {
                    (
                        logical_place(x - desk_origin.0, scale),
                        logical_place(y - desk_origin.1, scale),
                    )
                };
                let surface = self.compositor.create_surface(&self.qh, WidgetMenuSurface);
                let xdg_surface =
                    self.wm_base
                        .get_xdg_surface(&surface, &self.qh, WidgetMenuSurface);
                let positioner = self.wm_base.create_positioner(&self.qh, ());
                positioner.set_size(
                    layout.width.div_ceil(scale).max(1) as i32,
                    layout.height.div_ceil(scale).max(1) as i32,
                );
                match anchor {
                    Anchor::Below((left, top, right, bottom)) => {
                        let (x, y) = to_surface((left, top));
                        let (x2, y2) = to_surface((right, bottom));
                        positioner.set_anchor_rect(x, y, (x2 - x).max(1), (y2 - y).max(1));
                        positioner.set_anchor(PopupAnchor::BottomLeft);
                    }
                    Anchor::At(px, py) => {
                        let (x, y) = to_surface((px, py));
                        positioner.set_anchor_rect(x, y, 1, 1);
                        positioner.set_anchor(PopupAnchor::TopLeft);
                    }
                }
                positioner.set_gravity(Gravity::BottomRight);
                positioner.set_constraint_adjustment(
                    ConstraintAdjustment::FlipX
                        | ConstraintAdjustment::FlipY
                        | ConstraintAdjustment::SlideX
                        | ConstraintAdjustment::SlideY,
                );
                let popup = match shell {
                    Shell::Toplevel {
                        xdg_surface: parent,
                        ..
                    } => xdg_surface.get_popup(
                        Some(parent),
                        &positioner,
                        &self.qh,
                        WidgetMenuSurface,
                    ),
                    Shell::Layer(layer) => {
                        let popup =
                            xdg_surface.get_popup(None, &positioner, &self.qh, WidgetMenuSurface);
                        layer.get_popup(&popup);
                        popup
                    }
                    Shell::X11 => unreachable!("an X11 widget's menu is an X11 window"),
                };
                positioner.destroy();
                popup.grab(&seat, self.widgets.press);
                surface.commit();
                MenuShell::Popup {
                    surface,
                    xdg_surface,
                    popup,
                    buffer: None,
                    configured: false,
                }
            }
        };
        self.widgets.menu = Some(WidgetMenu {
            widget_id,
            layout,
            labels,
            check,
            highlighted: None,
            shell,
            scale: if matches!(self.widgets.shell, WidgetShell::X11) {
                1
            } else {
                scale
            },
            armed: false,
            first: None,
        });
    }

    /// Closes the widget menu, if it is open, and tells the desk it closed with nothing
    /// chosen; `pressed_beside` says a press outside the widget closed it.
    fn close_widget_menu(&mut self, pressed_beside: bool) {
        let Some(widget_id) = self.take_widget_menu() else {
            return;
        };
        self.widgets
            .desk
            .menu_closed(widget_id, None, pressed_beside);
        self.widget_effects();
    }

    /// Takes the widget menu down, and says whose it was.
    fn take_widget_menu(&mut self) -> Option<i32> {
        let menu = self.widgets.menu.take()?;
        match menu.shell {
            MenuShell::Popup {
                surface,
                xdg_surface,
                popup,
                ..
            } => {
                popup.destroy();
                xdg_surface.destroy();
                surface.destroy();
            }
            MenuShell::X11 => {
                if let Some(x11) = &mut self.x11 {
                    x11.close_menu();
                }
            }
        }
        if self.widgets.pointer == Some(On::Menu) {
            self.widgets.pointer = None;
        }
        Some(menu.widget_id)
    }

    fn draw_widget_menu(&mut self) {
        let Some(menu) = self.widgets.menu.as_mut() else {
            return;
        };
        let dpi = dpi_of_scale(menu.scale);
        let size = (menu.layout.width, menu.layout.height);
        let pixels = paint_widget_menu(
            &menu.layout,
            &menu.labels,
            menu.check.as_ref(),
            menu.highlighted,
            dpi,
        );
        match &mut menu.shell {
            MenuShell::X11 => {
                if let Some(x11) = &self.x11 {
                    x11.paint_menu(&pixels, size);
                }
            }
            MenuShell::Popup {
                surface,
                buffer,
                configured,
                ..
            } => {
                if !*configured {
                    return;
                }
                let reuse = buffer
                    .as_ref()
                    .is_some_and(|buffer| buffer.size == size && !buffer.is_busy());
                if !reuse {
                    *buffer =
                        ShmBuffer::with_format(&self.shm, &self.qh, size, Format::Argb8888).ok();
                }
                let Some(buffer) = buffer else {
                    return;
                };
                buffer.write(&pixels);
                buffer.set_busy();
                surface.set_buffer_scale(menu.scale as i32);
                surface.attach(Some(&buffer.buffer), 0, 0);
                surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
                surface.commit();
            }
        }
    }

    fn widget_menu_item_at(&self, (x, y): (f64, f64)) -> Option<usize> {
        let menu = self.widgets.menu.as_ref()?;
        let scale = f64::from(menu.scale);
        menu.layout.item_at(x * scale, y * scale)
    }

    fn widget_menu_highlight(&mut self, item: Option<usize>) {
        let Some(menu) = self.widgets.menu.as_mut() else {
            return;
        };
        if menu.highlighted != item {
            menu.highlighted = item;
            self.draw_widget_menu();
        }
    }

    fn widget_menu_pointer(&mut self, position: (f64, f64)) {
        self.widgets.position = position;
        let item = self.widget_menu_item_at(position);
        let Some(menu) = self.widgets.menu.as_mut() else {
            return;
        };
        match menu.first {
            None => menu.first = Some(position),
            Some((x, y)) => {
                if (position.0 - x).abs() > 4.0 || (position.1 - y).abs() > 4.0 {
                    menu.armed = true;
                }
            }
        }
        self.widget_menu_highlight(item);
    }

    fn widget_menu_button(&mut self, position: (f64, f64), down: bool) {
        let Some(menu) = self.widgets.menu.as_mut() else {
            return;
        };
        if down {
            menu.armed = true;
            return;
        }
        if !menu.armed {
            return;
        }
        if let Some(index) = self.widget_menu_item_at(position) {
            self.widget_menu_choose(index);
        }
    }

    fn widget_menu_key(&mut self, code: u32) {
        const ESC: u32 = 1;
        const ENTER: u32 = 28;
        const SPACE: u32 = 57;
        const UP: u32 = 103;
        const DOWN: u32 = 108;
        const KP_ENTER: u32 = 96;
        let Some((highlighted, next)) = self.widgets.menu.as_ref().map(|menu| {
            let next = matches!(code, UP | DOWN)
                .then(|| menu.layout.step(menu.highlighted, code == DOWN))
                .flatten();
            (menu.highlighted, next)
        }) else {
            return;
        };
        match code {
            // Escape closes the menu; the edit goes on, as on Windows.
            ESC => self.close_widget_menu(false),
            UP | DOWN => self.widget_menu_highlight(next),
            ENTER | KP_ENTER | SPACE => {
                if let Some(index) = highlighted {
                    self.widget_menu_choose(index);
                }
            }
            _ => {}
        }
    }

    fn widget_menu_choose(&mut self, index: usize) {
        let Some(command) = self
            .widgets
            .menu
            .as_ref()
            .and_then(|menu| menu.layout.command(index))
        else {
            return;
        };
        let Some(widget_id) = self.take_widget_menu() else {
            return;
        };
        self.widgets
            .desk
            .menu_closed(widget_id, Some(command), false);
        self.widget_effects();
    }

    /// The widget's surface is on an output of `scale` now.
    fn widget_scale(&mut self, widget_id: i32, scale: u32) {
        let Some(window) = self.widgets.windows.get_mut(&widget_id) else {
            return;
        };
        if window.scale == scale || matches!(window.shell, Shell::X11) {
            return;
        }
        window.scale = scale;
        self.widgets
            .desk
            .scale_changed(widget_id, dpi_of_scale(scale));
        self.widgets.dirty.insert(widget_id);
        self.widget_effects();
    }
}

impl Dispatch<WlSurface, WidgetSurface> for State {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        widget: &WidgetSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let scale = match event {
            wl_surface::Event::Enter { output } => {
                let name = Proxy::data::<u32>(&output).copied();
                name.map(|name| state.output_scale(name))
            }
            wl_surface::Event::PreferredBufferScale { factor } => Some(factor.max(1) as u32),
            _ => None,
        };
        if let Some(scale) = scale {
            state.widget_scale(widget.0, scale);
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, WidgetSurface> for State {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        widget: &WidgetSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let widget_id = widget.0;
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                if let Some(window) = state.widgets.windows.get_mut(&widget_id) {
                    window.configured = true;
                }
                state.widgets.dirty.insert(widget_id);
                state.draw_widgets();
            }
            zwlr_layer_surface_v1::Event::Closed => {
                // The desktop took the surface away, as when its output went: a new one opens
                // where the widget is.
                state.close_widget(widget_id);
                if let Some(window) = state.widgets.desk.window(widget_id) {
                    let (origin, size) = (window.origin, window.size);
                    state.open_widget(widget_id, origin, size);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgSurface, WidgetSurface> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        widget: &WidgetSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some(window) = state.widgets.windows.get_mut(&widget.0) {
                window.configured = true;
            }
            state.widgets.dirty.insert(widget.0);
            state.draw_widgets();
        }
    }
}

impl Dispatch<XdgToplevel, WidgetSurface> for State {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        widget: &WidgetSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The widget keeps its own size; closing its window from the desktop removes nothing,
        // as a widget is removed from its menu or from DSA Settings.
        if let xdg_toplevel::Event::Close = event {
            log(&format!(
                "widget {}: its window stays; remove the widget from its menu or DSA Settings",
                widget.0
            ));
        }
    }
}

impl Dispatch<WlCallback, WidgetMoved> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        moved: &WidgetMoved,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(window) = state.widgets.windows.get_mut(&moved.widget_id) {
                window.applied = moved.origin;
            }
        }
    }
}

impl Dispatch<WlSurface, WidgetMenuSurface> for State {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: wl_surface::Event,
        _: &WidgetMenuSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgSurface, WidgetMenuSurface> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &WidgetMenuSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some(WidgetMenu {
                shell:
                    MenuShell::Popup {
                        xdg_surface: ours,
                        configured,
                        ..
                    },
                ..
            }) = state.widgets.menu.as_mut()
            {
                if *ours == *xdg_surface {
                    *configured = true;
                }
            }
            state.draw_widget_menu();
        }
    }
}

impl Dispatch<XdgPopup, WidgetMenuSurface> for State {
    fn event(
        state: &mut Self,
        popup: &XdgPopup,
        event: xdg_popup::Event,
        _: &WidgetMenuSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            let ours = state.widgets.menu.as_ref().is_some_and(
                |menu| matches!(&menu.shell, MenuShell::Popup { popup: open, .. } if open == popup),
            );
            if ours {
                // The desktop closes a popup when the user clicks outside it, which is beside
                // the widget: an edit ends.
                state.close_widget_menu(true);
            }
        }
    }
}

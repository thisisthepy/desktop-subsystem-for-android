//! The window menu as a popup (`xdg_popup`) of its task window, and what its items do.
//!
//! The menu opens below the icon's cell when the cell is pressed or Alt+Space is pressed, and
//! at the pointer on a right click on the title bar. It takes the pointer and the keyboard
//! while it is open (a popup grab), so a click elsewhere closes it, Escape closes it, the arrow
//! keys move through it and Enter chooses. A press that opened it and its release choose
//! nothing: a release chooses only after a press in the menu, or once the pointer moved in it,
//! so pressing and letting go where the menu opens never picks the item under the pointer.
//!
//! A task in picture-in-picture through X11 has the same menu, opened by Alt+Space, in an X11
//! window of its own (`crate::compositor::x11_pip`).
//!
//! Move and Size arm the window: the next press of the left button (or a touch) in it starts
//! the move, or the resize from the edge nearest the press, and the cursor shows which until
//! then. A key or a press of another button disarms it.
//!
//! Screenshot is taken here: the window's last frame without the title bar's band goes to the
//! clipboard as `image/png` and into the Screenshots folder, encoded on a thread of its own.

use std::io::Write as _;
use std::sync::{Arc, OnceLock};

use dsa_host_core::compositor::window_hosts::FromHost;
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity, XdgPositioner,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};

use super::shm::ShmBuffer;
use super::State;
use crate::compositor::session::log;
use crate::compositor::title_bar::Coverage;
pub use crate::compositor::window_menu::MenuPlace;
use crate::compositor::window_menu::{
    choice_serial, desktop_menu_point, items, label, menu_text, moveresize_direction, paint_menu,
    work, ChosenBy, MenuItem, MenuLayout, MoveSize, WindowWork, TEXT_AT_100,
};
use crate::compositor::x11_pip::MenuInput;
use crate::shell::png::{self, Rgba};
use crate::shell::screenshot;

/// The user data of the menu's surfaces, which no task's window has.
#[derive(Debug, Clone, Copy)]
pub struct MenuSurface;

/// How far the pointer moves in the menu, in logical units, before a release may choose.
const ARMING_MOVE: f64 = 4.0;

/// The evdev key codes the menu answers.
mod key {
    pub const ESC: u32 = 1;
    pub const ENTER: u32 = 28;
    pub const SPACE: u32 = 57;
    pub const UP: u32 = 103;
    pub const DOWN: u32 = 108;
    pub const KP_ENTER: u32 = 96;
}

/// What shows the menu.
// At most one menu is open, so the popup stays inline rather than boxed.
#[allow(clippy::large_enum_variant)]
enum Shell {
    /// A popup of the task's Wayland window.
    Popup {
        surface: WlSurface,
        xdg_surface: XdgSurface,
        popup: XdgPopup,
        buffer: Option<ShmBuffer>,
        configured: bool,
    },
    /// An X11 window, for a task in picture-in-picture through X11; its top left corner on
    /// the screen.
    X11 { origin: (i32, i32) },
}

/// The open window menu.
pub struct Menu {
    pub task_id: i32,
    layout: MenuLayout,
    labels: Vec<Option<Coverage>>,
    highlighted: Option<usize>,
    shell: Shell,
    scale: u32,
    /// Whether a release may choose the item under the pointer.
    armed: bool,
    /// Where the pointer first was in the menu, in logical units.
    first: Option<(f64, f64)>,
    /// Where the menu opened, and the serial of the press or key that opened it.
    place: MenuPlace,
    opened_serial: u32,
    /// The serial of the last press in the menu.
    last_press: Option<u32>,
}

impl Menu {
    fn item_at(&self, (x, y): (f64, f64)) -> Option<usize> {
        let scale = f64::from(self.scale);
        self.layout.item_at(x * scale, y * scale)
    }
}

/// A screenshot on its way to the clipboard and the Screenshots folder.
pub struct Shot {
    image: Rgba,
    png: OnceLock<Vec<u8>>,
}

impl Shot {
    /// The screenshot as a PNG, encoded the first time it is asked for.
    fn png(&self) -> &[u8] {
        self.png.get_or_init(|| png::encode(&self.image, false))
    }
}

impl State {
    /// The menu of `task_id`'s window at `dpi`: its layout and its labels. `None` without
    /// Pango, which the labels are drawn with.
    fn menu_entries(&self, task_id: i32, dpi: u32) -> Option<(MenuLayout, Vec<Option<Coverage>>)> {
        let text = self.text.as_ref()?;
        let window = self.windows.get(&task_id)?;
        let words = menu_text(self.korean);
        let maximized = window.model.states.maximized && window.pip.is_none();
        let entries = items(maximized, window.model.resizable, window.multiple_windows);
        let px = f64::from(dsa_host_core::compositor::caption::scaled(TEXT_AT_100, dpi));
        let labels: Vec<Option<Coverage>> = entries
            .iter()
            .map(|item| label(&words, *item).and_then(|label| text.line(label, px, None)))
            .collect();
        let widths: Vec<u32> = labels
            .iter()
            .map(|label| label.as_ref().map_or(0, |label| label.width))
            .collect();
        Some((MenuLayout::new(entries, &widths, dpi), labels))
    }

    /// Opens the window menu of `task_id`'s window at `place`, from the press or key
    /// `serial`. Returns false when it cannot be drawn (no text, or the window has no title
    /// bar), so the caller opens the desktop's window menu instead.
    pub(crate) fn open_menu(&mut self, task_id: i32, place: MenuPlace, serial: u32) -> bool {
        self.close_menu();
        let Some(seat) = self.seat.clone() else {
            return false;
        };
        let Some(window) = self.windows.get(&task_id) else {
            return false;
        };
        if window.pip.is_some() || !window.model.has_caption() {
            return false;
        }
        let scale = window.model.scale.max(1);
        let dpi = window.model.dpi();
        let Some((layout, labels)) = self.menu_entries(task_id, dpi) else {
            return false;
        };
        let Some(window) = self.windows.get(&task_id) else {
            return false;
        };
        let (width, height) = (
            layout.width.div_ceil(scale) as i32,
            layout.height.div_ceil(scale) as i32,
        );

        let surface = self.compositor.create_surface(&self.qh, MenuSurface);
        let xdg_surface = self
            .wm_base
            .get_xdg_surface(&surface, &self.qh, MenuSurface);
        let positioner = self.wm_base.create_positioner(&self.qh, ());
        positioner.set_size(width.max(1), height.max(1));
        match place {
            MenuPlace::Below(left, top, cell_width, cell_height) => {
                positioner.set_anchor_rect(left, top, cell_width.max(1), cell_height.max(1));
                positioner.set_anchor(Anchor::BottomLeft);
            }
            MenuPlace::At(x, y) => {
                positioner.set_anchor_rect(x, y, 1, 1);
                positioner.set_anchor(Anchor::TopLeft);
            }
        }
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_constraint_adjustment(
            ConstraintAdjustment::FlipX
                | ConstraintAdjustment::FlipY
                | ConstraintAdjustment::SlideX
                | ConstraintAdjustment::SlideY,
        );
        let popup = xdg_surface.get_popup(
            Some(window.xdg_surface()),
            &positioner,
            &self.qh,
            MenuSurface,
        );
        positioner.destroy();
        popup.grab(&seat, serial);
        surface.commit();
        self.menu = Some(Menu {
            task_id,
            layout,
            labels,
            highlighted: None,
            shell: Shell::Popup {
                surface,
                xdg_surface,
                popup,
                buffer: None,
                configured: false,
            },
            scale,
            armed: false,
            first: None,
            place,
            opened_serial: serial,
            last_press: None,
        });
        true
    }

    /// Opens the window menu of `task_id`'s picture-in-picture window through X11, at its
    /// top left corner, as Alt+Space opens the system menu on Windows. Without Pango to draw
    /// it, the window manager's own window menu opens instead.
    fn open_x11_menu(&mut self, task_id: i32) {
        self.close_menu();
        let Some(origin) = self.x11.as_ref().and_then(|x11| x11.window_origin(task_id)) else {
            return;
        };
        let dpi = dsa_host_core::compositor::window_sizing::BASE_DPI;
        let Some((layout, labels)) = self.menu_entries(task_id, dpi) else {
            if let Some(Err(why)) = self
                .x11
                .as_ref()
                .map(|x11| x11.show_window_menu(task_id, origin))
            {
                log(&format!("task {task_id}: no window menu could open: {why}"));
            }
            return;
        };
        let pixels = paint_menu(&layout, &labels, None, dpi);
        let size = (layout.width, layout.height);
        let opened = self
            .x11
            .as_mut()
            .is_some_and(|x11| x11.open_menu(&pixels, size, origin));
        if !opened {
            return;
        }
        self.menu = Some(Menu {
            task_id,
            layout,
            labels,
            highlighted: None,
            shell: Shell::X11 { origin },
            scale: 1,
            armed: false,
            first: None,
            place: MenuPlace::At(0, 0),
            opened_serial: self.serial,
            last_press: None,
        });
    }

    /// Input on the X11 window menu, or Alt+Space in an X11 picture-in-picture window.
    pub fn x11_menu_input(&mut self, input: MenuInput) {
        // The menu's X11 window shows a widget's menu too, on a desktop where widgets are
        // X11 windows.
        if self.widget_menu_is_x11() {
            self.x11_widget_menu_input(input);
            return;
        }
        let serial = self.serial;
        match input {
            MenuInput::Open { task_id } => self.open_x11_menu(task_id),
            MenuInput::Moved(position) => self.menu_pointer(position),
            MenuInput::Left => self.menu_pointer_left(),
            MenuInput::Exposed => self.draw_menu(),
            MenuInput::Key(code) => self.menu_key(serial, code),
            MenuInput::Button { down, position } => {
                // The menu holds the pointer, so a press off it comes here too, and closes it.
                let inside = self.menu.as_ref().is_some_and(|menu| {
                    position.0 >= 0.0
                        && position.1 >= 0.0
                        && position.0 < f64::from(menu.layout.width)
                        && position.1 < f64::from(menu.layout.height)
                });
                if down && !inside {
                    self.close_menu();
                } else {
                    self.menu_button(serial, position, down);
                }
            }
        }
    }

    /// Closes the window menu, if it is open.
    pub(crate) fn close_menu(&mut self) {
        if let Some(menu) = self.menu.take() {
            match menu.shell {
                Shell::Popup {
                    surface,
                    xdg_surface,
                    popup,
                    ..
                } => {
                    popup.destroy();
                    xdg_surface.destroy();
                    surface.destroy();
                }
                Shell::X11 { .. } => {
                    if let Some(x11) = &mut self.x11 {
                        x11.close_menu();
                    }
                }
            }
        }
        self.seat_state.on_menu = false;
    }

    fn draw_menu(&mut self) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        let dpi = dsa_host_core::compositor::window_sizing::BASE_DPI * menu.scale;
        let size = (menu.layout.width, menu.layout.height);
        match &mut menu.shell {
            Shell::X11 { .. } => {
                let pixels = paint_menu(&menu.layout, &menu.labels, menu.highlighted, dpi);
                if let Some(x11) = &self.x11 {
                    x11.paint_menu(&pixels, size);
                }
            }
            Shell::Popup {
                surface,
                buffer,
                configured,
                ..
            } => {
                if !*configured {
                    return;
                }
                let pixels = paint_menu(&menu.layout, &menu.labels, menu.highlighted, dpi);
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

    fn highlight(&mut self, item: Option<usize>) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        if menu.highlighted != item {
            menu.highlighted = item;
            self.draw_menu();
        }
    }

    /// The pointer is at `position` (logical units) on the menu.
    pub(crate) fn menu_pointer(&mut self, position: (f64, f64)) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        match menu.first {
            None => menu.first = Some(position),
            Some((x, y)) => {
                if (position.0 - x).abs() > ARMING_MOVE || (position.1 - y).abs() > ARMING_MOVE {
                    menu.armed = true;
                }
            }
        }
        let item = menu.item_at(position);
        self.highlight(item);
    }

    /// The pointer left the menu.
    pub(crate) fn menu_pointer_left(&mut self) {
        self.highlight(None);
    }

    /// A button went down or up at `position` (logical units) on the menu.
    pub(crate) fn menu_button(&mut self, serial: u32, position: (f64, f64), down: bool) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        if down {
            menu.armed = true;
            menu.last_press = Some(serial);
            return;
        }
        if !menu.armed {
            return;
        }
        if let Some(index) = menu.item_at(position) {
            self.choose(index, serial, ChosenBy::Pointer);
        }
    }

    /// A touch on the menu at `position` (logical units) chooses what it touched.
    pub(crate) fn menu_touch(&mut self, serial: u32, position: (f64, f64)) {
        let Some(index) = self.menu.as_ref().and_then(|menu| menu.item_at(position)) else {
            return;
        };
        self.choose(index, serial, ChosenBy::Touch);
    }

    /// A key went down while the menu is open; the menu takes every key.
    pub(crate) fn menu_key(&mut self, serial: u32, code: u32) {
        let Some((highlighted, next)) = self.menu.as_ref().map(|menu| {
            let next = matches!(code, key::UP | key::DOWN)
                .then(|| menu.layout.step(menu.highlighted, code == key::DOWN))
                .flatten();
            (menu.highlighted, next)
        }) else {
            return;
        };
        match code {
            key::ESC => self.close_menu(),
            key::UP | key::DOWN => self.highlight(next),
            key::ENTER | key::KP_ENTER | key::SPACE => {
                if let Some(index) = highlighted {
                    self.choose(index, serial, ChosenBy::Keyboard);
                }
            }
            _ => {}
        }
    }

    /// Carries out the menu's entry `index`, chosen `by` the input event `serial`.
    fn choose(&mut self, index: usize, serial: u32, by: ChosenBy) {
        let Some((task_id, item, serial, place, x11_origin)) =
            self.menu.as_ref().and_then(|menu| {
                let item = *menu.layout.items.get(index)?;
                let serial = choice_serial(by, serial, menu.last_press, menu.opened_serial);
                let x11_origin = match menu.shell {
                    Shell::X11 { origin } => Some(origin),
                    Shell::Popup { .. } => None,
                };
                Some((menu.task_id, item, serial, menu.place, x11_origin))
            })
        else {
            return;
        };
        if item == MenuItem::Separator {
            return;
        }
        // The clipboard takes the serial while the menu still has the keyboard.
        if item == MenuItem::Screenshot {
            self.screenshot(task_id, serial);
        }
        self.close_menu();
        if let Some(origin) = x11_origin {
            self.x11_menu_work(task_id, work(item), origin);
            return;
        }
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        match work(item) {
            WindowWork::Minimize => window.toplevel.set_minimized(),
            WindowWork::Maximize => window.toplevel.set_maximized(),
            WindowWork::Restore => window.toplevel.unset_maximized(),
            WindowWork::Close => self.report(FromHost::Closed { task_id }),
            WindowWork::MoveSize(what) => {
                // The desktop starts a move or a resize only from a held press, which a
                // menu's choice is not: the next press in the window starts it.
                window.armed = Some(what);
                self.seat_state.reset_shape();
            }
            WindowWork::DesktopMenu => {
                self.desktop_window_menu_at(task_id, serial, desktop_menu_point(place));
            }
            WindowWork::Screenshot | WindowWork::Nothing => {}
            WindowWork::Android(action) => {
                self.report(FromHost::MenuChosen { task_id, action });
            }
        }
    }

    /// Carries out an entry of the X11 window menu, whose window manager does the window's
    /// part.
    fn x11_menu_work(&mut self, task_id: i32, work: WindowWork, origin: (i32, i32)) {
        let Some(x11) = &self.x11 else {
            return;
        };
        let failed = match work {
            WindowWork::Minimize => {
                x11.minimize(task_id);
                Ok(())
            }
            WindowWork::Maximize | WindowWork::Restore => {
                x11.maximize(task_id, work == WindowWork::Maximize);
                Ok(())
            }
            WindowWork::MoveSize(what) => {
                x11.move_resize(task_id, moveresize_direction(what, false))
            }
            WindowWork::DesktopMenu => x11.show_window_menu(task_id, origin),
            WindowWork::Close => {
                self.report(FromHost::Closed { task_id });
                Ok(())
            }
            WindowWork::Android(action) => {
                self.report(FromHost::MenuChosen { task_id, action });
                Ok(())
            }
            WindowWork::Screenshot | WindowWork::Nothing => Ok(()),
        };
        if let Err(why) = failed {
            log(&format!(
                "task {task_id}: the window menu's item did nothing: {why}"
            ));
        }
    }

    /// Starts the move or resize the window menu armed `task_id`'s window for, from the press
    /// or touch `serial` at `position` (logical units). Returns whether one was armed.
    pub(crate) fn start_armed(&mut self, task_id: i32, serial: u32, position: (f64, f64)) -> bool {
        let Some(seat) = self.seat.clone() else {
            return false;
        };
        let Some(window) = self.windows.get_mut(&task_id) else {
            return false;
        };
        let Some(what) = window.armed.take() else {
            return false;
        };
        match what {
            MoveSize::Move => window.toplevel._move(&seat, serial),
            MoveSize::Size => {
                let (width, height) = window.model.to_logical(window.model.shown);
                let edge = crate::compositor::window_menu::size_edge(
                    position,
                    (f64::from(width), f64::from(height)),
                );
                if let Ok(edge) =
                    wayland_protocols::xdg::shell::client::xdg_toplevel::ResizeEdge::try_from(edge)
                {
                    window.toplevel.resize(&seat, serial, edge);
                }
            }
        }
        true
    }

    /// Takes back what the window menu armed in any window.
    pub(crate) fn disarm(&mut self) {
        for window in self.windows.values_mut() {
            window.armed = None;
        }
    }

    /// Takes a screenshot of `task_id`'s window: its last frame without the title bar's
    /// band, to the clipboard (from the input event `serial`) and the Screenshots folder.
    fn screenshot(&mut self, task_id: i32, serial: u32) {
        let Some(window) = self.windows.get(&task_id) else {
            return;
        };
        // The frame is the task's, in its pixels: the band covers fewer or more of its rows
        // when the window shows it at another scale.
        let band = window.model.caption_rows_in_frame();
        // A GPU frame is read back from under the band only; a frame in memory has it all.
        let last_gpu = window.last_gpu;
        let frame = match last_gpu {
            Some((display_id, frame)) => self
                .read_gpu_frame(display_id, frame, band)
                .map(|frame| (frame, 0)),
            None => self
                .windows
                .get(&task_id)
                .and_then(|window| window.last.clone())
                .map(|frame| (frame, band))
                .ok_or_else(|| "the app has not drawn its window yet".to_string()),
        };
        let image = frame.and_then(|(frame, band)| {
            screenshot::app_content(&frame.pixels, frame.width, frame.height, band)
                .ok_or_else(|| "the window shows nothing under its title bar".to_string())
        });
        let image = match image {
            Ok(image) => image,
            Err(why) => {
                log(&format!(
                    "task {task_id}: the screenshot was not taken: {why}"
                ));
                return;
            }
        };
        let shot = Arc::new(Shot {
            image,
            png: OnceLock::new(),
        });
        self.set_clipboard_shot(Arc::clone(&shot), serial);
        let prefix = menu_text(self.korean).screenshot_file;
        let spawned = std::thread::Builder::new()
            .name("dsa-screenshot".into())
            .spawn(move || match screenshot::save(shot.png(), prefix) {
                Ok(path) => log(&format!(
                    "task {task_id}: screenshot saved as {}",
                    path.display()
                )),
                Err(why) => log(&format!(
                    "task {task_id}: the screenshot was not saved: {why}"
                )),
            });
        if let Err(why) = spawned {
            log(&format!("the screenshot could not be saved: {why}"));
        }
    }

    /// The screenshot becomes the desktop's clipboard, as `image/png`.
    fn set_clipboard_shot(&mut self, shot: Arc<Shot>, serial: u32) {
        let (Some(manager), Some(device)) = (&self.data_manager, &self.data_device) else {
            return;
        };
        let source = manager.create_data_source(&self.qh, shot);
        source.offer("image/png".to_string());
        device.set_selection(Some(&source), serial);
        self.data_state.replace_source(source);
    }
}

impl Dispatch<WlSurface, MenuSurface> for State {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: wl_surface::Event,
        _: &MenuSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgSurface, MenuSurface> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &MenuSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some(Menu {
                shell:
                    Shell::Popup {
                        xdg_surface: ours,
                        configured,
                        ..
                    },
                ..
            }) = state.menu.as_mut()
            {
                if *ours == *xdg_surface {
                    *configured = true;
                }
            }
            state.draw_menu();
        }
    }
}

impl Dispatch<XdgPopup, MenuSurface> for State {
    fn event(
        state: &mut Self,
        popup: &XdgPopup,
        event: xdg_popup::Event,
        _: &MenuSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            let ours = state.menu.as_ref().is_some_and(
                |menu| matches!(&menu.shell, Shell::Popup { popup: open, .. } if open == popup),
            );
            if ours {
                state.close_menu();
            }
        }
    }
}

delegate_noop!(State: ignore XdgPositioner);

impl Dispatch<WlDataSource, Arc<Shot>> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        shot: &Arc<Shot>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let shot = Arc::clone(shot);
                std::thread::spawn(move || {
                    let mut file = std::fs::File::from(fd);
                    let _ = file.write_all(shot.png());
                });
            }
            wl_data_source::Event::Cancelled => {
                state.data_state.source_cancelled(source);
                source.destroy();
            }
            _ => {}
        }
    }
}

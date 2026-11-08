//! The seat: pointer, keyboard, touch, pen and input method, turned into the guest's input.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::caption::{CaptionButton, CaptionHit};
use dsa_host_core::compositor::window_hosts::FromHost;
use dsa_host_core::input::{buttons, ContactPhase, PointerTool, WindowInput};
use dsa_host_core::keys::{keycode, KeyInput};
use wayland_client::protocol::{
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_seat::{self, WlSeat},
    wl_shm::Format,
    wl_surface::WlSurface,
    wl_touch::{self, WlTouch},
};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::ZwpTabletV2,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};
use wayland_protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;

use super::menu::{MenuPlace, MenuSurface};
use super::shm::ShmBuffer;
use super::{task_of, Event, State};
use crate::compositor::window_menu::{desktop_menu_point, opens_window_menu, size_edge, MoveSize};
use crate::input::keymap::{self, evdev_key, guest_key};
use crate::input::pointer::{evdev_button, mouse_button, window_hit, Scroll, WindowHit};
use crate::input::xkb::Typed;

/// How long two clicks on the title bar may be apart to count as a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What a held key repeats.
#[derive(Debug, Clone)]
enum Repeating {
    Text(String),
    Key(u32),
}

/// A pen's state between `zwp_tablet_tool_v2.frame`s.
#[derive(Debug, Default)]
struct Pen {
    task: Option<i32>,
    position: (f64, f64),
    pressure: Option<f32>,
    down: bool,
    was_down: bool,
    left: bool,
    eraser: bool,
    buttons: u32,
    /// Degrees from upright, (toward the right, toward the user).
    tilt: (f32, f32),
}

/// The seat's input state.
pub struct SeatState {
    pub pointer_focus: Option<i32>,
    pub keyboard_focus: Option<i32>,
    /// The pointer's position on its window, in logical units.
    position: (f64, f64),
    /// Where the surface the pointer is on sits in its window.
    offset: (f64, f64),
    scroll: Scroll,
    /// The caption button a press started on, while it is held.
    pressed_button: Option<CaptionButton>,
    /// Whether a press started on the back arrow, while it is held.
    pressed_back: bool,
    /// Whether the pointer is on the window menu, and where on it, in logical units.
    pub(crate) on_menu: bool,
    menu_position: (f64, f64),
    /// Whether the window's app has the pointer's buttons, so the title bar keeps out.
    pointer_in_app: bool,
    shape: Option<Shape>,
    enter_serial: u32,
    touches: HashMap<i32, (i32, (f64, f64))>,
    pen: Pen,
    /// The locks on, as the guest names them.
    locks: u32,
    repeat_rate: i32,
    repeat_delay: i32,
    repeat: Option<(u32, Repeating, Instant)>,
    /// The time of the last key event, and when it came, to time repeats and releases.
    key_clock: (u32, Instant),
    /// The arrow shown where the desktop has no cursor shapes to name.
    arrow: Option<(WlSurface, ShmBuffer)>,
    /// A key whose press opened the window menu, so its release goes nowhere.
    swallow_release: Option<u32>,
}

impl SeatState {
    /// Forgets the cursor shape set last, so the next motion sets one again.
    pub(crate) fn reset_shape(&mut self) {
        self.shape = None;
    }
}

impl Default for SeatState {
    fn default() -> SeatState {
        SeatState {
            pointer_focus: None,
            keyboard_focus: None,
            position: (0.0, 0.0),
            offset: (0.0, 0.0),
            scroll: Scroll::default(),
            pressed_button: None,
            pressed_back: false,
            on_menu: false,
            menu_position: (0.0, 0.0),
            pointer_in_app: false,
            shape: None,
            enter_serial: 0,
            touches: HashMap::new(),
            pen: Pen::default(),
            locks: 0,
            repeat_rate: 0,
            repeat_delay: 0,
            repeat: None,
            key_clock: (0, Instant::now()),
            arrow: None,
            swallow_release: None,
        }
    }
}

impl State {
    pub(crate) fn seat_ready(&mut self, seat: WlSeat) {
        if let Some(manager) = &self.data_manager {
            self.data_device = Some(manager.get_data_device(&seat, &self.qh, ()));
        }
        if let Some(manager) = &self.text_input_manager {
            self.text_input = Some(manager.get_text_input(&seat, &self.qh, ()));
        }
        if let Some(manager) = &self.tablet_manager {
            manager.get_tablet_seat(&seat, &self.qh, ());
        }
        self.seat = Some(seat);
    }

    /// The time of the desktop's clock (in milliseconds, as key events carry it) now.
    fn key_time(&self, now: Instant) -> u32 {
        let (time, at) = self.seat_state.key_clock;
        time.wrapping_add(now.saturating_duration_since(at).as_millis() as u32)
    }

    pub(crate) fn pointer_input(&mut self, task_id: i32, input: WindowInput) {
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        let geometry = window.model.geometry();
        let mapped = window.pointer.handle(input, geometry);
        for event in mapped.events {
            self.input(event);
        }
    }

    fn set_shape(&mut self, shape: Shape) {
        if self.seat_state.shape == Some(shape) {
            return;
        }
        self.seat_state.shape = Some(shape);
        if let Some(device) = &self.cursor_device {
            device.set_shape(self.seat_state.enter_serial, shape);
            return;
        }
        // No cursor shapes: an arrow of our own, the same over the whole window.
        if self.seat_state.arrow.is_none() {
            let surface = self.compositor.create_surface(&self.qh, -1);
            match ShmBuffer::with_format(&self.shm, &self.qh, ARROW_SIZE, Format::Argb8888) {
                Ok(mut buffer) => {
                    buffer.write(&arrow_pixels());
                    surface.attach(Some(&buffer.buffer), 0, 0);
                    surface.damage_buffer(0, 0, ARROW_SIZE.0 as i32, ARROW_SIZE.1 as i32);
                    surface.commit();
                    self.seat_state.arrow = Some((surface, buffer));
                }
                Err(_) => return,
            }
        }
        if let (Some(pointer), Some((surface, _))) = (&self.pointer, &self.seat_state.arrow) {
            pointer.set_cursor(self.seat_state.enter_serial, Some(surface), 0, 0);
        }
    }

    fn pointer_moved(&mut self, task_id: i32) {
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        let (x, y) = window.model.point_to_pixels(self.seat_state.position);
        let mut hit = WindowHit::Caption(CaptionHit::Client);
        if window.model.has_caption() && !self.seat_state.pointer_in_app {
            let border = window.model.caption_layout().resize_border;
            hit = window_hit(
                &window.model.caption_layout(),
                window.model.shown,
                border,
                x as i32,
                y as i32,
                &window.title_bar,
            );
        }
        let hovered = match hit {
            WindowHit::Caption(CaptionHit::Button(button)) => Some(button),
            _ => None,
        };
        let mut changed = window.hover(hovered);
        changed |= window.title_bar.hover_back(hit == WindowHit::Back);
        let shape = match (window.armed, hit) {
            // Move or Size from the window menu waits for a press: the cursor says which.
            (Some(MoveSize::Move), _) => Shape::Move,
            (Some(MoveSize::Size), _) => {
                let (width, height) = window.model.to_logical(window.model.shown);
                edge_shape(size_edge(
                    self.seat_state.position,
                    (f64::from(width), f64::from(height)),
                ))
            }
            (None, WindowHit::Resize(edge)) => edge_shape(edge as u32),
            (None, _) => Shape::Default,
        };
        self.set_shape(shape);
        if changed {
            self.redraw(task_id);
        }
        if hit == WindowHit::Caption(CaptionHit::Client) {
            self.pointer_input(task_id, WindowInput::MouseMove { position: (x, y) });
            self.watch_drag_out(task_id, (x, y));
        } else if !self.seat_state.pointer_in_app {
            self.pointer_input(task_id, WindowInput::MouseLeave);
        }
    }

    /// Tells the session when a press of the left button has just left the window, which may
    /// carry an app's drag out of it.
    fn watch_drag_out(&mut self, task_id: i32, position: (f64, f64)) {
        let Some(window) = self.windows.get(&task_id) else {
            return;
        };
        let geometry = window.model.geometry();
        let Some(inside) = self.data_state.leave.moved(position, geometry.client) else {
            return;
        };
        let (x, y) = geometry.to_guest(inside);
        self.events.send(Event::DragOut {
            out: dsa_host_core::drag::DragOut {
                task_id,
                x,
                y,
                tool: dsa_protocol::control::PointerTool::Mouse as i32,
                pointer_id: 0,
            },
            serial: self.data_state.press_serial,
        });
    }

    fn pointer_button(&mut self, task_id: i32, serial: u32, code: u32, down: bool) {
        self.serial = serial;
        // Move or Size from the window menu starts with this press, held; another button
        // takes it back.
        let armed = self
            .windows
            .get(&task_id)
            .is_some_and(|window| window.armed.is_some());
        if armed && down {
            if code == evdev_button::LEFT {
                let position = self.seat_state.position;
                self.start_armed(task_id, serial, position);
                self.seat_state.reset_shape();
                return;
            }
            self.disarm();
            self.seat_state.reset_shape();
        }
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        let (x, y) = window.model.point_to_pixels(self.seat_state.position);
        let in_app = self.seat_state.pointer_in_app;
        let hit = if window.model.has_caption() && !in_app {
            let layout = window.model.caption_layout();
            window_hit(
                &layout,
                window.model.shown,
                layout.resize_border,
                x as i32,
                y as i32,
                &window.title_bar,
            )
        } else {
            WindowHit::Caption(CaptionHit::Client)
        };
        let left = code == evdev_button::LEFT;
        let right = code == evdev_button::RIGHT;
        // A press on the back arrow that ends elsewhere sends nothing.
        if left && !down && hit != WindowHit::Back && self.seat_state.pressed_back {
            self.seat_state.pressed_back = false;
            if window.title_bar.press_back(false) {
                self.redraw(task_id);
            }
            return;
        }
        match hit {
            WindowHit::Caption(CaptionHit::Client) => {
                if left {
                    self.data_state.leave.pressed(down);
                    if down {
                        self.data_state.press_serial = serial;
                    }
                }
                if let Some(button) = mouse_button(code) {
                    let input = WindowInput::MouseButton {
                        button,
                        down,
                        position: (x, y),
                    };
                    let geometry = window.model.geometry();
                    let mapped = window.pointer.handle(input, geometry);
                    self.seat_state.pointer_in_app = window.pointer.buttons() != 0;
                    for event in mapped.events {
                        self.input(event);
                    }
                }
            }
            WindowHit::Resize(edge) if left && down => {
                if let (Some(seat), Ok(edge)) = (&self.seat, ResizeEdge::try_from(edge as u32)) {
                    window.toplevel.resize(seat, serial, edge);
                }
            }
            WindowHit::Caption(CaptionHit::Caption) if down => {
                let Some(seat) = self.seat.clone() else {
                    return;
                };
                if left {
                    let now = Instant::now();
                    let double = window
                        .last_caption_click
                        .is_some_and(|at| now.duration_since(at) < DOUBLE_CLICK);
                    window.last_caption_click = Some(now);
                    if double {
                        if window.model.states.maximized {
                            window.toplevel.unset_maximized();
                        } else {
                            window.toplevel.set_maximized();
                        }
                    } else {
                        window.toplevel._move(&seat, serial);
                    }
                } else if right {
                    self.window_menu_at_pointer(task_id, serial);
                }
            }
            WindowHit::Caption(CaptionHit::Button(button)) if left => {
                if down {
                    self.seat_state.pressed_button = Some(button);
                    window.caption.pressed = Some(button);
                } else {
                    window.caption.pressed = None;
                    if self.seat_state.pressed_button.take() == Some(button) {
                        match button {
                            CaptionButton::Minimize => window.toplevel.set_minimized(),
                            CaptionButton::Maximize if window.model.states.maximized => {
                                window.toplevel.unset_maximized()
                            }
                            CaptionButton::Maximize => window.toplevel.set_maximized(),
                            CaptionButton::Close => self.report(FromHost::Closed { task_id }),
                        }
                    }
                }
                self.redraw(task_id);
            }
            // The icon's cell opens the window menu below itself, as a caption icon does.
            WindowHit::WindowMenu if left && down => {
                let layout = window.model.caption_layout();
                let scale = window.model.scale.max(1) as i32;
                if let Some((cell_left, top, width, height)) = layout.icon_cell() {
                    let place = MenuPlace::Below(
                        cell_left / scale,
                        top / scale,
                        width.div_ceil(scale as u32) as i32,
                        height.div_ceil(scale as u32) as i32,
                    );
                    if !self.open_menu(task_id, place, serial) {
                        self.desktop_window_menu(task_id, serial);
                    }
                }
            }
            // While the task can go back, the cell is the back arrow: a click sends Back.
            WindowHit::Back if left => {
                if down {
                    self.seat_state.pressed_back = true;
                    window.title_bar.press_back(true);
                    self.redraw(task_id);
                } else {
                    window.title_bar.press_back(false);
                    let clicked = std::mem::take(&mut self.seat_state.pressed_back);
                    self.redraw(task_id);
                    if clicked {
                        let time = self.key_time(Instant::now());
                        self.send_back(task_id, time);
                    }
                }
            }
            // A right click on the icon's cell opens the window menu too, as on the band.
            WindowHit::WindowMenu | WindowHit::Back if right && down => {
                self.window_menu_at_pointer(task_id, serial);
            }
            _ => {
                if !down {
                    self.seat_state.pressed_button = None;
                }
            }
        }
    }

    /// Opens the window menu where the pointer is, from the press `serial`; the desktop's own
    /// window menu where the window cannot draw its own.
    fn window_menu_at_pointer(&mut self, task_id: i32, serial: u32) {
        let (x, y) = self.seat_state.position;
        if !self.open_menu(task_id, MenuPlace::At(x as i32, y as i32), serial) {
            self.desktop_window_menu(task_id, serial);
        }
    }

    /// The desktop's own window menu, at the pointer.
    fn desktop_window_menu(&mut self, task_id: i32, serial: u32) {
        let (x, y) = self.seat_state.position;
        self.desktop_window_menu_at(task_id, serial, (x as i32, y as i32));
    }

    /// The desktop's own window menu at `at` (logical units in the window), from the input
    /// event `serial`. A desktop that has no window menu, or takes no such serial, shows
    /// nothing; Wayland gives the window no word of it.
    pub(crate) fn desktop_window_menu_at(&mut self, task_id: i32, serial: u32, at: (i32, i32)) {
        if let (Some(seat), Some(window)) = (&self.seat, self.windows.get(&task_id)) {
            window.toplevel.show_window_menu(seat, serial, at.0, at.1);
        }
    }

    /// Alt+Space: the window menu below the icon's cell, as the system menu opens on Windows,
    /// from the key's `serial`; the desktop's own where the window cannot draw its own (full
    /// screen, or no Pango).
    fn window_menu_from_key(&mut self, task_id: i32, serial: u32) {
        let Some(window) = self.windows.get(&task_id) else {
            return;
        };
        let layout = window.model.caption_layout();
        let scale = window.model.scale.max(1) as i32;
        let place = match layout.icon_cell() {
            Some((cell_left, top, width, height)) if window.model.has_caption() => {
                MenuPlace::Below(
                    cell_left / scale,
                    top / scale,
                    width.div_ceil(scale as u32) as i32,
                    height.div_ceil(scale as u32) as i32,
                )
            }
            _ => MenuPlace::At(0, 0),
        };
        if !self.open_menu(task_id, place, serial) {
            self.desktop_window_menu_at(task_id, serial, desktop_menu_point(place));
        }
    }

    /// Sends the task Back, pressed and released, through the window's keys as any key goes,
    /// as the back arrow in the title bar does.
    pub(crate) fn send_back(&mut self, task_id: i32, time_ms: u32) {
        self.send_key(task_id, keycode::BACK, true, time_ms);
        self.send_key(task_id, keycode::BACK, false, time_ms);
    }

    /// Does what a click on a title bar button does, for an accessibility client that
    /// activated the button. Back goes only while the title bar shows the back arrow.
    pub fn title_bar_action(
        &mut self,
        task_id: i32,
        action: crate::accessibility::objects::TitleAction,
    ) {
        use crate::accessibility::objects::TitleAction;
        let Some(window) = self.windows.get(&task_id) else {
            return;
        };
        match action {
            TitleAction::Back => {
                if window.title_bar.back {
                    let time = self.key_time(Instant::now());
                    self.send_back(task_id, time);
                }
            }
            TitleAction::Minimize => window.toplevel.set_minimized(),
            TitleAction::Maximize => window.toplevel.set_maximized(),
            TitleAction::Restore => window.toplevel.unset_maximized(),
            TitleAction::Close => self.report(FromHost::Closed { task_id }),
        }
    }

    fn key(&mut self, serial: u32, time: u32, code: u32, down: bool) {
        self.serial = serial;
        // The release of the key that opened the window menu goes nowhere.
        if self.seat_state.swallow_release == Some(code) {
            self.seat_state.swallow_release = None;
            if !down {
                return;
            }
        }
        // The window menu takes the keyboard while it is open.
        if self.menu.is_some() {
            if down {
                self.menu_key(serial, code);
            }
            return;
        }
        let Some(task_id) = self.seat_state.keyboard_focus else {
            return;
        };
        if down {
            // A key takes back Move or Size armed from the window menu.
            if self.windows.values().any(|window| window.armed.is_some()) {
                self.disarm();
                self.seat_state.reset_shape();
            }
        }
        if code == evdev_key::F11 && down {
            self.toggle_fullscreen(task_id);
            return;
        }
        let (alt, ctrl) = match self.xkb.as_ref() {
            Some(xkb) => (xkb.alt(), xkb.ctrl()),
            None => (false, false),
        };
        if down && opens_window_menu(code, alt, ctrl) {
            self.seat_state.swallow_release = Some(code);
            self.window_menu_from_key(task_id, serial);
            return;
        }
        self.seat_state.key_clock = (time, Instant::now());
        let repeated = self.seat_state.repeat.as_ref().map(|(key, _, _)| *key);
        if !down && repeated == Some(code) {
            self.seat_state.repeat = None;
        }
        if down {
            let typed = self.xkb.as_mut().map(|xkb| xkb.press(code));
            match typed {
                Some(Typed::Text(text)) => {
                    let events = match self.windows.get_mut(&task_id) {
                        Some(window) => window.ime.typed(&text),
                        None => return,
                    };
                    for event in events {
                        self.input(event);
                    }
                    self.start_repeat(code, Repeating::Text(text));
                    return;
                }
                Some(Typed::Composing) => return,
                Some(Typed::Nothing) | None => {}
            }
        }
        let Some(key_code) = guest_key(code) else {
            return;
        };
        self.send_key(task_id, key_code, down, time);
        if down && dsa_host_core::keys::meta::of_modifier(key_code) == 0 {
            self.start_repeat(code, Repeating::Key(key_code));
        }
    }

    fn send_key(&mut self, task_id: i32, key_code: u32, down: bool, time_ms: u32) {
        let locks = self.seat_state.locks;
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        let event = window.keys.handle(KeyInput {
            key_code,
            down,
            locks,
            time_ms,
        });
        if let Some(event) = event {
            self.input(event);
        }
    }

    fn start_repeat(&mut self, code: u32, what: Repeating) {
        if self.seat_state.repeat_rate <= 0 {
            self.seat_state.repeat = None;
            return;
        }
        let delay = Duration::from_millis(self.seat_state.repeat_delay.max(0) as u64);
        self.seat_state.repeat = Some((code, what, Instant::now() + delay));
    }

    /// Repeats the held key, at the rate the desktop asks for.
    pub(crate) fn repeat_due(&mut self, now: Instant) {
        let Some((code, what, at)) = self.seat_state.repeat.clone() else {
            return;
        };
        if now < at {
            return;
        }
        let Some(task_id) = self.seat_state.keyboard_focus else {
            self.seat_state.repeat = None;
            return;
        };
        let interval = Duration::from_millis(1000 / self.seat_state.repeat_rate.max(1) as u64);
        self.seat_state.repeat = Some((code, what.clone(), now + interval));
        match what {
            Repeating::Text(text) => {
                let events = match self.windows.get_mut(&task_id) {
                    Some(window) => window.ime.typed(&text),
                    None => return,
                };
                for event in events {
                    self.input(event);
                }
            }
            Repeating::Key(key_code) => {
                let time = self.key_time(now);
                self.send_key(task_id, key_code, true, time);
            }
        }
    }

    fn keyboard_left(&mut self) {
        self.seat_state.repeat = None;
        let Some(task_id) = self.seat_state.keyboard_focus.take() else {
            return;
        };
        let time = self.key_time(Instant::now());
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        let mut events: Vec<dsa_host_core::input::InputEvent> = window
            .keys
            .release_all(time)
            .into_iter()
            .map(Into::into)
            .collect();
        events.extend(window.ime.leave().into_iter().map(Into::into));
        for event in events {
            self.input(event);
        }
    }

    /// Gives the input method the guest's focused editor in the window that has the keyboard:
    /// its surrounding text and its content type, or turns the input method off where no
    /// editor takes text, so keys reach a game as keys.
    pub(crate) fn describe_editor(&mut self, task_id: i32) {
        if self.seat_state.keyboard_focus != Some(task_id) {
            return;
        }
        let Some(text_input) = self.text_input.clone() else {
            return;
        };
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        let Some(kind) = window.ime.editor().map(|editor| editor.kind) else {
            return;
        };
        if !kind.takes_text() {
            if self.text_input_enabled {
                text_input.disable();
                text_input.commit();
                self.text_input_enabled = false;
            }
            return;
        }
        // Enabling resets the input method's state, so it is done once, not per change.
        let enabled_now = !self.text_input_enabled;
        if enabled_now {
            text_input.enable();
            self.text_input_enabled = true;
        }
        if let Some(surrounding) = window.ime.surrounding() {
            text_input.set_surrounding_text(
                surrounding.text,
                surrounding.cursor,
                surrounding.anchor,
            );
            text_input.set_text_change_cause(if window.ime.is_composing() {
                ChangeCause::InputMethod
            } else {
                ChangeCause::Other
            });
        }
        let (hint, purpose) = crate::input::text_input::content_type(kind);
        text_input.set_content_type(
            ContentHint::from_bits_truncate(hint),
            ContentPurpose::try_from(purpose).unwrap_or(ContentPurpose::Normal),
        );
        text_input.commit();
        if enabled_now {
            self.place_input_method(task_id);
        }
    }

    /// Puts the input method's windows beside the cursor of the guest's focused editor.
    pub(crate) fn place_input_method(&mut self, task_id: i32) {
        let (Some(text_input), Some(window)) = (&self.text_input, self.windows.get(&task_id))
        else {
            return;
        };
        let Some(cursor) = window.ime_cursor else {
            return;
        };
        let scale = window.model.scale.max(1) as i32;
        text_input.set_cursor_rectangle(
            cursor.left / scale,
            cursor.top / scale,
            ((cursor.right - cursor.left) / scale).max(1),
            ((cursor.bottom - cursor.top) / scale).max(1),
        );
        text_input.commit();
    }

    fn contact(&mut self, task_id: i32, input: WindowInput) {
        self.pointer_input(task_id, input);
    }

    fn pen_frame(&mut self) {
        let pen = &mut self.seat_state.pen;
        let Some(task_id) = pen.task else {
            return;
        };
        let phase = if pen.left {
            ContactPhase::HoverExit
        } else if pen.down && !pen.was_down {
            ContactPhase::Down
        } else if pen.down {
            ContactPhase::Move
        } else if pen.was_down {
            ContactPhase::Up
        } else {
            ContactPhase::Hover
        };
        pen.was_down = pen.down;
        let left = std::mem::take(&mut pen.left);
        if left {
            pen.task = None;
        }
        let position = pen.position;
        let pressure = pen.pressure;
        let tool = if pen.eraser {
            PointerTool::Eraser
        } else {
            PointerTool::Pen
        };
        let pen_buttons = pen.buttons;
        let tilt = pen.tilt;
        let Some(window) = self.windows.get(&task_id) else {
            return;
        };
        let position = window.model.point_to_pixels(position);
        self.contact(
            task_id,
            WindowInput::Contact {
                tool,
                pointer_id: 0,
                phase,
                position,
                pressure,
                buttons: pen_buttons,
                tilt,
            },
        );
    }
}

/// The size of the fallback arrow cursor, in pixels.
const ARROW_SIZE: (u32, u32) = (12, 19);

/// The fallback arrow: black, outlined in white, pointing up and left, as premultiplied
/// `ARGB8888` (blue, green, red, alpha in memory).
fn arrow_pixels() -> Vec<u8> {
    let (width, height) = (ARROW_SIZE.0 as i32, ARROW_SIZE.1 as i32);
    // The arrow's outline: each row's last column inside it.
    let inside = |x: i32, y: i32| -> bool {
        if !(0..height).contains(&y) || x < 0 {
            return false;
        }
        match y {
            0..=11 => x <= y,
            12 => x <= 7,
            _ => {
                let from = (y - 13) / 2 + 4;
                (from..=from + 2).contains(&x)
            }
        }
    };
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let pixel: [u8; 4] = if !inside(x, y) {
                [0, 0, 0, 0]
            } else if !inside(x - 1, y)
                || !inside(x + 1, y)
                || !inside(x, y - 1)
                || !inside(x, y + 1)
            {
                [0xff, 0xff, 0xff, 0xff]
            } else {
                [0, 0, 0, 0xff]
            };
            pixels.extend_from_slice(&pixel);
        }
    }
    pixels
}

/// The cursor for resizing by `edge` (`xdg_toplevel` numbering).
fn edge_shape(edge: u32) -> Shape {
    match edge {
        1 => Shape::NResize,
        2 => Shape::SResize,
        4 => Shape::WResize,
        8 => Shape::EResize,
        5 => Shape::NwResize,
        9 => Shape::NeResize,
        6 => Shape::SwResize,
        10 => Shape::SeResize,
        _ => Shape::Default,
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
            let pointer = seat.get_pointer(qh, ());
            if let Some(manager) = &state.cursor_shapes {
                state.cursor_device = Some(manager.get_pointer(&pointer, qh, ()));
            }
            state.pointer = Some(pointer);
        }
        if capabilities.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
        if capabilities.contains(wl_seat::Capability::Touch) && state.touch.is_none() {
            state.touch = Some(seat.get_touch(qh, ()));
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                state.seat_state.enter_serial = serial;
                state.seat_state.shape = None;
                if surface.data::<MenuSurface>().is_some() {
                    state.seat_state.pointer_focus = None;
                    state.seat_state.on_menu = true;
                    state.seat_state.menu_position = (surface_x, surface_y);
                    state.set_shape(Shape::Default);
                    state.menu_pointer((surface_x, surface_y));
                    return;
                }
                state.seat_state.pointer_focus = task_of(&surface);
                let offset = state
                    .seat_state
                    .pointer_focus
                    .map(|task_id| state.surface_offset(task_id, &surface))
                    .unwrap_or((0.0, 0.0));
                state.seat_state.offset = offset;
                state.seat_state.position = (surface_x + offset.0, surface_y + offset.1);
                if let Some(task_id) = state.seat_state.pointer_focus {
                    state.pointer_moved(task_id);
                }
            }
            wl_pointer::Event::Leave { .. } => {
                if std::mem::take(&mut state.seat_state.on_menu) {
                    state.menu_pointer_left();
                    return;
                }
                if let Some(task_id) = state.seat_state.pointer_focus.take() {
                    if let Some(window) = state.windows.get_mut(&task_id) {
                        let caption = window.hover(None);
                        let back = window.title_bar.hover_back(false);
                        if caption || back {
                            state.redraw(task_id);
                        }
                    }
                    state.pointer_input(task_id, WindowInput::MouseLeave);
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                if state.seat_state.on_menu {
                    state.seat_state.menu_position = (surface_x, surface_y);
                    state.menu_pointer((surface_x, surface_y));
                    return;
                }
                let offset = state.seat_state.offset;
                state.seat_state.position = (surface_x + offset.0, surface_y + offset.1);
                if let Some(task_id) = state.seat_state.pointer_focus {
                    state.pointer_moved(task_id);
                }
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(button_state),
                ..
            } => {
                let down = button_state == wl_pointer::ButtonState::Pressed;
                if state.seat_state.on_menu {
                    state.serial = serial;
                    let position = state.seat_state.menu_position;
                    state.menu_button(serial, position, down);
                    return;
                }
                if let Some(task_id) = state.seat_state.pointer_focus {
                    state.pointer_button(task_id, serial, button, down);
                }
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                value,
                ..
            } => state.seat_state.scroll.continuous(axis_index(axis), value),
            wl_pointer::Event::AxisDiscrete {
                axis: WEnum::Value(axis),
                discrete,
            } => state
                .seat_state
                .scroll
                .value120(axis_index(axis), discrete * 120),
            wl_pointer::Event::AxisValue120 {
                axis: WEnum::Value(axis),
                value120,
            } => state.seat_state.scroll.value120(axis_index(axis), value120),
            wl_pointer::Event::Frame => {
                let Some(notches) = state.seat_state.scroll.take() else {
                    return;
                };
                let Some(task_id) = state.seat_state.pointer_focus else {
                    return;
                };
                let Some(window) = state.windows.get(&task_id) else {
                    return;
                };
                let position = window.model.point_to_pixels(state.seat_state.position);
                state.pointer_input(task_id, WindowInput::Wheel { position, notches });
            }
            _ => {}
        }
    }
}

fn axis_index(axis: wl_pointer::Axis) -> u32 {
    match axis {
        wl_pointer::Axis::HorizontalScroll => 1,
        _ => 0,
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                if let Some(xkb) = state.xkb.as_mut() {
                    xkb.set_keymap(fd, size);
                }
            }
            wl_keyboard::Event::Enter {
                serial, surface, ..
            } => {
                state.serial = serial;
                state.seat_state.keyboard_focus = task_of(&surface);
                state.data_focus_changed();
            }
            wl_keyboard::Event::Leave { .. } => state.keyboard_left(),
            wl_keyboard::Event::Key {
                serial,
                time,
                key,
                state: WEnum::Value(key_state),
            } => {
                let down = key_state == wl_keyboard::KeyState::Pressed;
                state.key(serial, time, key, down);
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                let (caps, num) = match state.xkb.as_mut() {
                    Some(xkb) => {
                        xkb.modifiers(mods_depressed, mods_latched, mods_locked, group);
                        xkb.lock_masks()
                    }
                    None => (keymap::XKB_LOCK_MASK, keymap::XKB_MOD2_MASK),
                };
                state.seat_state.locks = keymap::locks(mods_locked, caps, num);
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.seat_state.repeat_rate = rate;
                state.seat_state.repeat_delay = delay;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let touch = |id: i32, phase: ContactPhase, position: (f64, f64)| WindowInput::Contact {
            tool: PointerTool::Touch,
            pointer_id: id as u32,
            phase,
            position,
            pressure: None,
            buttons: 0,
            tilt: (0.0, 0.0),
        };
        match event {
            wl_touch::Event::Down {
                serial,
                surface,
                id,
                x,
                y,
                ..
            } => {
                state.serial = serial;
                if surface.data::<MenuSurface>().is_some() {
                    state.menu_touch(serial, (x, y));
                    return;
                }
                let Some(task_id) = task_of(&surface) else {
                    return;
                };
                // Move or Size from the window menu starts with this touch, held.
                if state.start_armed(task_id, serial, (x, y)) {
                    return;
                }
                let Some(window) = state.windows.get(&task_id) else {
                    return;
                };
                let position = window.model.point_to_pixels((x, y));
                state.seat_state.touches.insert(id, (task_id, position));
                state.contact(task_id, touch(id, ContactPhase::Down, position));
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                let Some((task_id, _)) = state.seat_state.touches.get(&id).copied() else {
                    return;
                };
                let Some(window) = state.windows.get(&task_id) else {
                    return;
                };
                let position = window.model.point_to_pixels((x, y));
                state.seat_state.touches.insert(id, (task_id, position));
                state.contact(task_id, touch(id, ContactPhase::Move, position));
            }
            wl_touch::Event::Up { id, .. } => {
                if let Some((task_id, position)) = state.seat_state.touches.remove(&id) {
                    state.contact(task_id, touch(id, ContactPhase::Up, position));
                }
            }
            wl_touch::Event::Cancel => {
                let touches: Vec<_> = state.seat_state.touches.drain().collect();
                for (id, (task_id, position)) in touches {
                    state.contact(task_id, touch(id, ContactPhase::Cancel, position));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut Self,
        text_input: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                text_input.enable();
                state.text_input_enabled = true;
                text_input.set_content_type(ContentHint::empty(), ContentPurpose::Normal);
                text_input.commit();
                if let Some(task_id) = task_of(&surface) {
                    state.place_input_method(task_id);
                    // The editor the guest described last, once the window has the keyboard.
                    state.describe_editor(task_id);
                }
            }
            zwp_text_input_v3::Event::Leave { surface } => {
                text_input.disable();
                state.text_input_enabled = false;
                text_input.commit();
                if let Some(task_id) = task_of(&surface) {
                    let events = match state.windows.get_mut(&task_id) {
                        Some(window) => window.ime.leave(),
                        None => Vec::new(),
                    };
                    for event in events {
                        state.input(event);
                    }
                }
            }
            zwp_text_input_v3::Event::PreeditString {
                text, cursor_begin, ..
            } => {
                if let Some(window) = state
                    .seat_state
                    .keyboard_focus
                    .and_then(|task_id| state.windows.get_mut(&task_id))
                {
                    window.ime.preedit(text, cursor_begin);
                }
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                if let Some(window) = state
                    .seat_state
                    .keyboard_focus
                    .and_then(|task_id| state.windows.get_mut(&task_id))
                {
                    window.ime.commit(text);
                }
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                if let Some(window) = state
                    .seat_state
                    .keyboard_focus
                    .and_then(|task_id| state.windows.get_mut(&task_id))
                {
                    window.ime.delete_surrounding(before_length, after_length);
                }
            }
            zwp_text_input_v3::Event::Done { .. } => {
                let events = match state
                    .seat_state
                    .keyboard_focus
                    .and_then(|task_id| state.windows.get_mut(&task_id))
                {
                    Some(window) => window.ime.done(),
                    None => Vec::new(),
                };
                for event in events {
                    state.input(event);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletSeatV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpTabletSeatV2,
        _: zwp_tablet_seat_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpTabletPadV2,
        _: zwp_tablet_pad_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadGroupV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpTabletPadGroupV2,
        _: zwp_tablet_pad_group_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);
}

wayland_client::delegate_noop!(State: ignore ZwpTabletV2);
wayland_client::delegate_noop!(State: ignore ZwpTabletPadRingV2);
wayland_client::delegate_noop!(State: ignore ZwpTabletPadStripV2);

impl Dispatch<ZwpTabletToolV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let pen = &mut state.seat_state.pen;
        match event {
            zwp_tablet_tool_v2::Event::Type {
                tool_type: WEnum::Value(kind),
            } => pen.eraser = kind == zwp_tablet_tool_v2::Type::Eraser,
            zwp_tablet_tool_v2::Event::ProximityIn { surface, .. } => {
                pen.task = task_of(&surface);
                pen.left = false;
            }
            zwp_tablet_tool_v2::Event::ProximityOut => pen.left = true,
            zwp_tablet_tool_v2::Event::Down { serial } => {
                pen.down = true;
                state.serial = serial;
            }
            zwp_tablet_tool_v2::Event::Up => pen.down = false,
            zwp_tablet_tool_v2::Event::Motion { x, y } => pen.position = (x, y),
            zwp_tablet_tool_v2::Event::Pressure { pressure } => {
                pen.pressure = Some(pressure as f32 / 65535.0)
            }
            zwp_tablet_tool_v2::Event::Tilt { tilt_x, tilt_y } => {
                pen.tilt = (tilt_x as f32, tilt_y as f32)
            }
            zwp_tablet_tool_v2::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } => {
                let bit = match button {
                    evdev_button::STYLUS => buttons::STYLUS_PRIMARY,
                    evdev_button::STYLUS2 => buttons::STYLUS_SECONDARY,
                    _ => 0,
                };
                if button_state == zwp_tablet_tool_v2::ButtonState::Pressed {
                    pen.buttons |= bit;
                } else {
                    pen.buttons &= !bit;
                }
            }
            zwp_tablet_tool_v2::Event::Frame { .. } => state.pen_frame(),
            _ => {}
        }
    }
}

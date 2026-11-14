//! The widget windows of a host that draws them itself: what each window shows, where it is,
//! and what the user's presses, moves, keys and menu choices do to it, with no window system
//! in it.
//!
//! The host makes one borderless window per widget on its desktop layer and passes on what
//! happens in it: the pointer in the window's own pixels ([`Desk::down`], [`Desk::moved`],
//! [`Desk::up`]), the keyboard ([`Desk::key`], [`Desk::focus`]), the time ([`Desk::tick`]),
//! and the end of the widget's menu ([`Desk::menu_closed`]). The desk answers with
//! [`Effect`]s, which the host carries out: open, move, resize, draw or close a window, show
//! the widget's menu as the desktop's own, take the keyboard. What goes to the service (where
//! a window is, the pointer in the widget, Remove, Widget settings) comes out as the
//! [`WindowReport`]s the watch sends ([`super::service::sync`]).
//!
//! Positions are in the host's pixels: a window's top left corner on the screen
//! ([`DeskWindow::origin`]) and its size, at its monitor's `dpi`, 96 times the monitor's
//! scale. The host converts from its own units (a Wayland output's logical pixels times its
//! scale, a Mac's points times the backing scale, from the top of the screen).
//!
//! A window that is resized by its edges is resized here, not by the desktop: a layer
//! surface, an X11 desktop window and a borderless Mac panel have no edges of the desktop's
//! own to drag. While it is resized its size goes to the service when the drag pauses and when
//! it ends ([`ResizeReports`]), and the widget's last image shows unscaled meanwhile; the host
//! keeps calling [`Desk::tick`] while [`Desk::busy`] says so.

use std::collections::BTreeMap;
use std::time::Instant;

use dsa_protocol::host_api::types::PointerAction;

use super::geometry::{
    all_clear, edit_layout, edit_margin, frame_overdue, hit_test, keep_clickable, max_window_size,
    min_window_size, placeholder, placement_of, press_threshold, px_of_dip, reachable_origin,
    to_frame, unscaled, window_size, Hit, ScreenRect, HOLD_TO_MOVE,
};
use super::gesture::{Gesture, Press, ResizePoll, ResizeReports, Step};
use super::menu::{self, choice_of, size_choices, Entry, MenuChoice, MenuText};
use super::paint::{draw_lifted, Image, Look};
use super::service::{placed_report, WindowCommand, WindowReport};
use crate::widgets::{Placement, Widget, WidgetFrame};

/// How far the pointer moves before a press is a drag, in device-independent pixels: the
/// drag threshold of Windows and GTK at a scale of 100%.
pub const DRAG_DIP: u32 = 4;

/// Turns a widget's PNG frame into its pixels.
pub type Decode = Box<dyn Fn(&[u8]) -> Result<Image, String> + Send>;

/// A mouse button, or a finger or a pen, which press like the left one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// The left button, a finger or a pen.
    Primary,
    /// The right button, or a two-finger click.
    Secondary,
}

/// A key the widget windows answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Ends an edit.
    Escape,
    /// Enter, Space or the menu key: opens the widget's menu.
    Menu,
}

/// Where the widget's menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Under this rectangle on the screen (left, top, right, bottom): the widget, after a long
    /// press or from the keyboard.
    Below(ScreenRect),
    /// At this point on the screen, where the widget was right-clicked.
    At(i32, i32),
}

/// What the host does for the desk.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Open the widget's window at `origin`, `size` pixels, on the desktop layer.
    Open {
        widget_id: i32,
        origin: (i32, i32),
        size: (i32, i32),
    },
    /// Move or resize the widget's window.
    Place {
        widget_id: i32,
        origin: (i32, i32),
        size: (i32, i32),
    },
    /// Show the window's picture again ([`Desk::picture`]).
    Redraw { widget_id: i32 },
    /// Close the widget's window.
    Close { widget_id: i32 },
    /// Show the widget's menu where `anchor` says, as a menu of the desktop's own, and tell
    /// [`Desk::menu_closed`] what the user chose. `name` is what assistive technology calls it.
    Menu {
        widget_id: i32,
        anchor: Anchor,
        entries: Vec<Entry>,
        name: String,
    },
    /// The window takes the keyboard while its widget is edited, so Esc ends the edit and a
    /// click on another window does too.
    Focus { widget_id: i32 },
    /// A line for the window manager's log.
    Log(String),
}

/// One widget's window, as the desk keeps it.
#[derive(Debug, Clone)]
pub struct DeskWindow {
    pub widget: Widget,
    /// The window's top left corner on the screen, in the host's pixels.
    pub origin: (i32, i32),
    /// The window's size in pixels, with the room around a widget being edited.
    pub size: (i32, i32),
    /// Its monitor's density: 96 at a scale of 100%.
    pub dpi: u32,
    /// The latest frame, and the last one that had something in it.
    frame: Option<Image>,
    kept: Option<Image>,
    gesture: Gesture,
    /// When the widget's size is passed on while it is resized, and whether the resize ended
    /// with its last size perhaps still to pass on.
    resizes: ResizeReports<ScreenRect>,
    resize_ended: bool,
    /// The room around the widget while it is edited.
    edit: Option<i32>,
    focused: bool,
    menu_open: bool,
    /// Whether Android has a press of this window.
    pressed: bool,
    /// Where the pointer was last, on the screen, and whether a finger or a pen put it there.
    pointer: (i32, i32),
    touch: bool,
    waiting_since: Option<Instant>,
    noted: bool,
}

impl DeskWindow {
    /// Whether the widget is being edited.
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The widget's own rectangle in the window: all of it, less the room around a widget
    /// being edited. Left, top, width and height.
    pub fn widget_rect(&self) -> (i32, i32, i32, i32) {
        let margin = self.edit.unwrap_or(0);
        (
            margin,
            margin,
            (self.size.0 - 2 * margin).max(1),
            (self.size.1 - 2 * margin).max(1),
        )
    }

    fn margin(&self) -> i32 {
        self.edit.unwrap_or(0)
    }

    fn screen(&self, local: (i32, i32)) -> (i32, i32) {
        (self.origin.0 + local.0, self.origin.1 + local.1)
    }

    fn hit(&self, local: (i32, i32)) -> Hit {
        let resize_mode = self.widget.provider.resize_mode;
        let (w, h) = self.size;
        hit_test(
            self.editing(),
            local.0,
            local.1,
            w,
            h,
            self.dpi,
            resize_mode,
        )
    }

    /// The window's rectangle on the screen.
    fn rect(&self) -> ScreenRect {
        let (x, y) = self.origin;
        (x, y, x + self.size.0, y + self.size.1)
    }

    /// The widget's own rectangle on the screen, without the room of an edit.
    fn widget_on_screen(&self) -> ScreenRect {
        let (x, y, w, h) = self.widget_rect();
        let (left, top) = self.screen((x, y));
        (left, top, left + w, top + h)
    }

    /// The smallest and largest the window may be made now, width and height in pixels: what
    /// the widget allows, with the room around it while it is edited.
    fn size_limits(&self) -> ((i32, i32), (i32, i32)) {
        let provider = &self.widget.provider;
        let room = 2 * self.margin();
        let (min_w, min_h) = min_window_size(provider, self.dpi);
        let (max_w, max_h) = max_window_size(provider, self.dpi);
        (
            (min_w + room, min_h + room),
            (max_w.max(min_w) + room, max_h.max(min_h) + room),
        )
    }

    fn threshold(&self) -> (i32, i32) {
        let drag = px_of_dip(DRAG_DIP, self.dpi).max(1);
        press_threshold((drag, drag), self.touch, self.dpi)
    }

    /// Where the window's widget is now, as the service keeps it.
    fn placement(&self) -> Placement {
        let (x, y, w, h) = self.widget_rect();
        placement_of(self.origin.0 + x, self.origin.1 + y, w, h, self.dpi)
    }

    fn report(&self) -> WindowReport {
        placed_report(self.widget.widget_id, self.widget_on_screen(), self.dpi)
    }

    /// The lifted widget's rectangle on the screen while it is edited, else the widget's.
    fn shown_on_screen(&self) -> ScreenRect {
        let (x, y, w, h) = if self.editing() {
            edit_layout(
                self.size.0,
                self.size.1,
                self.dpi,
                self.widget.provider.resize_mode,
            )
            .lifted
        } else {
            self.widget_rect()
        };
        let (left, top) = self.screen((x, y));
        (left, top, left + w, top + h)
    }

    /// The image the widget shows: its latest frame, or the last that had something in it
    /// when Android sent one with nothing.
    fn image(&self) -> Option<&Image> {
        match &self.frame {
            Some(frame) if !all_clear(&frame.pixels) => Some(frame),
            _ => self.kept.as_ref(),
        }
    }
}

/// The widget windows of one host.
pub struct Desk {
    windows: BTreeMap<i32, DeskWindow>,
    effects: Vec<Effect>,
    reports: Vec<WindowReport>,
    /// The colors the windows are drawn in.
    pub look: Look,
    /// The density a new window opens at, until its host says which monitor it is on.
    pub dpi: u32,
    /// The monitors' work areas on the screen, which a window is kept on.
    areas: Vec<ScreenRect>,
    text: MenuText,
    decode: Decode,
}

impl Desk {
    /// A desk with no windows, drawn in `look`, opening windows at `dpi`, with its menu in
    /// Korean when `korean`, and its frames decoded by `decode`.
    pub fn new(look: Look, dpi: u32, korean: bool, decode: Decode) -> Desk {
        Desk {
            windows: BTreeMap::new(),
            effects: Vec::new(),
            reports: Vec::new(),
            look,
            dpi: dpi.max(1),
            areas: Vec::new(),
            text: menu::menu_text(korean),
            decode,
        }
    }

    /// What the host has to do, in order.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// What goes to the service, in order.
    pub fn take_reports(&mut self) -> Vec<WindowReport> {
        std::mem::take(&mut self.reports)
    }

    /// The window of widget `widget_id`.
    pub fn window(&self, widget_id: i32) -> Option<&DeskWindow> {
        self.windows.get(&widget_id)
    }

    /// The widgets that have windows.
    pub fn ids(&self) -> Vec<i32> {
        self.windows.keys().copied().collect()
    }

    /// Whether the desk needs [`Desk::tick`] soon: a press may become a long press, or a
    /// resize has a size to pass on.
    pub fn busy(&self, _now: Instant) -> bool {
        self.windows.values().any(|w| {
            matches!(
                w.gesture,
                Gesture::Widget { slid: false, .. } | Gesture::Resizing { .. }
            ) || w.resize_ended
        })
    }

    /// The monitors' work areas are `areas` now, on the screen in the host's pixels.
    pub fn set_areas(&mut self, areas: Vec<ScreenRect>) {
        self.areas = areas;
    }

    /// Carries out what the watch asks.
    pub fn command(&mut self, command: WindowCommand, now: Instant) {
        match command {
            WindowCommand::Place(widget) => self.place(widget, now),
            WindowCommand::Frame(frame) => self.frame(frame),
            WindowCommand::Remove(widget_id) => {
                if self.windows.remove(&widget_id).is_some() {
                    self.effects.push(Effect::Close { widget_id });
                }
            }
        }
    }

    fn place(&mut self, widget: Widget, now: Instant) {
        let widget_id = widget.widget_id;
        let opened = !self.windows.contains_key(&widget_id);
        if !opened {
            // Placed anew (described again, as when Android restarts): an edit ends, and the
            // window goes where the placement says.
            self.leave_edit(widget_id);
        }
        let dpi = self.windows.get(&widget_id).map_or(self.dpi, |w| w.dpi);
        let size = window_size(&widget.placement, dpi);
        let origin = reachable_origin(
            widget.placement.x,
            widget.placement.y,
            size.0,
            size.1,
            &self.areas,
        );
        match self.windows.get_mut(&widget_id) {
            Some(window) => {
                window.widget = widget;
                window.origin = origin;
                window.size = size;
                self.effects.push(Effect::Place {
                    widget_id,
                    origin,
                    size,
                });
            }
            None => {
                self.windows.insert(
                    widget_id,
                    DeskWindow {
                        widget,
                        origin,
                        size,
                        dpi,
                        frame: None,
                        kept: None,
                        gesture: Gesture::Idle,
                        resizes: ResizeReports::new(),
                        resize_ended: false,
                        edit: None,
                        focused: false,
                        menu_open: false,
                        pressed: false,
                        pointer: origin,
                        touch: false,
                        waiting_since: Some(now),
                        noted: false,
                    },
                );
                self.effects.push(Effect::Open {
                    widget_id,
                    origin,
                    size,
                });
            }
        }
        self.effects.push(Effect::Redraw { widget_id });
        self.report(widget_id);
    }

    fn frame(&mut self, frame: WidgetFrame) {
        let widget_id = frame.widget_id;
        if !self.windows.contains_key(&widget_id) {
            return;
        }
        match (self.decode)(&frame.png) {
            Ok(image) => {
                let Some(window) = self.windows.get_mut(&widget_id) else {
                    return;
                };
                if !all_clear(&image.pixels) {
                    window.kept = Some(image.clone());
                }
                window.frame = Some(image);
                window.waiting_since = None;
                self.effects.push(Effect::Redraw { widget_id });
            }
            Err(why) => self.effects.push(Effect::Log(format!(
                "a picture of widget {widget_id} could not be read: {why}"
            ))),
        }
    }

    fn report(&mut self, widget_id: i32) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let report = window.report();
        window.widget.placement = window.placement();
        self.reports.push(report);
    }

    fn place_effect(&mut self, widget_id: i32) {
        if let Some(window) = self.windows.get(&widget_id) {
            self.effects.push(Effect::Place {
                widget_id,
                origin: window.origin,
                size: window.size,
            });
            self.effects.push(Effect::Redraw { widget_id });
        }
    }

    /// Lifts the widget for editing: its window grows by the room for the shadow and the
    /// handles on each side, keeping the widget where it was, and takes the keyboard. One
    /// widget is edited at a time.
    fn enter_edit(&mut self, widget_id: i32) {
        self.end_other_edits(widget_id);
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        if window.editing() {
            return;
        }
        let margin = edit_margin(window.dpi);
        window.edit = Some(margin);
        window.gesture.regrab((margin, margin));
        window.origin = (window.origin.0 - margin, window.origin.1 - margin);
        window.size = (window.size.0 + 2 * margin, window.size.1 + 2 * margin);
        self.effects.push(Effect::Log(format!(
            "widget {widget_id}: editing it after a long press"
        )));
        self.place_effect(widget_id);
        self.effects.push(Effect::Focus { widget_id });
    }

    /// Ends the widget's edit, if it is being edited: its window shrinks back around the
    /// widget, which stays where the edit left it.
    fn leave_edit(&mut self, widget_id: i32) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let Some(margin) = window.edit.take() else {
            return;
        };
        window.gesture.regrab((-margin, -margin));
        window.origin = (window.origin.0 + margin, window.origin.1 + margin);
        window.size = (
            (window.size.0 - 2 * margin).max(1),
            (window.size.1 - 2 * margin).max(1),
        );
        self.place_effect(widget_id);
    }

    fn end_other_edits(&mut self, widget_id: i32) {
        let others: Vec<i32> = self
            .windows
            .iter()
            .filter(|(id, w)| **id != widget_id && w.editing())
            .map(|(id, _)| *id)
            .collect();
        for other in others {
            self.leave_edit(other);
        }
    }

    /// Opens the widget's menu where `anchor` says, with the size it has now marked.
    fn open_menu(&mut self, widget_id: i32, anchor: Anchor) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        window.menu_open = true;
        let placement = window.placement();
        let entries = menu::entries(&self.text, &window.widget.provider, &placement);
        let name = menu::menu_name(&self.text, &window.widget.provider);
        self.effects.push(Effect::Menu {
            widget_id,
            anchor,
            entries,
            name,
        });
    }

    /// Opens the widget's menu under it, lifted or not, as the keyboard does.
    fn open_menu_below(&mut self, widget_id: i32) {
        let Some(window) = self.windows.get(&widget_id) else {
            return;
        };
        let anchor = Anchor::Below(window.shown_on_screen());
        self.open_menu(widget_id, anchor);
    }

    /// Reports pointer input inside the widget, at `local` in its window, in its frame's
    /// pixels. Moves count only while Android has the press.
    fn pointer(&mut self, widget_id: i32, action: PointerAction, local: (i32, i32)) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        if action == PointerAction::Move && !window.pressed {
            return;
        }
        match action {
            PointerAction::Down => window.pressed = true,
            PointerAction::Up | PointerAction::Cancel => window.pressed = false,
            _ => {}
        }
        let (x, y, w, h) = window.widget_rect();
        let frame = window
            .image()
            .map(|f| (f.width, f.height))
            .unwrap_or((w.max(1) as u32, h.max(1) as u32));
        let (fx, fy) = to_frame(local.0 - x, local.1 - y, frame);
        self.reports.push(WindowReport::Pointer {
            widget_id,
            action,
            x: fx,
            y: fy,
        });
    }

    /// Does what a step of the window's gesture asks, at `now`.
    fn step(&mut self, widget_id: i32, step: Step, local: (i32, i32), now: Instant) {
        match step {
            // A hold checked early is checked again on the next tick; one that slid is the
            // widget's own drag.
            Step::Nothing | Step::HoldOn(_) | Step::HoldRefused(_) => {}
            Step::Android(action) => self.pointer(widget_id, action, local),
            Step::Lift => {
                // The long press is not the widget's: Android's press is cancelled, so it
                // never becomes a click, before the widget lifts.
                self.pointer(widget_id, PointerAction::Cancel, local);
                self.enter_edit(widget_id);
            }
            Step::EditMenu => {
                if let Some(window) = self.windows.get(&widget_id) {
                    let anchor = Anchor::Below(window.shown_on_screen());
                    self.open_menu(widget_id, anchor);
                }
            }
            Step::StartMove { to } | Step::MoveTo(to) => {
                if let Some(window) = self.windows.get_mut(&widget_id) {
                    window.origin = to;
                }
                self.place_effect(widget_id);
            }
            Step::EndMove => {
                self.report(widget_id);
                // A finished move ends the edit, as on a phone's home screen.
                self.leave_edit(widget_id);
            }
            Step::StartResize(_) => {
                if let Some(window) = self.windows.get_mut(&widget_id) {
                    window.resizes.restart();
                    window.resize_ended = false;
                }
            }
            Step::ResizeTo((left, top, right, bottom)) => {
                let Some(window) = self.windows.get_mut(&widget_id) else {
                    return;
                };
                let (origin, size) = ((left, top), ((right - left).max(1), (bottom - top).max(1)));
                if (origin, size) == (window.origin, window.size) {
                    return;
                }
                window.origin = origin;
                window.size = size;
                let widget = window.widget_on_screen();
                window.resizes.moved(widget, now);
                self.place_effect(widget_id);
            }
            Step::EndResize => {
                // The last size is passed on now, or as soon as the last one passed on is far
                // enough behind, and the service keeps it.
                if let Some(window) = self.windows.get_mut(&widget_id) {
                    window.resize_ended = true;
                    let (_, _, w, h) = window.widget_rect();
                    self.effects.push(Effect::Log(format!(
                        "widget {widget_id}: resized to {w}x{h} pixels"
                    )));
                }
                self.poll_resize(widget_id, now);
                // A finished resize ends the edit, as on a phone's home screen.
                self.leave_edit(widget_id);
            }
        }
    }

    /// Passes the widget's size on if its resize has one due ([`ResizeReports::poll`]).
    fn poll_resize(&mut self, widget_id: i32, now: Instant) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let ended = window.resize_ended;
        let poll = window.resizes.poll(now, ended);
        if ended && poll != ResizePoll::Wait {
            window.resize_ended = false;
        }
        if let ResizePoll::Send(_) = poll {
            self.report(widget_id);
        }
    }

    /// A button went down at `local` in the widget's window; `touch` when a finger or a pen
    /// pressed.
    pub fn down(
        &mut self,
        widget_id: i32,
        local: (i32, i32),
        button: Button,
        touch: bool,
        now: Instant,
    ) {
        if !self.windows.contains_key(&widget_id) {
            return;
        }
        // A press on any widget ends the edit of every other one.
        self.end_other_edits(widget_id);
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let at = window.screen(local);
        window.pointer = at;
        window.touch = touch;
        if button == Button::Secondary {
            self.open_menu(widget_id, Anchor::At(at.0, at.1));
            return;
        }
        let editing = window.editing();
        let hit = window.hit(local);
        if hit == Hit::Outside {
            self.leave_edit(widget_id);
            return;
        }
        let (min, max) = window.size_limits();
        let press = Press {
            hit,
            editing,
            window: window.rect(),
            min,
            max,
        };
        let step = window.gesture.press(press, at, now);
        self.step(widget_id, step, local, now);
    }

    /// The pointer moved to `local` in the widget's window.
    pub fn moved(&mut self, widget_id: i32, local: (i32, i32)) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let at = window.screen(local);
        window.pointer = at;
        let (origin, threshold) = (window.origin, window.threshold());
        let step = window.gesture.moved(at, origin, threshold);
        self.step(widget_id, step, local, Instant::now());
    }

    /// The button went up at `local` in the widget's window.
    pub fn up(&mut self, widget_id: i32, local: (i32, i32), button: Button) {
        if button != Button::Primary {
            return;
        }
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let step = window.gesture.up();
        self.step(widget_id, step, local, Instant::now());
    }

    /// The window lost the pointer before the button went up, as when the desktop took it.
    pub fn cancel(&mut self, widget_id: i32) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let local = (
            window.pointer.0 - window.origin.0,
            window.pointer.1 - window.origin.1,
        );
        let step = window.gesture.cancel();
        self.step(widget_id, step, local, Instant::now());
    }

    /// The pointer came into the window, at `local`. Nothing is drawn over the widget for it,
    /// so its own buttons stay free to click in every corner; the host shows the cursor of
    /// [`Desk::hit`].
    pub fn entered(&mut self, widget_id: i32, local: (i32, i32)) {
        if let Some(window) = self.windows.get_mut(&widget_id) {
            window.pointer = window.screen(local);
        }
    }

    /// The pointer left the window.
    pub fn left(&mut self, _widget_id: i32) {}

    /// The window took the keyboard, or lost it. Losing it to another window ends an edit;
    /// its own menu taking it ends nothing.
    pub fn focus(&mut self, widget_id: i32, focused: bool) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        window.focused = focused;
        let menu_open = window.menu_open;
        if !focused && !menu_open {
            self.leave_edit(widget_id);
        }
    }

    /// A key went down in the window.
    pub fn key(&mut self, widget_id: i32, key: Key) {
        match key {
            Key::Escape => self.leave_edit(widget_id),
            Key::Menu => self.open_menu_below(widget_id),
        }
    }

    /// The widget's menu closed with `command` chosen (an entry's command, None when it was
    /// dismissed). `pressed_beside` says it was dismissed by a press outside the widget, on
    /// the desktop or another window, which ends an edit; dismissed with Esc or by a press on
    /// the widget, the edit goes on.
    pub fn menu_closed(&mut self, widget_id: i32, command: Option<u32>, pressed_beside: bool) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        window.menu_open = false;
        let editing = window.editing();
        let choice = command.and_then(|c| choice_of(c, &size_choices(&window.widget.provider)));
        match choice {
            Some(MenuChoice::Resize {
                width_dp,
                height_dp,
            }) => {
                // The edit ends first, so the window is the widget's own size again.
                self.leave_edit(widget_id);
                if let Some(window) = self.windows.get_mut(&widget_id) {
                    let placement = Placement {
                        width_dp,
                        height_dp,
                        ..window.widget.placement
                    };
                    window.size = window_size(&placement, window.dpi);
                }
                self.place_effect(widget_id);
                self.report(widget_id);
            }
            Some(MenuChoice::Remove) => {
                self.leave_edit(widget_id);
                self.reports.push(WindowReport::Remove { widget_id });
            }
            Some(MenuChoice::Settings) => {
                // The setup opens as the app's window, which takes the keyboard.
                self.leave_edit(widget_id);
                self.reports.push(WindowReport::Reconfigure { widget_id });
            }
            None if editing && pressed_beside => self.leave_edit(widget_id),
            None => {}
        }
    }

    /// The window is on a monitor of `dpi` now: an edit ends, and the widget keeps its size
    /// in dp, so it looks the same size there.
    pub fn scale_changed(&mut self, widget_id: i32, dpi: u32) {
        let dpi = dpi.max(1);
        if self.windows.get(&widget_id).is_none_or(|w| w.dpi == dpi) {
            return;
        }
        self.leave_edit(widget_id);
        if let Some(window) = self.windows.get_mut(&widget_id) {
            let placement = window.placement();
            window.dpi = dpi;
            window.size = window_size(&placement, dpi);
        }
        self.place_effect(widget_id);
        self.report(widget_id);
    }

    /// The desktop put the window at `origin` itself, as an X11 window manager may.
    pub fn moved_by_desktop(&mut self, widget_id: i32, origin: (i32, i32)) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        if window.origin == origin || window.gesture.dragging() {
            return;
        }
        window.origin = origin;
        self.report(widget_id);
    }

    /// A press on the widget firm enough to be a long press at once, as a trackpad's force
    /// click: it lifts the widget for editing as holding it would, unless it already slid.
    pub fn force_press(&mut self, widget_id: i32) {
        let Some(window) = self.windows.get_mut(&widget_id) else {
            return;
        };
        let Gesture::Widget { since, .. } = window.gesture else {
            return;
        };
        let (at, origin) = (window.pointer, window.origin);
        let step = window.gesture.held(at, origin, since + HOLD_TO_MOVE);
        let local = (at.0 - origin.0, at.1 - origin.1);
        self.step(widget_id, step, local, Instant::now());
    }

    /// What is due at `now`: a press held still long enough lifts its widget, a resize that
    /// paused or ended passes its size on, and a window still waiting for its first picture is
    /// logged once.
    pub fn tick(&mut self, now: Instant) {
        let ids = self.ids();
        for widget_id in ids {
            let Some(window) = self.windows.get_mut(&widget_id) else {
                continue;
            };
            let (at, origin) = (window.pointer, window.origin);
            let step = window.gesture.held(at, origin, now);
            let local = (at.0 - origin.0, at.1 - origin.1);
            if frame_overdue(window.waiting_since, window.noted, now) {
                window.noted = true;
                self.effects.push(Effect::Log(format!(
                    "widget {widget_id} has shown no picture from Android yet; its window shows its placeholder"
                )));
            }
            self.step(widget_id, step, local, now);
            self.poll_resize(widget_id, now);
        }
    }

    /// What the pointer at `local` in the window would do, for its cursor: an edge resizes,
    /// the rest is the widget's.
    pub fn hit(&self, widget_id: i32, local: (i32, i32)) -> Option<Hit> {
        let window = self.windows.get(&widget_id)?;
        if let Gesture::Resizing { edge, .. } = window.gesture {
            return Some(edge);
        }
        Some(window.hit(local))
    }

    /// The window's picture at its size now: the widget's image at its own size, never
    /// scaled, from the window's top left corner (while a resize has not reached Android yet
    /// it is the last one, and the room it does not fill stays clear), or its placeholder
    /// until Android has drawn it; raised over its shadow with its border and handles while it
    /// is edited. Every pixel takes the pointer, so the whole window can be pressed.
    pub fn picture(&self, widget_id: i32, _now: Instant) -> Option<Image> {
        let window = self.windows.get(&widget_id)?;
        let (width, height) = (window.size.0.max(1) as usize, window.size.1.max(1) as usize);
        let (_, _, ww, wh) = window.widget_rect();
        let card;
        let image = match window.image() {
            Some(image) => image,
            None => {
                let (w, h) = (ww.max(1) as u32, wh.max(1) as u32);
                card = Image {
                    width: w,
                    height: h,
                    pixels: placeholder(w as usize, h as usize, window.dpi),
                };
                &card
            }
        };
        let mut pixels = if window.editing() {
            let resize_mode = window.widget.provider.resize_mode;
            draw_lifted(image, width, height, window.dpi, resize_mode, &self.look)
        } else {
            let from = (image.width as usize, image.height as usize);
            unscaled(&image.pixels, from, (width, height))
        };
        keep_clickable(&mut pixels);
        Some(Image {
            width: width as u32,
            height: height as u32,
            pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::WidgetProvider;
    use std::time::Duration;

    use super::super::geometry::{android_density, dp_of_px};

    fn clock(reconfigurable: bool) -> Widget {
        Widget {
            widget_id: 7,
            provider: WidgetProvider {
                provider: "com.example.clock/.ClockWidget".into(),
                label: "Clock".into(),
                min_width_dp: 110,
                min_height_dp: 40,
                default_width_dp: 180,
                default_height_dp: 110,
                resize_mode: 3,
                reconfigurable,
                ..WidgetProvider::default()
            },
            placement: Placement {
                x: 100,
                y: 200,
                width_dp: 180,
                height_dp: 110,
                resized: false,
            },
        }
    }

    /// The size of the clock's window at `dpi`, in pixels.
    fn clock_size(dpi: u32) -> (i32, i32) {
        window_size(&clock(false).placement, dpi)
    }

    fn desk(dpi: u32) -> Desk {
        Desk::new(
            Look::new(false, 0xFF35_84E4),
            dpi,
            false,
            Box::new(|png: &[u8]| {
                crate::png::decode(png)
                    .map(|image| Image::from_rgba(image.width, image.height, &image.pixels))
            }),
        )
    }

    fn placed(dpi: u32, reconfigurable: bool) -> (Desk, Instant) {
        let mut desk = desk(dpi);
        let now = Instant::now();
        desk.command(WindowCommand::Place(clock(reconfigurable)), now);
        (desk, now)
    }

    fn pointers(reports: &[WindowReport]) -> Vec<PointerAction> {
        reports
            .iter()
            .filter_map(|r| match r {
                WindowReport::Pointer { action, .. } => Some(*action),
                _ => None,
            })
            .collect()
    }

    fn menu_of(effects: &[Effect]) -> Option<(Anchor, Vec<Entry>)> {
        effects.iter().find_map(|e| match e {
            Effect::Menu {
                anchor, entries, ..
            } => Some((*anchor, entries.clone())),
            _ => None,
        })
    }

    fn frame_of(width: u32, height: u32, rgba: [u8; 4]) -> WidgetFrame {
        WidgetFrame {
            widget_id: 7,
            width,
            height,
            png: crate::png::encode(
                &crate::png::Rgba {
                    width,
                    height,
                    pixels: rgba.repeat((width * height) as usize),
                },
                true,
            ),
        }
    }

    #[test]
    fn fr12_a_widget_window_opens_at_its_placement_sized_in_dp_at_its_monitors_scale() {
        // At a scale of 150% (144 dpi) the window is the widget's size in dp at Android's
        // density there, the same physical size as on any other monitor.
        let (mut desk, _) = placed(144, false);
        let size = clock_size(144);
        assert_eq!(
            size,
            (
                crate::widgets::px_of(180, android_density(144)) as i32,
                crate::widgets::px_of(110, android_density(144)) as i32
            )
        );
        let effects = desk.take_effects();
        assert_eq!(
            effects[0],
            Effect::Open {
                widget_id: 7,
                origin: (100, 200),
                size,
            }
        );
        let reports = desk.take_reports();
        assert_eq!(
            reports,
            vec![WindowReport::Placed {
                widget_id: 7,
                placement: Placement {
                    x: 100,
                    y: 200,
                    width_dp: 180,
                    height_dp: 110,
                    resized: false,
                },
                width: size.0 as u32,
                height: size.1 as u32,
                density_dpi: android_density(144),
            }]
        );
        // A window off every monitor comes back into view.
        let mut desk = desk_with_area();
        desk.command(
            WindowCommand::Place(Widget {
                placement: Placement {
                    x: 5000,
                    y: 5000,
                    ..clock(false).placement
                },
                ..clock(false)
            }),
            Instant::now(),
        );
        let window = desk.window(7).unwrap();
        let (w, h) = clock_size(96);
        assert_eq!(window.origin, (1920 - w, 1080 - h));
    }

    fn desk_with_area() -> Desk {
        let mut desk = desk(96);
        desk.set_areas(vec![(0, 0, 1920, 1080)]);
        desk
    }

    #[test]
    fn fr12_a_tap_and_a_drag_on_the_widget_stay_the_widgets() {
        let (mut desk, now) = placed(160, false);
        desk.take_reports();
        desk.down(7, (40, 40), Button::Primary, false, now);
        desk.moved(7, (41, 40));
        desk.up(7, (41, 40), Button::Primary);
        assert_eq!(
            pointers(&desk.take_reports()),
            vec![PointerAction::Down, PointerAction::Move, PointerAction::Up]
        );
        // A drag past the threshold before the hold is the widget's own (a list scrolls): it
        // never lifts the widget, however long it then stays.
        desk.down(7, (40, 40), Button::Primary, false, now);
        desk.moved(7, (40, 80));
        desk.tick(now + Duration::from_secs(2));
        assert!(!desk.window(7).unwrap().editing());
        desk.up(7, (40, 80), Button::Primary);
        assert_eq!(
            pointers(&desk.take_reports()),
            vec![PointerAction::Down, PointerAction::Move, PointerAction::Up]
        );
    }

    #[test]
    fn fr12_a_point_in_the_window_is_the_same_pixel_of_the_widgets_unscaled_frame() {
        // A frame drawn at another size than the window (one from before a resize) is shown
        // from the window's top left corner at its own size, so a press is the same pixel of
        // it, kept inside it.
        let (mut desk, now) = placed(160, false);
        desk.command(WindowCommand::Frame(frame_of(60, 36, [255; 4])), now);
        desk.take_reports();
        desk.down(7, (20, 13), Button::Primary, false, now);
        desk.up(7, (20, 13), Button::Primary);
        desk.down(7, (100, 80), Button::Primary, false, now);
        let reports = desk.take_reports();
        let points: Vec<(f32, f32)> = reports
            .iter()
            .filter_map(|r| match r {
                WindowReport::Pointer {
                    action: PointerAction::Down,
                    x,
                    y,
                    ..
                } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(points, vec![(20.0, 13.0), (60.0, 36.0)]);
    }

    #[test]
    fn fr12_long_pressing_a_widget_edits_it_like_a_home_screen() {
        let (mut desk, now) = placed(160, true);
        desk.take_effects();
        desk.take_reports();
        let margin = edit_margin(160);
        let (w0, h0) = clock_size(160);

        // A press held still for half a second lifts the widget: Android's press is
        // cancelled, so the long press never clicks, and the window grows by the room for the
        // shadow and the handles, keeping the widget where it was, and takes the keyboard.
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(300));
        assert!(!desk.window(7).unwrap().editing());
        desk.tick(now + Duration::from_millis(500));
        let window = desk.window(7).unwrap();
        assert!(window.editing());
        assert_eq!(window.origin, (100 - margin, 200 - margin));
        assert_eq!(window.size, (w0 + 2 * margin, h0 + 2 * margin));
        assert_eq!(
            pointers(&desk.take_reports()),
            vec![PointerAction::Down, PointerAction::Cancel]
        );
        let effects = desk.take_effects();
        assert!(effects.contains(&Effect::Focus { widget_id: 7 }));

        // Letting go without moving opens the menu under the lifted widget, with Widget
        // settings first, as the widget can be set up again.
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        let (anchor, entries) = menu_of(&desk.take_effects()).expect("the menu opens");
        let Anchor::Below((left, top, right, bottom)) = anchor else {
            panic!("the menu opens under the widget, not at {anchor:?}");
        };
        assert!(left < 100 && top < 200 && right > 100 + w0 && bottom > 200 + h0);
        assert!(
            matches!(&entries[0], Entry::Item { command: menu::SETTINGS, label, .. } if label == "Widget settings")
        );
        assert!(matches!(&entries[1], Entry::Sizes { .. }));

        // Choosing Widget settings ends the edit and opens the widget's setup again.
        desk.menu_closed(7, Some(menu::SETTINGS), false);
        assert!(!desk.window(7).unwrap().editing());
        assert_eq!(
            desk.take_reports(),
            vec![WindowReport::Reconfigure { widget_id: 7 }]
        );

        // Lifted again, dragging moves it, and letting go ends the edit and tells the service.
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(600));
        desk.take_reports();
        desk.moved(7, (50 + margin + 30, 50 + margin + 20));
        desk.up(7, (50 + margin + 30, 50 + margin + 20), Button::Primary);
        let window = desk.window(7).unwrap();
        assert!(!window.editing());
        assert_eq!(window.origin, (130, 220));
        let reports = desk.take_reports();
        assert!(
            matches!(reports.last(), Some(WindowReport::Placed { placement, .. }) if (placement.x, placement.y) == (130, 220)),
            "{reports:?}"
        );
        assert!(
            pointers(&reports).is_empty(),
            "a move is never the widget's: {reports:?}"
        );

        // Lifted again, a handle resizes it within what the widget allows, and letting go
        // ends the edit with the new size in dp.
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(600));
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.menu_closed(7, None, false);
        assert!(
            desk.window(7).unwrap().editing(),
            "Esc or a press on it keeps the edit"
        );
        desk.take_reports();
        let size = desk.window(7).unwrap().size;
        let layout = edit_layout(size.0, size.1, 160, 3);
        let (_, right) = layout
            .handles
            .iter()
            .find(|(hit, _)| *hit == Hit::Right)
            .unwrap();
        let handle = (right.cx as i32, right.cy as i32);
        assert_eq!(desk.hit(7, handle), Some(Hit::Right));
        desk.down(7, handle, Button::Primary, false, now);
        desk.moved(7, (handle.0 + 60, handle.1));
        desk.up(7, (handle.0 + 60, handle.1), Button::Primary);
        let window = desk.window(7).unwrap();
        assert!(!window.editing());
        assert_eq!(window.size, (w0 + 60, h0));
        let reports = desk.take_reports();
        let wider = dp_of_px(w0 + 60, 160);
        assert!(
            matches!(reports.last(), Some(WindowReport::Placed { placement, width, .. }) if placement.width_dp == wider && *width == (w0 + 60) as u32),
            "{reports:?}"
        );
        assert!(
            pointers(&reports).is_empty(),
            "a resize is never the widget's: {reports:?}"
        );

        // A handle never makes it smaller than the widget allows.
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(600));
        let size = desk.window(7).unwrap().size;
        let layout = edit_layout(size.0, size.1, 160, 3);
        let (_, bottom) = layout
            .handles
            .iter()
            .find(|(hit, _)| *hit == Hit::Bottom)
            .unwrap();
        let handle = (bottom.cx as i32, bottom.cy as i32);
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.menu_closed(7, None, false);
        desk.down(7, handle, Button::Primary, false, now);
        desk.moved(7, (handle.0, handle.1 - 500));
        desk.up(7, (handle.0, handle.1 - 500), Button::Primary);
        let smallest = min_window_size(&clock(true).provider, 160);
        assert_eq!(desk.window(7).unwrap().size.1, smallest.1);

        // Esc ends an edit; a press beside the lifted widget ends it too.
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(600));
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.menu_closed(7, None, false);
        desk.key(7, Key::Escape);
        assert!(!desk.window(7).unwrap().editing());
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(600));
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.menu_closed(7, None, true);
        assert!(!desk.window(7).unwrap().editing());
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(600));
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.menu_closed(7, None, false);
        desk.down(7, (1, 1), Button::Primary, false, now);
        assert!(
            !desk.window(7).unwrap().editing(),
            "a press in the room around it ends it"
        );
    }

    #[test]
    fn fr12_a_long_press_anywhere_on_the_widget_edits_it() {
        // Every part of the widget but the edges it is resized by is the widget's, the top
        // right corner too: no button sits there, so a long press there lifts it as anywhere
        // else does.
        let (w0, h0) = clock_size(160);
        for at in [(w0 - 20, 15), (15, 15), (w0 / 2, h0 / 2), (15, h0 - 15)] {
            let (mut desk, now) = placed(160, false);
            assert_eq!(desk.hit(7, at), Some(Hit::Client), "{at:?}");
            desk.down(7, at, Button::Primary, false, now);
            desk.tick(now + HOLD_TO_MOVE);
            assert!(desk.window(7).unwrap().editing(), "{at:?}");
        }
    }

    #[test]
    fn fr12_a_widget_that_cannot_be_set_up_again_has_no_widget_settings() {
        let (mut desk, now) = placed(160, false);
        desk.take_effects();
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(500));
        let margin = edit_margin(160);
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        let (_, entries) = menu_of(&desk.take_effects()).unwrap();
        assert!(!entries.iter().any(|e| matches!(
            e,
            Entry::Item {
                command: menu::SETTINGS,
                ..
            }
        )));
        assert!(matches!(&entries[0], Entry::Sizes { .. }));
    }

    #[test]
    fn fr12_right_clicking_a_widget_offers_resize_and_remove() {
        let (mut desk, now) = placed(160, false);
        desk.take_effects();
        desk.take_reports();
        desk.down(7, (30, 20), Button::Secondary, false, now);
        let (anchor, entries) =
            menu_of(&desk.take_effects()).expect("a right click opens the menu");
        assert_eq!(anchor, Anchor::At(130, 220));
        assert!(matches!(&entries[0], Entry::Sizes { label, .. } if label == "Resize"));
        assert!(matches!(
            entries.last(),
            Some(Entry::Item {
                command: menu::REMOVE,
                ..
            })
        ));
        assert!(
            pointers(&desk.take_reports()).is_empty(),
            "a right click is not the widget's"
        );
        // Large is one and a half times its default size.
        let Entry::Sizes { sizes, .. } = &entries[0] else {
            unreachable!()
        };
        let large = sizes
            .iter()
            .find_map(|s| match s {
                Entry::Item { command, label, .. } if label.starts_with("Large") => Some(*command),
                _ => None,
            })
            .unwrap();
        desk.menu_closed(7, Some(large), false);
        let large_size = window_size(
            &Placement {
                width_dp: 270,
                height_dp: 165,
                ..clock(false).placement
            },
            160,
        );
        assert_eq!(desk.window(7).unwrap().size, large_size);
        assert!(matches!(
            desk.take_reports().last(),
            Some(WindowReport::Placed { placement, .. }) if (placement.width_dp, placement.height_dp) == (270, 165)
        ));
        desk.down(7, (30, 20), Button::Secondary, false, now);
        desk.menu_closed(7, Some(menu::REMOVE), false);
        assert_eq!(
            desk.take_reports(),
            vec![WindowReport::Remove { widget_id: 7 }]
        );
        // The window closes once the service says the widget is gone.
        assert!(desk.window(7).is_some());
        desk.command(WindowCommand::Remove(7), now);
        assert!(desk.window(7).is_none());
        assert!(desk
            .take_effects()
            .contains(&Effect::Close { widget_id: 7 }));
    }

    #[test]
    fn fr12_a_widget_window_is_resized_by_its_edges_in_dp_within_what_the_widget_allows() {
        // At 200% (192 dpi) the window's right edge dragged 120 pixels makes the widget that
        // much wider, in dp at Android's density there.
        let (mut desk, now) = placed(192, false);
        desk.take_reports();
        let (w, h) = desk.window(7).unwrap().size;
        assert_eq!((w, h), clock_size(192));
        assert_eq!(desk.hit(7, (w - 1, h / 2)), Some(Hit::Right));
        desk.down(7, (w - 1, h / 2), Button::Primary, false, now);
        desk.moved(7, (w - 1 + 120, h / 2));
        desk.up(7, (w - 1 + 120, h / 2), Button::Primary);
        assert_eq!(desk.window(7).unwrap().size, (w + 120, h));
        let reports = desk.take_reports();
        assert_eq!(
            reports,
            vec![WindowReport::Placed {
                widget_id: 7,
                placement: Placement {
                    x: 100,
                    y: 200,
                    width_dp: dp_of_px(w + 120, 192),
                    height_dp: dp_of_px(h, 192),
                    resized: false,
                },
                width: (w + 120) as u32,
                height: h as u32,
                density_dpi: android_density(192),
            }]
        );
        // From the left edge the right edge stays.
        desk.down(7, (0, h / 2), Button::Primary, false, now);
        desk.moved(7, (-60, h / 2));
        desk.up(7, (-60, h / 2), Button::Primary);
        let window = desk.window(7).unwrap();
        assert_eq!((window.origin.0, window.size.0), (40, w + 180));
        // On a monitor of another scale it keeps its size in dp.
        let before = placement_of(40, 200, w + 180, h, 192);
        desk.take_reports();
        desk.scale_changed(7, 96);
        let window = desk.window(7).unwrap();
        assert_eq!(window.size, window_size(&before, 96));
        assert!(matches!(
            desk.take_reports().last(),
            Some(WindowReport::Placed { density_dpi, .. }) if *density_dpi == android_density(96)
        ));
    }

    #[test]
    fn fr12_a_resized_widget_passes_its_size_on_when_the_drag_pauses() {
        // Each size Android is told makes it draw the widget anew, so a size is passed on
        // when the drag pauses on it and when it ends, not at every step of the drag.
        let (mut desk, now) = placed(160, false);
        desk.take_reports();
        let (w, h) = desk.window(7).unwrap().size;
        let placed_widths = |desk: &mut Desk| -> Vec<u32> {
            desk.take_reports()
                .iter()
                .filter_map(|r| match r {
                    WindowReport::Placed { width, .. } => Some(*width),
                    _ => None,
                })
                .collect()
        };
        desk.down(7, (w - 1, h / 2), Button::Primary, false, now);
        desk.moved(7, (w - 1 + 10, h / 2));
        assert!(desk.busy(now), "a resize has a size to pass on");
        desk.tick(now);
        assert!(placed_widths(&mut desk).is_empty(), "not before a pause");
        desk.tick(now + Duration::from_millis(200));
        assert_eq!(placed_widths(&mut desk), vec![(w + 10) as u32]);
        // The next pause is passed on no sooner than a quarter of a second after the last.
        desk.moved(7, (w - 1 + 20, h / 2));
        desk.tick(now + Duration::from_millis(300));
        assert!(placed_widths(&mut desk).is_empty());
        desk.tick(now + Duration::from_millis(460));
        assert_eq!(placed_widths(&mut desk), vec![(w + 20) as u32]);
        // Letting go passes the last size on once that much time has gone by, and its image
        // shows unscaled meanwhile: the window is the new size, the picture too.
        desk.moved(7, (w - 1 + 30, h / 2));
        desk.up(7, (w - 1 + 30, h / 2), Button::Primary);
        assert!(desk.busy(now), "the last size is still to pass on");
        desk.tick(now + Duration::from_millis(800));
        assert_eq!(placed_widths(&mut desk), vec![(w + 30) as u32]);
        assert!(!desk.busy(now + Duration::from_millis(800)));
        let picture = desk.picture(7, now).unwrap();
        assert_eq!((picture.width, picture.height), ((w + 30) as u32, h as u32));
    }

    #[test]
    fn fr12_nothing_is_drawn_over_a_widget_and_the_keyboard_opens_its_menu_under_it() {
        let (mut desk, now) = placed(160, false);
        let (w, h) = clock_size(160);
        desk.command(
            WindowCommand::Frame(frame_of(w as u32, h as u32, [10, 20, 30, 255])),
            now,
        );
        // The pointer over the widget draws nothing over it: the picture is the widget's own.
        desk.entered(7, (w - 20, 15));
        desk.moved(7, (w - 20, 15));
        let picture = desk.picture(7, now).unwrap();
        assert!(picture
            .pixels
            .chunks_exact(4)
            .all(|p| p == [30, 20, 10, 255]));
        // A press in its top right corner is the widget's.
        desk.take_reports();
        desk.down(7, (w - 20, 15), Button::Primary, false, now);
        desk.up(7, (w - 20, 15), Button::Primary);
        assert_eq!(
            pointers(&desk.take_reports()),
            vec![PointerAction::Down, PointerAction::Up]
        );
        // The menu key opens the menu under the widget.
        desk.take_effects();
        desk.key(7, Key::Menu);
        let (anchor, _) = menu_of(&desk.take_effects()).expect("the menu key opens the menu");
        assert_eq!(anchor, Anchor::Below((100, 200, 100 + w, 200 + h)));
    }

    #[test]
    fn fr12_a_widget_window_shows_a_placeholder_then_its_picture() {
        let (mut desk, now) = placed(160, false);
        let picture = desk.picture(7, now).unwrap();
        let (w, h) = clock_size(160);
        assert_eq!((picture.width, picture.height), (w as u32, h as u32));
        assert!(
            !all_clear(&picture.pixels),
            "the placeholder shows where it is"
        );
        desk.command(WindowCommand::Frame(frame_of(2, 2, [255, 0, 0, 255])), now);
        let picture = desk.picture(7, now).unwrap();
        assert_eq!(&picture.pixels[0..4], &[0, 0, 255, 255]);
        // Its own size, never scaled: beside the 2 x 2 image the window is clear, but still
        // takes the pointer.
        assert_eq!(&picture.pixels[8..12], &[0, 0, 0, 1]);
        // A picture with nothing in it keeps the last that had something.
        desk.command(WindowCommand::Frame(frame_of(2, 2, [0; 4])), now);
        assert_eq!(
            &desk.picture(7, now).unwrap().pixels[0..4],
            &[0, 0, 255, 255]
        );
        // A broken picture is logged, and the window keeps what it had.
        desk.take_effects();
        desk.command(
            WindowCommand::Frame(WidgetFrame {
                widget_id: 7,
                width: 2,
                height: 2,
                png: b"\x89PNG broken".to_vec(),
            }),
            now,
        );
        assert!(matches!(desk.take_effects().as_slice(), [Effect::Log(_)]));
    }

    #[test]
    fn fr12_one_widget_is_edited_at_a_time() {
        let mut desk = desk(160);
        let now = Instant::now();
        desk.command(WindowCommand::Place(clock(false)), now);
        desk.command(
            WindowCommand::Place(Widget {
                widget_id: 8,
                placement: Placement {
                    x: 600,
                    ..clock(false).placement
                },
                ..clock(false)
            }),
            now,
        );
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(500));
        let margin = edit_margin(160);
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.menu_closed(7, None, false);
        assert!(desk.window(7).unwrap().editing());
        desk.down(8, (50, 50), Button::Primary, false, now);
        assert!(!desk.window(7).unwrap().editing());
        // Losing the keyboard to another window ends an edit, but not to its own menu.
        desk.up(8, (50, 50), Button::Primary);
        desk.down(7, (50, 50), Button::Primary, false, now);
        desk.tick(now + Duration::from_millis(500));
        desk.up(7, (50 + margin, 50 + margin), Button::Primary);
        desk.focus(7, false);
        assert!(desk.window(7).unwrap().editing(), "its menu is open");
        desk.menu_closed(7, None, false);
        desk.focus(7, false);
        assert!(!desk.window(7).unwrap().editing());
    }

    #[test]
    fn fr12_a_finger_may_wander_a_little_and_still_long_press() {
        let (mut desk, now) = placed(160, false);
        desk.down(7, (50, 50), Button::Primary, true, now);
        // A force click on a trackpad lifts it at once, as holding it would.
        let (mut forced, now) = placed(160, false);
        forced.down(7, (50, 50), Button::Primary, false, now);
        forced.force_press(7);
        assert!(forced.window(7).unwrap().editing());
        // Ten pixels at 160 dpi is past the mouse's threshold (4 device-independent pixels,
        // 7 here), not a finger's (8, 13 here).
        desk.moved(7, (60, 50));
        desk.tick(now + Duration::from_millis(500));
        assert!(desk.window(7).unwrap().editing());
    }
}

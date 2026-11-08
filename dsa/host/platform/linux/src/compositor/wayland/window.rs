//! A task's toplevel: its surface, its buffers, its title bar, and the window rules' changes
//! applied to it.

use std::time::Instant;

use dsa_host_core::compositor::caption::{
    scaled, CaptionButton, CaptionLayout, CaptionState, CaptionTone, TITLE_TEXT_AT_100,
};
use dsa_host_core::compositor::task_windows::WindowChange;
use dsa_host_core::compositor::window_hosts::FromHost;
use dsa_host_core::compositor::window_mode::{picture_in_picture_place, WindowMode};
use dsa_host_core::ime::ImeCursor;
use dsa_host_core::input::PointerMapper;
use dsa_host_core::keys::KeyMapper;
use dsa_host_core::tasks::AppInfo;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{
    self, ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};

use super::shm::ShmBuffer;
use super::State;
use crate::compositor::frames::Frame;
use crate::compositor::text::Text;
use crate::compositor::title_bar::{
    caption_icon_png, frame_band_fill, place_text, BandArt, CaptionIcon, TitleBar,
};
use crate::compositor::toplevel::{Reported, States, Toplevel};
use crate::input::text_input::TextInputV3;
use crate::shell::desktop_entry::app_id;

/// How many buffers a window cycles through, so it can draw while the desktop reads one.
const BUFFERS: usize = 3;

/// The color a window shows before its task's first frame.
const BLANK: u32 = 0x00_20_20_20;

/// The margin of a picture-in-picture window from the work area's corner, at 100%.
const PIP_MARGIN: i32 = 16;

/// One task window.
pub struct Window {
    pub model: Toplevel,
    pub surface: WlSurface,
    xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    decoration: Option<ZxdgToplevelDecorationV1>,
    buffers: Vec<ShmBuffer>,
    /// Whether the first configure has been acknowledged; nothing is drawn before.
    configured: bool,
    /// The frame shown last (without the title bar), drawn again when the title bar changes.
    pub(crate) last: Option<Frame>,
    /// A frame that waits for a free buffer.
    waiting: Option<Frame>,
    pub caption: CaptionState,
    pub tone: CaptionTone,
    pub pointer: PointerMapper,
    pub keys: KeyMapper,
    pub ime: TextInputV3,
    pub ime_cursor: Option<ImeCursor>,
    /// When the title bar was last clicked, for a double click.
    pub last_caption_click: Option<Instant>,
    /// Whether the window was reported to the window rules as opened (with its DPI).
    pub dpi_told: bool,
    /// Crops a GPU image to the frame in it, when frames come as dmabufs.
    pub(crate) viewport: Option<wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport>,
    /// The title bar's buttons over a GPU frame, which the CPU cannot draw into.
    pub(crate) caption_layer: Option<super::gpu::CaptionLayer>,
    /// The GPU frame shown last, drawn again when the title bar changes.
    pub(crate) last_gpu: Option<(u32, crate::compositor::frames::GpuFrame)>,
    /// The band's fill over GPU frames, as sampled last, and when it was sampled.
    pub(crate) gpu_fill: Option<u32>,
    pub(crate) band_probed: Option<Instant>,
    /// Move or Size, chosen in the window menu: the next press in the window starts it.
    pub(crate) armed: Option<crate::compositor::window_menu::MoveSize>,
    /// Where the task is shown while in picture-in-picture, instead of its toplevel.
    pub(crate) pip: Option<super::pip::Pip>,
    /// Whether the app draws its own title bar, and whether the icon's cell shows the back
    /// arrow.
    pub title_bar: TitleBar,
    /// The app's title bar icon: its adaptive icon's foreground, else its whole icon.
    pub(crate) icon: CaptionIcon,
    /// Whether the app supports several windows, so the window menu offers New window.
    pub multiple_windows: bool,
    /// The app's name as drawn last: the name, the size it was drawn for, and its coverage.
    title_text: Option<(String, (u32, u32), Vec<u8>)>,
    /// The band drawn last, and what it was drawn from, so an unchanged band is not drawn
    /// again for every frame.
    band: Option<(BandKey, Vec<u8>)>,
}

/// What a title bar band is drawn from.
#[derive(Debug, Clone, PartialEq)]
struct BandKey {
    layout: CaptionLayout,
    dpi: u32,
    tone: CaptionTone,
    state: CaptionState,
    title_bar: TitleBar,
    title: String,
    fill: Option<u32>,
}

/// What a window shows of its app besides its frames.
#[derive(Debug, Clone, Default)]
pub struct AppLook {
    /// The PNG the title bar draws.
    pub caption_icon_png: Vec<u8>,
    pub multiple_windows: bool,
}

impl AppLook {
    pub fn of(app: &AppInfo) -> AppLook {
        AppLook {
            caption_icon_png: caption_icon_png(app).to_vec(),
            multiple_windows: app.multiple_windows,
        }
    }
}

impl State {
    /// Opens the task's window.
    pub(crate) fn open_window(
        &mut self,
        task_id: i32,
        package: &str,
        title: &str,
        size: (u32, u32),
    ) {
        if self.windows.contains_key(&task_id) {
            return;
        }
        let surface = self.compositor.create_surface(&self.qh, task_id);
        let xdg_surface = self.wm_base.get_xdg_surface(&surface, &self.qh, task_id);
        let toplevel = xdg_surface.get_toplevel(&self.qh, task_id);
        toplevel.set_app_id(app_id(package));
        toplevel.set_title(title.to_string());
        let decoration = self.decorations.as_ref().map(|manager| {
            let decoration = manager.get_toplevel_decoration(&toplevel, &self.qh, ());
            decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ClientSide);
            decoration
        });
        let look = self.app_looks.get(package).cloned().unwrap_or_default();
        let model = Toplevel::new(task_id, package, title, size);
        let (width, height) = model.to_logical(size);
        xdg_surface.set_window_geometry(0, 0, width, height);
        // The first commit, with no buffer, asks the desktop for the first configure.
        surface.commit();
        self.windows.insert(
            task_id,
            Window {
                model,
                surface,
                xdg_surface,
                toplevel,
                decoration,
                buffers: Vec::new(),
                configured: false,
                last: None,
                waiting: None,
                caption: CaptionState::default(),
                tone: CaptionTone::default(),
                pointer: PointerMapper::new(task_id),
                keys: KeyMapper::new(task_id),
                ime: TextInputV3::new(task_id),
                ime_cursor: None,
                last_caption_click: None,
                dpi_told: false,
                viewport: None,
                caption_layer: None,
                last_gpu: None,
                gpu_fill: None,
                band_probed: None,
                armed: None,
                pip: None,
                title_bar: TitleBar::default(),
                icon: CaptionIcon::new(&look.caption_icon_png),
                multiple_windows: look.multiple_windows,
                title_text: None,
                band: None,
            },
        );
    }

    /// The guest told of `app`: its windows draw its title bar icon and offer New window as it
    /// says, and so do the windows it opens later.
    pub fn set_app(&mut self, app: &AppInfo) {
        let look = AppLook::of(app);
        if self.app_looks.get(&app.package_name).is_some_and(|known| {
            known.caption_icon_png == look.caption_icon_png
                && known.multiple_windows == look.multiple_windows
        }) {
            return;
        }
        let shown: Vec<i32> = self
            .windows
            .iter_mut()
            .filter(|(_, window)| window.model.package == app.package_name)
            .map(|(task_id, window)| {
                window.icon = CaptionIcon::new(&look.caption_icon_png);
                window.multiple_windows = look.multiple_windows;
                window.band = None;
                *task_id
            })
            .collect();
        self.app_looks.insert(app.package_name.clone(), look);
        for task_id in shown {
            self.redraw(task_id);
        }
    }

    /// Applies one of the window rules' changes.
    pub fn change(&mut self, change: &WindowChange) {
        match change {
            WindowChange::Open {
                task_id,
                package_name,
                title,
                width,
                height,
                ..
            } => self.open_window(*task_id, package_name, title, (*width, *height)),
            WindowChange::Retitle { task_id, title } => {
                if let Some(window) = self.windows.get_mut(task_id) {
                    window.model.title = title.clone();
                    window.toplevel.set_title(title.clone());
                }
                self.redraw(*task_id);
            }
            WindowChange::Resize {
                task_id,
                width,
                height,
            } => {
                // The window takes the size with the task's next frame; until then a window
                // with no frame yet shows a blank of the size.
                if let Some(window) = self.windows.get_mut(task_id) {
                    if window.last.is_none() {
                        window.model.frame_shown((*width, *height));
                    }
                }
            }
            WindowChange::Close { task_id } => self.close_window(*task_id),
            WindowChange::TaskSize {
                task_id,
                width,
                height,
                max,
                min,
            } => {
                if let Some(window) = self.windows.get_mut(task_id) {
                    window.model.task = (*width, *height);
                    window.model.max = *max;
                    window.model.min = *min;
                    window.limits();
                }
            }
            // The input method places its windows by the cursor alone; it reads no surrounding
            // text from the editor's description yet.
            WindowChange::ImeEditor { .. } => {}
            WindowChange::ImeCursor { task_id, cursor } => {
                if let Some(window) = self.windows.get_mut(task_id) {
                    window.ime_cursor = Some(*cursor);
                }
                if self.seat_state.keyboard_focus == Some(*task_id) {
                    self.place_input_method(*task_id);
                }
            }
            WindowChange::CaptionTone { task_id, tone } => {
                if let Some(window) = self.windows.get_mut(task_id) {
                    window.tone = *tone;
                }
                self.redraw(*task_id);
            }
            WindowChange::Mode {
                task_id,
                mode,
                resizable,
            } => {
                let work = self.work_area();
                let Some(window) = self.windows.get_mut(task_id) else {
                    return;
                };
                window.model.resizable = *resizable;
                let shown = window.model.mode.app_asks(*mode);
                let small = window.apply_mode(shown, work);
                window.limits();
                self.after_mode(*task_id);
                self.picture_in_picture(*task_id, small);
            }
            WindowChange::CaptionMode {
                task_id,
                app_drawn,
                interactive,
            } => {
                if let Some(window) = self.windows.get_mut(task_id) {
                    window.title_bar.set_mode(*app_drawn, interactive.clone());
                }
                self.redraw(*task_id);
            }
            WindowChange::CaptionBack { task_id, back } => {
                if let Some(window) = self.windows.get_mut(task_id) {
                    window.title_bar.set_back(*back);
                }
                self.redraw(*task_id);
            }
        }
    }

    /// The user pressed F11 in the task's window.
    pub(crate) fn toggle_fullscreen(&mut self, task_id: i32) {
        let work = self.work_area();
        let small = match self.windows.get_mut(&task_id) {
            Some(window) => {
                let shown = window.model.mode.toggle_fullscreen();
                window.apply_mode(shown, work)
            }
            None => return,
        };
        self.after_mode(task_id);
        self.picture_in_picture(task_id, small);
    }

    /// Asks the task for the small size of a picture-in-picture window, and moves it to a
    /// surface that stays above the others; or brings its toplevel back when it left
    /// picture-in-picture.
    fn picture_in_picture(&mut self, task_id: i32, size: Option<(u32, u32)>) {
        match size {
            Some((width, height)) => {
                self.enter_pip(task_id, (width, height));
                self.report(FromHost::Resized {
                    task_id,
                    width,
                    height,
                });
            }
            None => self.leave_pip(task_id),
        }
    }

    /// Tells the window rules the title bar's band after the mode or the scale changed, and
    /// draws.
    fn after_mode(&mut self, task_id: i32) {
        let caption = self
            .windows
            .get_mut(&task_id)
            .and_then(|window| window.model.caption_change());
        if let Some(band) = caption {
            self.report(FromHost::Caption { task_id, band });
        }
        self.redraw(task_id);
    }

    pub(crate) fn close_window(&mut self, task_id: i32) {
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.task_id == task_id)
        {
            self.close_menu();
        }
        self.leave_pip(task_id);
        if let Some(window) = self.windows.remove(&task_id) {
            if let Some(viewport) = &window.viewport {
                viewport.destroy();
            }
            if let Some(layer) = window.caption_layer {
                layer.destroy();
            }
            if let Some(decoration) = &window.decoration {
                decoration.destroy();
            }
            window.toplevel.destroy();
            window.xdg_surface.destroy();
            window.surface.destroy();
        }
        if self.seat_state.keyboard_focus == Some(task_id) {
            self.seat_state.keyboard_focus = None;
        }
        if self.seat_state.pointer_focus == Some(task_id) {
            self.seat_state.pointer_focus = None;
        }
    }

    /// Shows a frame of the task in its window.
    pub fn present(&mut self, task_id: i32, frame: Frame) {
        if self
            .windows
            .get(&task_id)
            .is_some_and(|w| matches!(w.pip, Some(super::pip::Pip::X11)))
        {
            if let Some(x11) = &mut self.x11 {
                x11.show_pixels(task_id, &frame);
            }
            if let Some(window) = self.windows.get_mut(&task_id) {
                window.model.frame_shown((frame.width, frame.height));
            }
            return;
        }
        let shm = self.shm.clone();
        let qh = self.qh.clone();
        if let Some(window) = self.windows.get_mut(&task_id) {
            window.waiting = Some(frame);
            window.draw(&shm, &qh, self.text.as_ref());
        }
    }

    /// Draws the window again with its last frame: its title bar changed.
    pub(crate) fn redraw(&mut self, task_id: i32) {
        let shm = self.shm.clone();
        let qh = self.qh.clone();
        if self
            .windows
            .get(&task_id)
            .is_some_and(|w| matches!(w.pip, Some(super::pip::Pip::X11)))
        {
            // The X11 window keeps its last frame, and has no title bar to draw again.
            return;
        }
        if let Some((display_id, frame)) = self.windows.get(&task_id).and_then(|w| w.last_gpu) {
            self.present_gpu(task_id, display_id, frame);
            return;
        }
        if let Some(window) = self.windows.get_mut(&task_id) {
            if window.waiting.is_none() {
                window.waiting = window.last.clone();
            }
            window.draw(&shm, &qh, self.text.as_ref());
        }
    }

    /// Draws every window whose frame waits for a free buffer.
    pub fn draw_waiting(&mut self) {
        let shm = self.shm.clone();
        let qh = self.qh.clone();
        for window in self.windows.values_mut() {
            if window.waiting.is_some() {
                window.draw(&shm, &qh, self.text.as_ref());
            }
        }
    }
}

impl Window {
    /// Sets the smallest and largest sizes the desktop may give the window, in logical
    /// units: the guest's limits for the task, none for a window the user may not resize
    /// but its own size.
    fn limits(&self) {
        let model = &self.model;
        if !model.resizable && model.mode.effective() == WindowMode::Normal {
            let (width, height) = model.to_logical(model.shown);
            self.toplevel.set_min_size(width, height);
            self.toplevel.set_max_size(width, height);
            return;
        }
        let (min_width, min_height) = model.min.map(|min| model.to_logical(min)).unwrap_or((0, 0));
        let (max_width, max_height) = model.max.map(|max| model.to_logical(max)).unwrap_or((0, 0));
        self.toplevel.set_min_size(min_width, min_height);
        self.toplevel.set_max_size(max_width, max_height);
    }

    /// Shows the window in `mode`. Returns the size a picture-in-picture window asks the
    /// task for.
    fn apply_mode(&mut self, mode: WindowMode, work: (i32, i32)) -> Option<(u32, u32)> {
        match mode {
            WindowMode::Fullscreen => {
                self.toplevel.set_fullscreen(None);
                None
            }
            WindowMode::Normal => {
                self.toplevel.unset_fullscreen();
                None
            }
            WindowMode::PictureInPicture => {
                // Wayland lets no window place itself or stay above the others; the window
                // takes the small size, and the desktop places it.
                self.toplevel.unset_fullscreen();
                let scale = self.model.scale.max(1) as i32;
                let (_, _, width, height) = picture_in_picture_place(
                    self.model.task,
                    (0, 0, work.0 * scale, work.1 * scale),
                    PIP_MARGIN * scale,
                );
                Some((width, height))
            }
        }
    }

    /// Draws the waiting frame, with the title bar over it, into a free buffer.
    /// Whether the window may draw: its first configure has been acknowledged.
    pub(crate) fn is_configured(&self) -> bool {
        match &self.pip {
            Some(pip) => pip.is_configured(),
            None => self.configured,
        }
    }

    /// The surface the frames go to: the picture-in-picture surface, else the toplevel's.
    pub(crate) fn target(&self) -> &WlSurface {
        self.pip
            .as_ref()
            .and_then(|pip| pip.surface())
            .unwrap_or(&self.surface)
    }

    /// Takes the toplevel off the screen, for picture-in-picture elsewhere.
    pub(crate) fn unmap(&mut self) {
        self.surface.attach(None, 0, 0);
        self.surface.commit();
        self.configured = false;
        self.buffers.clear();
    }

    /// Puts the toplevel back: a commit with no buffer asks for a new first configure.
    pub(crate) fn remap(&mut self) {
        self.surface.commit();
    }

    pub(crate) fn xdg_surface(&self) -> &XdgSurface {
        &self.xdg_surface
    }

    /// The title bar's band as it is now, over a frame of the window's size: premultiplied
    /// BGRA rows of the layout's width and height, with the layout. `fill` fills a band the
    /// app left empty. The band is drawn again only when something it shows changed.
    pub(crate) fn band_pixels(
        &mut self,
        text: Option<&Text>,
        fill: Option<u32>,
    ) -> (CaptionLayout, &[u8]) {
        let layout = self.model.caption_layout();
        let dpi = self.model.dpi();
        let key = BandKey {
            layout,
            dpi,
            tone: self.tone,
            state: CaptionState {
                maximized: self.model.states.maximized,
                active: self.model.states.activated,
                ..self.caption
            },
            title_bar: self.title_bar.clone(),
            title: self.model.title.clone(),
            fill,
        };
        if self.band.as_ref().map(|(drawn, _)| drawn) != Some(&key) {
            self.update_title_text(&layout, dpi, text);
            let title = self
                .title_text
                .as_ref()
                .map(|(_, _, coverage)| coverage.as_slice());
            let icon = self.icon.at(layout.icon);
            let pixels = self.title_bar.paint(
                &layout,
                dpi,
                self.tone,
                &key.state,
                BandArt { icon, title, fill },
            );
            self.band = Some((key, pixels));
        }
        let pixels = self
            .band
            .as_ref()
            .map_or(&[][..], |(_, pixels)| pixels.as_slice());
        (layout, pixels)
    }

    /// Lays the app's name out again when it or the room for it changed. An app that draws
    /// its own title bar shows no name, and a desktop without Pango has none to show.
    fn update_title_text(&mut self, layout: &CaptionLayout, dpi: u32, text: Option<&Text>) {
        let Some((_, _, width, height)) = layout.title_rect() else {
            self.title_text = None;
            return;
        };
        let (Some(text), false) = (text, self.title_bar.app_drawn) else {
            self.title_text = None;
            return;
        };
        let size = (width, height);
        let title = &self.model.title;
        if self
            .title_text
            .as_ref()
            .is_some_and(|(drawn, at, _)| drawn == title && *at == size)
        {
            return;
        }
        let px = f64::from(scaled(TITLE_TEXT_AT_100, dpi));
        self.title_text = text
            .line(title, px, Some(width))
            .map(|line| (title.clone(), size, place_text(&line, width, height)));
    }

    fn draw(
        &mut self,
        shm: &wayland_client::protocol::wl_shm::WlShm,
        qh: &QueueHandle<State>,
        text: Option<&Text>,
    ) {
        if !self.is_configured() {
            return;
        }
        if self.last_gpu.is_some() && self.waiting.is_none() {
            return;
        }
        self.last_gpu = None;
        if let Some(layer) = &mut self.caption_layer {
            layer.hide();
        }
        if let Some(viewport) = &self.viewport {
            viewport.set_source(-1.0, -1.0, -1.0, -1.0);
            viewport.set_destination(-1, -1);
        }
        let frame = match self.waiting.take() {
            Some(frame) => frame,
            None if self.last.is_none() => {
                let (width, height) = self.model.shown;
                Frame::solid(width, height, BLANK)
            }
            None => return,
        };
        let size = (frame.width, frame.height);
        self.buffers
            .retain(|buffer| buffer.size == size || buffer.is_busy());
        let free = self
            .buffers
            .iter()
            .position(|buffer| buffer.size == size && !buffer.is_busy());
        let at = match free {
            Some(at) => at,
            None if self.buffers.len() < BUFFERS => match ShmBuffer::new(shm, qh, size) {
                Ok(buffer) => {
                    self.buffers.push(buffer);
                    self.buffers.len() - 1
                }
                Err(_) => return,
            },
            None => {
                // Every buffer is on screen; the frame waits for one to come back.
                self.waiting = Some(frame);
                return;
            }
        };
        self.model.frame_shown(size);
        let mut shown = frame.clone();
        if self.model.has_caption() {
            // The band takes the app's top color when the app leaves it empty.
            let band = self.model.caption_height();
            let fill = frame_band_fill(&frame.pixels, frame.width, frame.height, band);
            let (layout, pixels) = self.band_pixels(text, fill);
            shown.blend(pixels, (layout.width, layout.height), (0, 0));
        }
        let buffer = &mut self.buffers[at];
        buffer.write(&shown.pixels);
        buffer.set_busy();
        let (width, height) = self.model.to_logical(size);
        let target = self
            .pip
            .as_ref()
            .and_then(|pip| pip.surface())
            .unwrap_or(&self.surface);
        target.set_buffer_scale(self.model.scale.max(1) as i32);
        target.attach(Some(&buffer.buffer), 0, 0);
        target.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        if self.pip.is_none() {
            self.xdg_surface.set_window_geometry(0, 0, width, height);
        }
        target.commit();
        self.last = Some(frame);
    }

    /// Which caption button the pointer is over, if any, for the hover look.
    pub fn hover(&mut self, button: Option<CaptionButton>) -> bool {
        if self.caption.hovered == button {
            return false;
        }
        self.caption.hovered = button;
        true
    }
}

impl Dispatch<WlSurface, i32> for State {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        task_id: &i32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let scale = match event {
            wl_surface::Event::Enter { output } => {
                let name = wayland_client::Proxy::data::<u32>(&output).copied();
                name.map(|name| state.output_scale(name))
            }
            wl_surface::Event::PreferredBufferScale { factor } => Some(factor.max(1) as u32),
            _ => None,
        };
        let Some(scale) = scale else {
            return;
        };
        let task_id = *task_id;
        let Some(window) = state.windows.get_mut(&task_id) else {
            return;
        };
        if let Some(dpi) = window.model.rescale(scale) {
            if window.dpi_told {
                state.report(FromHost::Dpi { task_id, dpi });
            }
            state.after_mode(task_id);
        }
    }
}

impl Dispatch<XdgSurface, i32> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        task_id: &i32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_surface::Event::Configure { serial } = event else {
            return;
        };
        xdg_surface.ack_configure(serial);
        let task_id = *task_id;
        let first = match state.windows.get_mut(&task_id) {
            Some(window) => !std::mem::replace(&mut window.configured, true),
            None => return,
        };
        if first {
            // The window is open: the window rules hear its DPI and its title bar's height.
            let (dpi, caption) = match state.windows.get_mut(&task_id) {
                Some(window) => {
                    window.dpi_told = true;
                    window.limits();
                    (window.model.dpi(), window.model.caption_change())
                }
                None => return,
            };
            state.report(FromHost::Dpi { task_id, dpi });
            if let Some(band) = caption {
                state.report(FromHost::Caption { task_id, band });
            }
        }
        state.redraw(task_id);
    }
}

/// The states of an `xdg_toplevel.configure`, from its array of 32-bit values.
fn states(raw: &[u8]) -> States {
    let mut states = States::default();
    for value in raw.chunks_exact(4) {
        let value = u32::from_ne_bytes([value[0], value[1], value[2], value[3]]);
        match xdg_toplevel::State::try_from(value) {
            Ok(xdg_toplevel::State::Maximized) => states.maximized = true,
            Ok(xdg_toplevel::State::Fullscreen) => states.fullscreen = true,
            Ok(xdg_toplevel::State::Resizing) => states.resizing = true,
            Ok(xdg_toplevel::State::Activated) => states.activated = true,
            Ok(xdg_toplevel::State::Suspended) => states.suspended = true,
            _ => {}
        }
    }
    states
}

impl Dispatch<XdgToplevel, i32> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        task_id: &i32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let task_id = *task_id;
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states: raw,
            } => {
                let Some(window) = state.windows.get_mut(&task_id) else {
                    return;
                };
                let maximized = window.model.states.maximized;
                let activated = window.model.states.activated;
                let reported = window.model.configure((width, height), states(&raw));
                let maximized_changed = window.model.states.maximized != maximized;
                let activated_changed = window.model.states.activated != activated;
                for report in reported {
                    let report = match report {
                        Reported::Resized { width, height } => FromHost::Resized {
                            task_id,
                            width,
                            height,
                        },
                        Reported::Dragging(dragging) => FromHost::Dragging { task_id, dragging },
                        Reported::Focus(focused) => FromHost::Focus { task_id, focused },
                        Reported::Shown(shown) => FromHost::Shown { task_id, shown },
                    };
                    state.report(report);
                }
                if maximized_changed || activated_changed {
                    state.redraw(task_id);
                }
            }
            xdg_toplevel::Event::Close => {
                state.report(FromHost::Closed { task_id });
            }
            _ => {}
        }
    }
}

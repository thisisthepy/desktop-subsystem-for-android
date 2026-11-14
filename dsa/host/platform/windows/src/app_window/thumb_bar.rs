//! The media buttons in the taskbar preview of an app's task windows: previous, play or pause,
//! and next, as a music player's preview has them.
//!
//! While the app has a media session, the window manager sends what its buttons show
//! (`ToHost::AppMedia`): whether it plays, and which buttons the session allows. Each window
//! adds its three buttons once the taskbar has made its button (`TaskbarButtonCreated`), through
//! `ITaskbarList3::ThumbBarAddButtons`, and changes them with `ThumbBarUpdateButtons` whenever
//! the playback state or the allowed actions change. A button the session does not allow is
//! disabled; when the session ends the buttons are hidden, since the taskbar cannot take them
//! away. The middle button shows Pause while the app plays and Play otherwise.
//!
//! The icons are Segoe Fluent Icons glyphs drawn at the size of a small icon at the window's
//! DPI, in the colour of the taskbar's theme (dark glyphs on a light taskbar, white ones on a
//! dark taskbar) or the high contrast theme's text colour, and drawn again when the window
//! moves to a monitor of another scale or the theme changes. A click (`THBN_CLICKED`) goes to
//! the window manager as `FromHost::MediaButton`, which sends it to the app's media session as
//! a media key's press is sent.

use std::cell::{Cell, RefCell};

use dsa_host_core::compositor::window_hosts::{MediaButton, MediaButtons};

use crate::compositor::fluent::{glyph, Argb};

/// The identifiers of the three buttons, which a click names.
pub const PREVIOUS_ID: u32 = 1;
pub const PLAY_PAUSE_ID: u32 = 2;
pub const NEXT_ID: u32 = 3;

/// The longest tooltip a button takes, in UTF-16 units, without its NUL.
pub const MAX_TIP: usize = 259;

/// The words of the buttons' tooltips, in the user's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Texts {
    pub previous: &'static str,
    pub play: &'static str,
    pub pause: &'static str,
    pub next: &'static str,
}

impl Texts {
    pub fn english() -> Texts {
        Texts {
            previous: "Previous",
            play: "Play",
            pause: "Pause",
            next: "Next",
        }
    }

    pub fn korean() -> Texts {
        Texts {
            previous: "이전",
            play: "재생",
            pause: "일시 정지",
            next: "다음",
        }
    }

    /// The words for the language `lang_id` (a Windows language ID): Korean for Korean,
    /// English for every other language.
    pub fn for_language(lang_id: u16) -> Texts {
        if lang_id & 0x3ff == 0x12 {
            Texts::korean()
        } else {
            Texts::english()
        }
    }
}

/// One button as the taskbar is given it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbButton {
    pub id: u32,
    pub glyph: char,
    pub tip: &'static str,
    pub enabled: bool,
    pub hidden: bool,
}

/// The three buttons for `media`, the app's session, or for no session (`None`), in their
/// order on the preview: previous, play or pause, next.
pub fn buttons(media: Option<&MediaButtons>, texts: &Texts) -> [ThumbButton; 3] {
    let hidden = media.is_none();
    let media = media.copied().unwrap_or_default();
    let (middle_glyph, middle_tip, middle) = if media.playing {
        (glyph::PAUSE, texts.pause, MediaButton::Pause)
    } else {
        (glyph::PLAY, texts.play, MediaButton::Play)
    };
    [
        ThumbButton {
            id: PREVIOUS_ID,
            glyph: glyph::PREVIOUS,
            tip: texts.previous,
            enabled: media.allows(MediaButton::Previous),
            hidden,
        },
        ThumbButton {
            id: PLAY_PAUSE_ID,
            glyph: middle_glyph,
            tip: middle_tip,
            enabled: media.allows(middle),
            hidden,
        },
        ThumbButton {
            id: NEXT_ID,
            glyph: glyph::NEXT,
            tip: texts.next,
            enabled: media.allows(MediaButton::Next),
            hidden,
        },
    ]
}

/// What a click on the button `id` asks of the session `media`: `None` for a button the
/// session does not allow now, or no session at all.
pub fn pressed(id: u32, media: Option<&MediaButtons>) -> Option<MediaButton> {
    let media = media?;
    let button = match id {
        PREVIOUS_ID => MediaButton::Previous,
        PLAY_PAUSE_ID if media.playing => MediaButton::Pause,
        PLAY_PAUSE_ID => MediaButton::Play,
        NEXT_ID => MediaButton::Next,
        _ => return None,
    };
    media.allows(button).then_some(button)
}

/// The side of a button's icon at `dpi`, in pixels: a small icon's (16 at 100%), as the
/// taskbar draws them.
pub fn icon_side(dpi: u32) -> u32 {
    (16 * dpi.max(96) + 48) / 96
}

/// The colour the glyphs are drawn in: the high contrast theme's text colour while one is on,
/// otherwise WinUI's primary text colour for the taskbar's theme.
pub fn glyph_color(light_taskbar: bool, high_contrast_text: Option<Argb>) -> Argb {
    match high_contrast_text {
        Some(text) => text | 0xFF00_0000,
        None if light_taskbar => 0xE400_0000,
        None => 0xFFFF_FFFF,
    }
}

/// A glyph's coverage `mask` in `color`, as rows of straight-alpha BGRA pixels, the format an
/// icon is made from.
pub fn glyph_pixels(mask: &[u8], color: Argb) -> Vec<u8> {
    let alpha = (color >> 24) & 0xff;
    let (r, g, b) = ((color >> 16) as u8, (color >> 8) as u8, color as u8);
    mask.iter()
        .flat_map(|&cover| {
            let a = (u32::from(cover) * alpha + 127) / 255;
            if a == 0 {
                [0, 0, 0, 0]
            } else {
                [b, g, r, a as u8]
            }
        })
        .collect()
}

/// `text` as a button's tooltip: UTF-16, cut to fit, NUL-terminated.
pub fn tip_units(text: &str) -> [u16; MAX_TIP + 1] {
    let mut out = [0u16; MAX_TIP + 1];
    for (slot, unit) in out.iter_mut().zip(text.encode_utf16().take(MAX_TIP)) {
        *slot = unit;
    }
    out
}

thread_local! {
    /// The app's media session as the window manager last said, on the windows' thread: every
    /// window of the program is on that one thread.
    static CURRENT: Cell<Option<MediaButtons>> = const { Cell::new(None) };
}

/// Records the app's media session, or its end, for every window of this program. Returns
/// whether it changed.
pub fn set(media: Option<MediaButtons>) -> bool {
    CURRENT.with(|current| current.replace(media) != media)
}

/// The app's media session as the window manager last said.
pub fn current() -> Option<MediaButtons> {
    CURRENT.with(Cell::get)
}

/// The message the taskbar sends a window once it has made the window's button, and again
/// after Explorer restarts.
pub fn button_created_message() -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
    thread_local! {
        static MESSAGE: u32 = {
            let name: Vec<u16> = "TaskbarButtonCreated"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: a NUL-terminated name that outlives the call.
            unsafe { RegisterWindowMessageW(name.as_ptr()) }
        };
    }
    MESSAGE.with(|message| *message)
}

/// Whether a `WM_COMMAND`'s `wparam` is a click on a button of the preview, and on which.
pub fn clicked(wparam: usize) -> Option<u32> {
    const THBN_CLICKED: usize = 0x1800;
    ((wparam >> 16) & 0xffff == THBN_CLICKED).then_some((wparam & 0xffff) as u32)
}

/// The icons of the three buttons, for one DPI and colour, destroyed once the taskbar no
/// longer shows them.
struct Icons {
    side: u32,
    color: Argb,
    /// By glyph: previous, play, pause, next.
    handles: [windows_sys::Win32::UI::WindowsAndMessaging::HICON; 4],
}

impl Icons {
    const GLYPHS: [char; 4] = [glyph::PREVIOUS, glyph::PLAY, glyph::PAUSE, glyph::NEXT];

    fn draw(side: u32, color: Argb) -> Icons {
        use crate::compositor::fluent::text::{mask, Align, ICON_FACES};
        let handles = Icons::GLYPHS.map(|symbol| {
            let size = side as usize;
            mask(
                &symbol.to_string(),
                &ICON_FACES,
                side as i32,
                (size, size),
                Align::Center,
            )
            .map(|cover| super::live_icon::icon(&glyph_pixels(&cover, color), side))
            .unwrap_or(std::ptr::null_mut())
        });
        Icons {
            side,
            color,
            handles,
        }
    }

    fn of(&self, symbol: char) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
        Icons::GLYPHS
            .iter()
            .position(|g| *g == symbol)
            .map(|index| self.handles[index])
            .unwrap_or(std::ptr::null_mut())
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
        for handle in self.handles {
            if !handle.is_null() {
                // SAFETY: an icon made above, which the taskbar has copied.
                unsafe { DestroyIcon(handle) };
            }
        }
    }
}

/// One task window's buttons.
#[derive(Default)]
pub struct ThumbBar {
    taskbar: RefCell<Option<windows::Win32::UI::Shell::ITaskbarList3>>,
    /// Whether the buttons were added to the window's current taskbar button.
    added: Cell<bool>,
    /// Whether the taskbar has made the window's button, so buttons can be added.
    ready: Cell<bool>,
    icons: RefCell<Option<Icons>>,
}

impl ThumbBar {
    pub fn new() -> ThumbBar {
        ThumbBar::default()
    }

    /// The taskbar made the window's button, for the first time or after Explorer restarted:
    /// the buttons are added to it when the app has a media session, else once it has one.
    ///
    /// # Safety
    ///
    /// `hwnd` is a live window of the calling thread.
    pub unsafe fn button_created(&self, hwnd: isize) -> Result<(), String> {
        self.ready.set(true);
        self.added.set(false);
        // A new Explorer is a new taskbar object.
        self.taskbar.replace(None);
        self.update(hwnd)
    }

    /// Shows the app's media session as it is now on the window's buttons.
    ///
    /// # Safety
    ///
    /// `hwnd` is a live window of the calling thread.
    pub unsafe fn update(&self, hwnd: isize) -> Result<(), String> {
        let media = current();
        // A window whose app never had a session gets no buttons; one that had keeps them
        // hidden.
        if !self.ready.get() || (media.is_none() && !self.added.get()) {
            return Ok(());
        }
        let taskbar = self.taskbar()?;
        let buttons = self.native(hwnd, media.as_ref());
        let window = windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void);
        let result = if self.added.get() {
            taskbar.ThumbBarUpdateButtons(window, &buttons)
        } else {
            taskbar.ThumbBarAddButtons(window, &buttons)
        };
        result.map_err(|e| format!("the taskbar preview's media buttons: {e}"))?;
        self.added.set(true);
        Ok(())
    }

    /// The window's DPI or the theme changed: the icons are drawn again for it.
    ///
    /// # Safety
    ///
    /// `hwnd` is a live window of the calling thread.
    pub unsafe fn restyle(&self, hwnd: isize) -> Result<(), String> {
        if !self.added.get() {
            return Ok(());
        }
        self.update(hwnd)
    }

    unsafe fn taskbar(&self) -> Result<windows::Win32::UI::Shell::ITaskbarList3, String> {
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
        if let Some(taskbar) = self.taskbar.borrow().as_ref() {
            return Ok(taskbar.clone());
        }
        let taskbar: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("the taskbar cannot be reached: {e}"))?;
        taskbar
            .HrInit()
            .map_err(|e| format!("the taskbar cannot be reached: {e}"))?;
        self.taskbar.replace(Some(taskbar.clone()));
        Ok(taskbar)
    }

    /// The buttons as `ITaskbarList3` takes them, with icons drawn for the window's DPI and
    /// the taskbar's theme now.
    unsafe fn native(
        &self,
        hwnd: isize,
        media: Option<&MediaButtons>,
    ) -> [windows::Win32::UI::Shell::THUMBBUTTON; 3] {
        use windows::Win32::UI::Shell::{
            THBF_DISABLED, THBF_ENABLED, THBF_HIDDEN, THB_FLAGS, THB_ICON, THB_TOOLTIP, THUMBBUTTON,
        };
        use windows::Win32::UI::WindowsAndMessaging::HICON;
        use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
        use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
        let dpi = GetDpiForWindow(hwnd as windows_sys::Win32::Foundation::HWND);
        let side = icon_side(if dpi == 0 { 96 } else { dpi });
        let contrast = crate::compositor::fluent::high_contrast().map(|hc| hc.text);
        let color = glyph_color(crate::compositor::fluent::taskbar_is_light(), contrast);
        let stale = match self.icons.borrow().as_ref() {
            Some(icons) => icons.side != side || icons.color != color,
            None => true,
        };
        if stale {
            // The taskbar copied the icons it was given, so the old ones can go now.
            self.icons.replace(Some(Icons::draw(side, color)));
        }
        let texts = Texts::for_language(GetUserDefaultUILanguage());
        let icons = self.icons.borrow();
        let laid = buttons(media, &texts);
        let made = laid.map(|button| {
            let mut flags = if button.enabled {
                THBF_ENABLED
            } else {
                THBF_DISABLED
            };
            if button.hidden {
                flags |= THBF_HIDDEN;
            }
            let icon = icons
                .as_ref()
                .map(|icons| icons.of(button.glyph))
                .unwrap_or(std::ptr::null_mut());
            THUMBBUTTON {
                dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
                iId: button.id,
                iBitmap: 0,
                hIcon: HICON(icon),
                szTip: tip_units(button.tip),
                dwFlags: flags,
            }
        });
        made
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYING: MediaButtons = MediaButtons {
        playing: true,
        previous: true,
        play: true,
        pause: true,
        next: false,
    };

    #[test]
    fn fr24_the_taskbar_preview_has_media_buttons() {
        let texts = Texts::english();
        // Playing: previous, pause and a disabled next, as the session allows.
        let shown = buttons(Some(&PLAYING), &texts);
        let ids: Vec<u32> = shown.iter().map(|b| b.id).collect();
        assert_eq!(ids, [PREVIOUS_ID, PLAY_PAUSE_ID, NEXT_ID]);
        let glyphs: Vec<char> = shown.iter().map(|b| b.glyph).collect();
        assert_eq!(glyphs, [glyph::PREVIOUS, glyph::PAUSE, glyph::NEXT]);
        let enabled: Vec<bool> = shown.iter().map(|b| b.enabled).collect();
        assert_eq!(enabled, [true, true, false]);
        assert!(shown.iter().all(|b| !b.hidden));
        assert_eq!(shown[1].tip, "Pause");
        // Paused: the middle button plays, and its look follows at once.
        let paused = MediaButtons {
            playing: false,
            ..PLAYING
        };
        let shown = buttons(Some(&paused), &texts);
        assert_eq!(shown[1].glyph, glyph::PLAY);
        assert_eq!(shown[1].tip, "Play");
        assert!(shown[1].enabled);
        // No session: the buttons are hidden, and a click on one does nothing.
        assert!(buttons(None, &texts).iter().all(|b| b.hidden && !b.enabled));
        assert_eq!(pressed(PLAY_PAUSE_ID, None), None);
        // A click asks for what the button shows, and only what the session allows.
        assert_eq!(
            pressed(PLAY_PAUSE_ID, Some(&PLAYING)),
            Some(MediaButton::Pause)
        );
        assert_eq!(
            pressed(PLAY_PAUSE_ID, Some(&paused)),
            Some(MediaButton::Play)
        );
        assert_eq!(
            pressed(PREVIOUS_ID, Some(&PLAYING)),
            Some(MediaButton::Previous)
        );
        assert_eq!(pressed(NEXT_ID, Some(&PLAYING)), None);
        assert_eq!(pressed(99, Some(&PLAYING)), None);
    }

    #[test]
    fn fr24_the_preview_buttons_speak_korean_and_english() {
        assert_eq!(Texts::for_language(0x0412), Texts::korean());
        assert_eq!(Texts::for_language(0x0409), Texts::english());
        let shown = buttons(Some(&PLAYING), &Texts::korean());
        assert_eq!(shown[1].tip, "일시 정지");
        let tip = tip_units("이전");
        assert_eq!(&tip[..3], &[0xC774, 0xC804, 0]);
        let long = tip_units(&"x".repeat(400));
        assert_eq!(long[MAX_TIP], 0);
    }

    #[test]
    fn fr24_the_preview_icons_follow_the_dpi_and_the_taskbars_theme() {
        assert_eq!(icon_side(96), 16);
        assert_eq!(icon_side(144), 24);
        assert_eq!(icon_side(192), 32);
        assert_eq!(glyph_color(true, None), 0xE400_0000);
        assert_eq!(glyph_color(false, None), 0xFFFF_FFFF);
        assert_eq!(glyph_color(true, Some(0x00FF_FF00)), 0xFFFF_FF00);
        // Full coverage in white, none, and half coverage in the light taskbar's ink.
        let pixels = glyph_pixels(&[255, 0], 0xFFFF_FFFF);
        assert_eq!(pixels, [255, 255, 255, 255, 0, 0, 0, 0]);
        let half = glyph_pixels(&[128], 0xE400_0000);
        assert_eq!(&half[..3], &[0, 0, 0]);
        assert_eq!(half[3], ((128u32 * 0xE4 + 127) / 255) as u8);
    }

    #[test]
    fn fr24_a_click_on_a_preview_button_is_read_from_its_command() {
        assert_eq!(clicked(0x1800_0002), Some(PLAY_PAUSE_ID));
        assert_eq!(clicked(0x0000_0002), None);
        let changed = set(Some(PLAYING));
        assert!(changed);
        assert!(!set(Some(PLAYING)));
        assert_eq!(current(), Some(PLAYING));
        assert!(set(None));
        assert_eq!(current(), None);
    }

    /// The glyphs are drawn: a Windows with Segoe Fluent Icons or Segoe MDL2 Assets gives
    /// each of them pixels at a small icon's size.
    #[test]
    fn fr24_the_preview_glyphs_are_drawn_at_the_windows_dpi() {
        for side in [icon_side(96), icon_side(192)] {
            let icons = Icons::draw(side, 0xFFFF_FFFF);
            for symbol in Icons::GLYPHS {
                assert!(
                    !icons.of(symbol).is_null(),
                    "no icon for {symbol:?} at {side}"
                );
            }
        }
    }
}

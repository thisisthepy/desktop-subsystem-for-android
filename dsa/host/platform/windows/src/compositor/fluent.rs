//! Windows 11 (Fluent) drawing for the window manager's own surfaces: the widgets' menu and
//! the border and handles of a widget being edited.
//!
//! The colors are WinUI 3's theme resources (`TextFillColorPrimary`,
//! `SubtleFillColorSecondary`, `DividerStrokeColorDefault` and the rest), in the theme the
//! user picked for apps, so the menu looks as a WinUI app's would. Under a high
//! contrast theme they take the theme's system colors instead, as WinUI does.
//!
//! Everything is drawn on a small canvas of premultiplied BGRA pixels ([`Canvas`]), the
//! format a layered window and a DWM surface both take. Shapes are anti-aliased by their
//! distance to the pixel's centre. Text and Segoe Fluent Icons glyphs are rasterized by GDI
//! into coverage masks ([`text`]) and blended in the color they are drawn in.

/// A color as `0xAARRGGBB`, with straight (not premultiplied) alpha.
pub type Argb = u32;

/// The Windows build from which DWM draws a system backdrop (`DWMWA_SYSTEMBACKDROP_TYPE`):
/// Windows 11 22H2.
pub const BACKDROP_BUILD: u32 = 22621;

/// The Windows build from which DWM rounds a window's corners on request
/// (`DWMWA_WINDOW_CORNER_PREFERENCE`): Windows 11.
pub const ROUNDED_BUILD: u32 = 22000;

/// Windows' default accent color, for when the user's cannot be read.
pub const DEFAULT_ACCENT: Argb = 0xFF00_78D4;

/// The Segoe Fluent Icons glyphs the surfaces draw (Segoe MDL2 Assets has them at the same
/// code points).
pub mod glyph {
    /// "CheckMark": the chosen item of a menu.
    pub const CHECK: char = '\u{E73E}';
    /// "ChevronRight": an item that opens a submenu.
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
    /// "FullScreen": arrows out to the four corners, for resizing.
    pub const RESIZE: char = '\u{E740}';
    /// "Unpin": taking something off where it was put.
    pub const UNPIN: char = '\u{E77A}';
    /// "Setting": a gear, for an item's own settings.
    pub const SETTINGS: char = '\u{E713}';
    /// "Play": a triangle pointing right.
    pub const PLAY: char = '\u{E768}';
    /// "Pause": two bars.
    pub const PAUSE: char = '\u{E769}';
    /// "Previous": a bar and a triangle pointing left, to the track before.
    pub const PREVIOUS: char = '\u{E892}';
    /// "Next": a triangle pointing right and a bar, to the track after.
    pub const NEXT: char = '\u{E893}';
    /// "OpenInNewWindow": a window with an arrow out of it, for opening an app.
    pub const OPEN_WINDOW: char = '\u{E8A7}';
    /// "Stop": a square.
    pub const STOP: char = '\u{E71A}';
}

/// The system colors of a high contrast theme that a menu and a button are drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighContrast {
    pub window: Argb,
    pub text: Argb,
    pub highlight: Argb,
    pub highlight_text: Argb,
    pub gray_text: Argb,
}

/// The shades of the user's accent color that WinUI draws accented glyphs in:
/// `AccentLight2` on a dark theme and `AccentDark1` on a light one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentShades {
    pub light2: Argb,
    pub base: Argb,
    pub dark1: Argb,
}

impl AccentShades {
    /// The shades from Explorer's `AccentPalette`: eight colors of four bytes (red, green,
    /// blue, unused), from `AccentLight3` to `AccentDark3`. None when it is too short.
    pub fn from_palette(bytes: &[u8]) -> Option<AccentShades> {
        if bytes.len() < 28 {
            return None;
        }
        let at = |index: usize| {
            let p = &bytes[index * 4..index * 4 + 3];
            0xFF00_0000 | (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2])
        };
        Some(AccentShades {
            light2: at(1),
            base: at(3),
            dark1: at(4),
        })
    }

    /// Shades made from the accent alone, when the palette cannot be read: lighter towards
    /// white and darker towards black, as the palette's are.
    pub fn from_accent(accent: Argb) -> AccentShades {
        AccentShades {
            light2: mix(accent, 0xFFFF_FFFF, 0.55),
            base: accent | 0xFF00_0000,
            dark1: mix(accent, 0xFF00_0000, 0.2),
        }
    }
}

/// `from` moved `amount` (0 to 1) of the way to `to`, opaque.
fn mix(from: Argb, to: Argb, amount: f32) -> Argb {
    let channel = |shift: u32| {
        let a = ((from >> shift) & 0xff) as f32;
        let b = ((to >> shift) & 0xff) as f32;
        ((a + (b - a) * amount).round().clamp(0.0, 255.0) as u32) << shift
    };
    0xFF00_0000 | channel(16) | channel(8) | channel(0)
}

/// What a surface is drawn on behind its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// DWM's transient window backdrop (Desktop Acrylic), which the content is drawn over
    /// with see-through pixels.
    Acrylic,
    /// An opaque fill: Windows 10, transparency effects turned off, or high contrast.
    Solid,
}

/// The parts of the user's settings the Fluent surfaces follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Whether apps use the dark theme (`AppsUseLightTheme` is 0).
    pub dark: bool,
    /// Whether transparency effects are on (`EnableTransparency`).
    pub transparency: bool,
    /// The high contrast theme's colors while one is on.
    pub high_contrast: Option<HighContrast>,
    pub accent: AccentShades,
    /// The Windows build number, 0 when unknown.
    pub build: u32,
}

impl Theme {
    /// The light theme of a Windows 11 22H2 with every effect on, which tests start from.
    pub fn light() -> Theme {
        Theme {
            dark: false,
            transparency: true,
            high_contrast: None,
            accent: AccentShades::from_accent(DEFAULT_ACCENT),
            build: BACKDROP_BUILD,
        }
    }

    /// What a flyout is drawn on: acrylic where DWM draws it and the user wants it, else a
    /// solid fill. A high contrast theme is always solid, as WinUI's is.
    pub fn backdrop(&self) -> Backdrop {
        if self.build >= BACKDROP_BUILD && self.transparency && self.high_contrast.is_none() {
            Backdrop::Acrylic
        } else {
            Backdrop::Solid
        }
    }

    /// Whether DWM rounds the corners of a window that asks.
    pub fn rounded(&self) -> bool {
        self.build >= ROUNDED_BUILD
    }
}

/// The colors a menu is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuPalette {
    /// What the menu is filled with: see-through over acrylic, else the acrylic's fallback.
    pub background: Argb,
    pub text: Argb,
    /// An item's text while the pointer or the keyboard is on it.
    pub text_highlighted: Argb,
    /// An item's text while it is pressed.
    pub text_pressed: Argb,
    pub text_disabled: Argb,
    /// The rounded highlight behind the item the pointer or the keyboard is on.
    pub hover: Argb,
    /// The highlight behind a pressed item.
    pub pressed: Argb,
    pub separator: Argb,
    /// The check mark of the chosen item.
    pub check: Argb,
    /// The chevron of an item that opens a submenu.
    pub chevron: Argb,
}

/// The menu's colors for `theme`: WinUI's `MenuFlyout` theme resources.
pub fn menu_palette(theme: &Theme) -> MenuPalette {
    if let Some(hc) = theme.high_contrast {
        return MenuPalette {
            background: hc.window | 0xFF00_0000,
            text: hc.text,
            text_highlighted: hc.highlight_text,
            text_pressed: hc.highlight_text,
            text_disabled: hc.gray_text,
            hover: hc.highlight,
            pressed: hc.highlight,
            separator: hc.text,
            check: hc.text,
            chevron: hc.text,
        };
    }
    let acrylic = theme.backdrop() == Backdrop::Acrylic;
    if theme.dark {
        MenuPalette {
            // AcrylicBackgroundFillColorDefault's fallback.
            background: if acrylic { 0 } else { 0xFF2C_2C2C },
            text: 0xFFFF_FFFF,
            text_highlighted: 0xFFFF_FFFF,
            text_pressed: 0xC5FF_FFFF,
            text_disabled: 0x5DFF_FFFF,
            hover: 0x0FFF_FFFF,
            pressed: 0x0AFF_FFFF,
            separator: 0x15FF_FFFF,
            check: theme.accent.light2,
            chevron: 0xC5FF_FFFF,
        }
    } else {
        MenuPalette {
            background: if acrylic { 0 } else { 0xFFF9_F9F9 },
            text: 0xE400_0000,
            text_highlighted: 0xE400_0000,
            text_pressed: 0x9E00_0000,
            text_disabled: 0x5C00_0000,
            hover: 0x0900_0000,
            pressed: 0x0600_0000,
            separator: 0x0F00_0000,
            check: theme.accent.dark1,
            chevron: 0x9E00_0000,
        }
    }
}

/// How much of the pixel whose top left corner is (`px`, `py`) a rounded rectangle at (`x`,
/// `y`) of `w` by `h` with corners of `radius` covers, from 0 to 1. The edge is smoothed over
/// one pixel.
pub fn rounded_coverage(px: i32, py: i32, x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    let radius = radius.clamp(0.0, (w.min(h) / 2.0).max(0.0));
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = ((px as f32 + 0.5) - cx).abs() - (w / 2.0 - radius);
    let qy = ((py as f32 + 0.5) - cy).abs() - (h / 2.0 - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let distance = outside + qx.max(qy).min(0.0) - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Premultiplied BGRA pixels, `width` by `height`, top row first.
pub struct Canvas<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a mut [u8],
}

impl Canvas<'_> {
    /// Sets every pixel to `color`.
    pub fn clear(&mut self, color: Argb) {
        let pixel = premultiplied(color, 1.0);
        for out in self.pixels.chunks_exact_mut(4) {
            out.copy_from_slice(&pixel);
        }
    }

    /// Lays `color` over the pixel at `x`, `y`, at `coverage` (0 to 1) of its alpha.
    pub fn blend(&mut self, x: usize, y: usize, color: Argb, coverage: f32) {
        if x >= self.width || y >= self.height || coverage <= 0.0 {
            return;
        }
        let source = premultiplied(color, coverage);
        let keep = 1.0 - f32::from(source[3]) / 255.0;
        let at = (y * self.width + x) * 4;
        for (index, value) in source.iter().enumerate() {
            let under = f32::from(self.pixels[at + index]);
            self.pixels[at + index] =
                (f32::from(*value) + under * keep).round().clamp(0.0, 255.0) as u8;
        }
    }

    /// Fills a rectangle of whole pixels.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Argb) {
        let (left, top) = (x.max(0), y.max(0));
        let right = (x + w).min(self.width as i32);
        let bottom = (y + h).min(self.height as i32);
        for py in top..bottom {
            for px in left..right {
                self.blend(px as usize, py as usize, color, 1.0);
            }
        }
    }

    /// Fills a rounded rectangle, smoothed along its edge.
    pub fn fill_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Argb) {
        let left = (x.floor() as i32).max(0);
        let top = (y.floor() as i32).max(0);
        let right = ((x + w).ceil() as i32).min(self.width as i32);
        let bottom = ((y + h).ceil() as i32).min(self.height as i32);
        for py in top..bottom {
            for px in left..right {
                let coverage = rounded_coverage(px, py, x, y, w, h, radius);
                self.blend(px as usize, py as usize, color, coverage);
            }
        }
    }

    /// Draws the inside edge, `thickness` wide, of a rounded rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rounded(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        thickness: f32,
        color: Argb,
    ) {
        let left = (x.floor() as i32).max(0);
        let top = (y.floor() as i32).max(0);
        let right = ((x + w).ceil() as i32).min(self.width as i32);
        let bottom = ((y + h).ceil() as i32).min(self.height as i32);
        let t = thickness;
        for py in top..bottom {
            for px in left..right {
                let outer = rounded_coverage(px, py, x, y, w, h, radius);
                let inner = rounded_coverage(
                    px,
                    py,
                    x + t,
                    y + t,
                    w - 2.0 * t,
                    h - 2.0 * t,
                    (radius - t).max(0.0),
                );
                self.blend(px as usize, py as usize, color, (outer - inner).max(0.0));
            }
        }
    }

    /// Blends `color` through a coverage mask of `w` by `h` bytes placed at `x`, `y`, at
    /// `opacity` (0 to 1).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_mask(
        &mut self,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
        mask: &[u8],
        color: Argb,
        opacity: f32,
    ) {
        for my in 0..h {
            for mx in 0..w {
                let Some(&cover) = mask.get(my * w + mx) else {
                    return;
                };
                if cover == 0 {
                    continue;
                }
                let (px, py) = (x + mx as i32, y + my as i32);
                if px < 0 || py < 0 {
                    continue;
                }
                let coverage = f32::from(cover) / 255.0 * opacity;
                self.blend(px as usize, py as usize, color, coverage);
            }
        }
    }
}

/// `color` at `coverage` of its alpha, as premultiplied BGRA bytes.
pub fn premultiplied(color: Argb, coverage: f32) -> [u8; 4] {
    let alpha = ((color >> 24) & 0xff) as f32 / 255.0 * coverage.clamp(0.0, 1.0);
    let channel = |shift: u32| (((color >> shift) & 0xff) as f32 * alpha).round() as u8;
    [
        channel(0),
        channel(8),
        channel(16),
        (alpha * 255.0).round() as u8,
    ]
}

/// Reads the user's theme now.
#[cfg(windows)]
pub fn read_theme() -> Theme {
    use super::system_settings::accent_from_dwm;
    const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
    const DWM: &str = r"Software\Microsoft\Windows\DWM";
    let light = registry::user_dword(PERSONALIZE, "AppsUseLightTheme");
    let transparency = registry::user_dword(PERSONALIZE, "EnableTransparency");
    let palette = registry::user_binary(ACCENT, "AccentPalette");
    let dwm = registry::user_dword(DWM, "AccentColor");
    let accent = match palette.as_deref().and_then(AccentShades::from_palette) {
        Some(shades) => shades,
        None => AccentShades::from_accent(dwm.map(accent_from_dwm).unwrap_or(DEFAULT_ACCENT)),
    };
    Theme {
        dark: light == Some(0),
        transparency: transparency != Some(0),
        high_contrast: high_contrast(),
        accent,
        build: crate::android_vm::windows_build()
            .map(|(build, _)| build)
            .unwrap_or(0),
    }
}

/// Whether the taskbar, its previews and its flyouts use the light theme
/// (`SystemUsesLightTheme`), which is set apart from the apps' theme.
#[cfg(windows)]
pub fn taskbar_is_light() -> bool {
    const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    registry::user_dword(PERSONALIZE, "SystemUsesLightTheme") == Some(1)
}

/// The high contrast theme's colors, while one is on.
#[cfg(windows)]
pub fn high_contrast() -> Option<HighContrast> {
    use windows_sys::Win32::Graphics::Gdi::{
        GetSysColor, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
        COLOR_WINDOWTEXT, SYS_COLOR_INDEX,
    };
    use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETHIGHCONTRAST};
    // SAFETY: a structure of the size it says, filled by the call.
    let on = unsafe {
        let mut contrast: HIGHCONTRASTW = std::mem::zeroed();
        contrast.cbSize = std::mem::size_of::<HIGHCONTRASTW>() as u32;
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            &mut contrast as *mut HIGHCONTRASTW as *mut std::ffi::c_void,
            0,
        ) != 0
            && contrast.dwFlags & HCF_HIGHCONTRASTON != 0
    };
    if !on {
        return None;
    }
    let color = |index: SYS_COLOR_INDEX| {
        // SAFETY: a plain read of a system color.
        let bgr = unsafe { GetSysColor(index) };
        0xFF00_0000 | ((bgr & 0xff) << 16) | (bgr & 0xff00) | ((bgr >> 16) & 0xff)
    };
    Some(HighContrast {
        window: color(COLOR_WINDOW),
        text: color(COLOR_WINDOWTEXT),
        highlight: color(COLOR_HIGHLIGHT),
        highlight_text: color(COLOR_HIGHLIGHTTEXT),
        gray_text: color(COLOR_GRAYTEXT),
    })
}

/// The current user's registry values the theme is read from.
#[cfg(windows)]
mod registry {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, REG_ROUTINE_FLAGS, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Reads the value into `out`; how many bytes it has, or None when it is not there.
    fn read(path: &str, name: &str, kind: REG_ROUTINE_FLAGS, out: &mut [u8]) -> Option<usize> {
        let (path, name) = (wide(path), wide(name));
        let mut size = out.len() as u32;
        // SAFETY: NUL-terminated strings and a buffer with its size in bytes, all outliving
        // the call.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                name.as_ptr(),
                kind,
                std::ptr::null_mut(),
                out.as_mut_ptr().cast(),
                &mut size,
            )
        };
        (status == 0).then_some(size as usize)
    }

    pub fn user_dword(path: &str, name: &str) -> Option<u32> {
        let mut value = [0u8; 4];
        read(path, name, RRF_RT_REG_DWORD, &mut value)?;
        Some(u32::from_le_bytes(value))
    }

    pub fn user_binary(path: &str, name: &str) -> Option<Vec<u8>> {
        let mut value = vec![0u8; 64];
        let size = read(path, name, RRF_RT_REG_BINARY, &mut value)?;
        value.truncate(size.min(64));
        Some(value)
    }
}

/// Text and icon glyphs rasterized by GDI into coverage masks: one byte per pixel, 255 where
/// the text covers the pixel fully. GDI writes no alpha, so the text is drawn white on black
/// and each pixel's brightness is its coverage, as the app window's title is.
#[cfg(windows)]
pub mod text {
    use windows_sys::Win32::Foundation::{RECT, SIZE};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject,
        DrawTextW, GdiFlush, GetTextExtentPoint32W, GetTextFaceW, SelectObject, SetBkMode,
        SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DEFAULT_CHARSET,
        DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
        DT_VCENTER, HDC, HFONT, LOGFONTW, TRANSPARENT,
    };

    /// The faces text is written in, the first one installed winning: Segoe UI Variable at
    /// its text size, as WinUI's controls use, then Segoe UI, which Windows 10 has.
    pub const TEXT_FACES: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];

    /// The faces of the icon glyphs: Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets
    /// before it. Both put the same glyphs at the same code points.
    pub const ICON_FACES: [&str; 2] = ["Segoe Fluent Icons", "Segoe MDL2 Assets"];

    /// The faces of a title: Segoe UI Variable at its display size, then Segoe UI.
    pub const TITLE_FACES: [&str; 2] = ["Segoe UI Variable Display", "Segoe UI"];

    /// The weights text is drawn at: WinUI's body text, and its titles' semibold.
    pub const REGULAR: i32 = 400;
    pub const SEMIBOLD: i32 = 600;

    /// Where text sits in its box: at the left, cut with an ellipsis where it runs past the
    /// box, or in the middle, as a glyph is.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Align {
        Left,
        Center,
    }

    /// The first face of `faces` that is installed at `em` pixels, selected into nothing;
    /// the last one when none is. Null when GDI could make none.
    unsafe fn font(dc: HDC, faces: &[&str], em: i32, weight: i32) -> HFONT {
        for (index, face) in faces.iter().enumerate() {
            let mut font: LOGFONTW = std::mem::zeroed();
            // A negative height is the em size, as WinUI gives font sizes.
            font.lfHeight = -em.max(1);
            font.lfWeight = weight;
            font.lfCharSet = DEFAULT_CHARSET as _;
            // Gray antialiasing: the text is drawn as coverage.
            font.lfQuality = ANTIALIASED_QUALITY as _;
            let units = face.encode_utf16().take(31);
            for (slot, unit) in font.lfFaceName.iter_mut().zip(units) {
                *slot = unit;
            }
            let handle = CreateFontIndirectW(&font);
            if handle.is_null() {
                continue;
            }
            if index == faces.len() - 1 {
                return handle;
            }
            // GDI makes a font of another face when the one asked for is not installed.
            let old = SelectObject(dc, handle);
            let mut got = [0u16; 32];
            let length = GetTextFaceW(dc, 32, got.as_mut_ptr()).max(0) as usize;
            SelectObject(dc, old);
            let got = String::from_utf16_lossy(&got[..length.min(got.len())]);
            if got.trim_end_matches('\0').eq_ignore_ascii_case(face) {
                return handle;
            }
            DeleteObject(handle);
        }
        std::ptr::null_mut()
    }

    /// How wide `text` is in the first installed face of `faces` at `em` pixels.
    pub fn measure(text: &str, faces: &[&str], em: i32) -> i32 {
        measure_weighted(text, faces, em, REGULAR)
    }

    /// How wide `text` is in the first installed face of `faces` at `em` pixels and `weight`.
    pub fn measure_weighted(text: &str, faces: &[&str], em: i32, weight: i32) -> i32 {
        if text.is_empty() {
            return 0;
        }
        // SAFETY: a memory DC and a font made, used and freed here; the string outlives the
        // call that reads it.
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return 0;
            }
            let font = font(dc, faces, em, weight);
            let old = (!font.is_null()).then(|| SelectObject(dc, font));
            let units: Vec<u16> = text.encode_utf16().collect();
            let mut size = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(dc, units.as_ptr(), units.len() as i32, &mut size);
            if let Some(old) = old {
                SelectObject(dc, old);
                DeleteObject(font);
            }
            DeleteDC(dc);
            size.cx.max(0)
        }
    }

    /// `text` in the first installed face of `faces` at `em` pixels, in a box of `size`
    /// pixels, vertically centred and placed as `align` says, as a coverage mask of the
    /// box's size. None when there is nothing to draw or GDI cannot draw it.
    pub fn mask(
        text: &str,
        faces: &[&str],
        em: i32,
        size: (usize, usize),
        align: Align,
    ) -> Option<Vec<u8>> {
        mask_weighted(text, faces, em, REGULAR, size, align)
    }

    /// As [`mask`], at `weight` ([`REGULAR`] or [`SEMIBOLD`]).
    pub fn mask_weighted(
        text: &str,
        faces: &[&str],
        em: i32,
        weight: i32,
        size: (usize, usize),
        align: Align,
    ) -> Option<Vec<u8>> {
        if text.trim().is_empty() || size.0 == 0 || size.1 == 0 {
            return None;
        }
        let (width, height) = (size.0 as i32, size.1 as i32);
        // SAFETY: a memory DC, a DIB section and a font made, used and freed here; the DIB's
        // bits are read only while it is alive, and the string outlives the call.
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: the rows come top first.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                return None;
            }
            let old_bitmap = SelectObject(dc, bitmap);
            let font = font(dc, faces, em, weight);
            let old_font = (!font.is_null()).then(|| SelectObject(dc, font));
            SetBkMode(dc, TRANSPARENT as _);
            SetTextColor(dc, 0x00FF_FFFF);
            let mut units: Vec<u16> = text.encode_utf16().collect();
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            let placement = match align {
                Align::Left => DT_LEFT | DT_END_ELLIPSIS,
                Align::Center => DT_CENTER,
            };
            DrawTextW(
                dc,
                units.as_mut_ptr(),
                units.len() as i32,
                &mut rect,
                placement | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            GdiFlush();
            let pixels =
                std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
            let mask = pixels
                .chunks_exact(4)
                .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
                .collect();
            if let Some(old) = old_font {
                SelectObject(dc, old);
                DeleteObject(font);
            }
            SelectObject(dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(dc);
            Some(mask)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr12_the_menu_follows_the_light_and_the_dark_theme() {
        let light = Theme::light();
        let dark = Theme {
            dark: true,
            ..light
        };
        let (light, dark) = (menu_palette(&light), menu_palette(&dark));
        // WinUI's TextFillColorPrimary: near-black on light, white on dark.
        assert_eq!(light.text, 0xE400_0000);
        assert_eq!(dark.text, 0xFFFF_FFFF);
        // SubtleFillColorSecondary behind the item the pointer is on.
        assert_eq!(light.hover, 0x0900_0000);
        assert_eq!(dark.hover, 0x0FFF_FFFF);
        // Over acrylic the menu itself is see-through; DWM draws the backdrop.
        assert_eq!(light.background >> 24, 0);
        assert_eq!(dark.background >> 24, 0);
        // The check mark is in the accent: its dark shade on light, its light one on dark.
        let accent = AccentShades::from_accent(DEFAULT_ACCENT);
        assert_eq!(light.check, accent.dark1);
        assert_eq!(dark.check, accent.light2);
    }

    #[test]
    fn fr12_the_menu_is_acrylic_only_where_windows_draws_it_and_the_user_wants_it() {
        let theme = Theme::light();
        assert_eq!(theme.backdrop(), Backdrop::Acrylic);
        // Windows 11 before 22H2 has no system backdrop, and Windows 10 no rounded corners.
        let early = Theme {
            build: 22000,
            ..theme
        };
        assert_eq!(early.backdrop(), Backdrop::Solid);
        assert!(early.rounded());
        let ten = Theme {
            build: 19045,
            ..theme
        };
        assert_eq!(ten.backdrop(), Backdrop::Solid);
        assert!(!ten.rounded());
        // Transparency effects off: solid, and opaque.
        let off = Theme {
            transparency: false,
            ..theme
        };
        assert_eq!(off.backdrop(), Backdrop::Solid);
        assert_eq!(menu_palette(&off).background, 0xFFF9_F9F9);
        assert_eq!(
            menu_palette(&Theme { dark: true, ..off }).background,
            0xFF2C_2C2C
        );
    }

    #[test]
    fn fr12_a_high_contrast_theme_draws_the_menu_in_its_own_colors() {
        let hc = HighContrast {
            window: 0xFF00_0000,
            text: 0xFFFF_FFFF,
            highlight: 0xFF1A_EBFF,
            highlight_text: 0xFF00_0000,
            gray_text: 0xFF3F_F23F,
        };
        let theme = Theme {
            high_contrast: Some(hc),
            ..Theme::light()
        };
        assert_eq!(theme.backdrop(), Backdrop::Solid);
        let palette = menu_palette(&theme);
        assert_eq!(palette.background, 0xFF00_0000);
        assert_eq!(palette.hover, hc.highlight);
        assert_eq!(palette.text_highlighted, hc.highlight_text);
        assert_eq!(palette.text_disabled, hc.gray_text);
    }

    #[test]
    fn fr12_the_accent_shades_are_read_from_explorers_palette() {
        // Light3, Light2, Light1, Accent, Dark1, Dark2, Dark3, unused: R, G, B, A each.
        let palette: Vec<u8> = [
            [0x99, 0xEB, 0xFF, 0],
            [0x4C, 0xC2, 0xFF, 0],
            [0x00, 0x91, 0xF8, 0],
            [0x00, 0x78, 0xD4, 0],
            [0x00, 0x67, 0xC0, 0],
            [0x00, 0x3E, 0x92, 0],
            [0x00, 0x1A, 0x68, 0],
            [0xF7, 0x63, 0x0C, 0],
        ]
        .concat();
        let shades = AccentShades::from_palette(&palette).unwrap();
        assert_eq!(shades.light2, 0xFF4C_C2FF);
        assert_eq!(shades.base, 0xFF00_78D4);
        assert_eq!(shades.dark1, 0xFF00_67C0);
        assert_eq!(AccentShades::from_palette(&palette[..20]), None);
        let made = AccentShades::from_accent(0xFF00_78D4);
        assert!((made.light2 & 0xff) > 0xd4 && (made.dark1 & 0xff) < 0xd4);
    }

    #[test]
    fn fr12_shapes_are_smoothed_along_their_edges_and_premultiplied() {
        let mut pixels = vec![0u8; 20 * 20 * 4];
        let mut canvas = Canvas {
            width: 20,
            height: 20,
            pixels: &mut pixels,
        };
        canvas.fill_rounded(0.0, 0.0, 20.0, 20.0, 10.0, 0x80FF_FFFF);
        let alpha = |x: usize, y: usize| pixels[(y * 20 + x) * 4 + 3];
        assert_eq!(alpha(10, 10), 0x80);
        assert_eq!(alpha(0, 0), 0, "a circle leaves its corners clear");
        let edge = alpha(0, 10);
        assert!(
            edge > 0 && edge <= 0x80,
            "the edge is partly covered: {edge}"
        );
        assert!(pixels
            .chunks_exact(4)
            .all(|p| p[..3].iter().all(|&c| c <= p[3])));
        assert_eq!(premultiplied(0xFF10_2030, 1.0), [0x30, 0x20, 0x10, 0xFF]);
        assert_eq!(premultiplied(0x0000_0000, 1.0), [0, 0, 0, 0]);
    }
}

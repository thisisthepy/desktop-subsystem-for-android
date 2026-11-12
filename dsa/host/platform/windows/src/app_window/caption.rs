//! The title bar of a task window: transparent, with the app's content running underneath.
//!
//! The window keeps its standard frame, so Windows still gives it its resize borders, its
//! shadow, its rounded corners, its system menu, snapping and the animations of a normal
//! window. Only the caption is taken away: `WM_NCCALCSIZE` makes the client area reach the
//! window's top edge, the app's frames fill it from there, and the window draws its caption
//! buttons over the app's top right corner in a visual of their own
//! ([`WindowPresenter::show_caption`]).
//!
//! The title bar is Android's status bar. It keeps the height Windows gives a caption at the
//! window's DPI. The window reports it (`FromHost::Caption`) when it opens and whenever it
//! changes, and Android gives the task a status bar inset of exactly that height, in the
//! task's pixels, as a phone gives an app the height of its status bar. An app that draws
//! edge to edge keeps its toolbar below the band and draws its own status bar background
//! under it; an app that does not gets the status bar background Android draws in its status
//! bar colour there. Either way only what the app chooses to draw under the band is covered.
//! The guest image also sets Android's status bar height resources to this height at each
//! scale's density, for the apps that read those instead of their insets.
//!
//! The band belongs to Windows, as the platform-neutral [`CaptionLayout`] decides:
//! `WM_NCHITTEST` answers a caption button's code over the buttons (the maximize button's
//! code is what shows the Windows 11 snap layouts), the top resize edge's codes along the
//! top, and `HTCAPTION` everywhere else in the band, so dragging moves the window and a double
//! click maximizes it, both done by Windows; a right click opens the window menu. The
//! buttons themselves are pressed here, on the non-client mouse messages, so Windows never
//! draws its classic buttons over them.
//!
//! The glyphs are dark over a light band and light over a dark one. The presenter reads how
//! light the app's pixels under the band are ([`WindowPresenter::on_band_colour`]), and they
//! decide, since an app drawing edge to edge often leaves its status bar transparent or reports
//! icons for a status bar it does not show; what the app's status bar reports
//! (`ToHost::CaptionTone`) counts only until a frame has been read. Under a high contrast theme
//! the buttons take the theme's colors.
//!
//! At the left of the band the window draws the app's icon (the program's icon, which the
//! taskbar shows too, read at the small icon size of the window's DPI so it stays sharp) and
//! the app's name (the window's title, in Segoe UI Variable at the caption text size, cut
//! with an ellipsis before the buttons), both in the glyphs' color and fainter while the
//! window is inactive. The icon is `HTSYSMENU`, the code of a standard caption icon; a click
//! on it opens the window menu, the Windows 11 menu that replaces the classic system menu, and
//! a double click closes the window, as Windows' own icon does ([`super::window_menu`]).
//!
//! An icon drawn without its plate can vanish into a band of its own colour, as a white logo
//! on a white title bar. The window measures the logo's contrast against the band each time
//! it draws, and when too little of it stands out it draws the whole icon with the app's
//! background, or the logo on a neutral plate ([`caption_icon::choose`]).
//!
//! An app that draws nothing behind the band (an old theme draws no system bar background)
//! would leave it black. The presenter samples the band and the row under it from the app's
//! frames ([`WindowPresenter::on_band_colour`]), and the band is then filled with the app's
//! top color, with glyphs in the tone that color calls for.
//!
//! The icon is the one the title bar of an app's program carries: an adaptive icon's
//! foreground layer alone (`CAPTION_ICON_ID`), so no square plate stands over the app, else
//! the program's own icon. The foreground alone keeps the margin the plate had, so its logo
//! is cropped to what it draws and fitted to the box a whole icon fills; the program's own
//! icon, which the taskbar and Alt+Tab show too, has its plate and is drawn as it is. While the task can go back (`ToHost::CaptionBack`), the icon's cell
//! shows a back arrow instead, a button of the caption buttons' look whose click sends the task
//! Back; its hit test code is `HTHELP`, which a window without a help button never otherwise
//! answers, so Windows leaves the press to this window. After a Back the arrow waits for the
//! task's state to be told again ([`BACK_SETTLE_MS`] at most) before it sends another, so a
//! quick second click never sends Back past the first screen, which would close the app.
//!
//! An app that draws its own title bar (`ToHost::CaptionMode`) fills the band with its own
//! content. The window then draws only the icon's cell and the buttons over it, and the points
//! of the band on the app's controls are `HTCLIENT`, so they reach the app as any point of the
//! client area does; the rest still drags the window.

use std::cell::{Cell, RefCell};

use std::sync::mpsc::Sender;

use dsa_host_core::compositor::caption::{
    paint_title_bar, scaled, shown_tone, title_bar_hit, BackButton, BandSample, CaptionBand,
    CaptionButton, CaptionHit, CaptionLayout, CaptionPalette, CaptionState, CaptionTone,
    TitleBarContent, TitleBarHit, TitleBarMode, TITLE_TEXT_AT_100,
};
use dsa_host_core::compositor::caption_icon::{self, IconLook};
use dsa_host_core::compositor::window_hosts::FromHost;
use dsa_host_core::input::InputEvent;
use dsa_host_core::keys::{keycode, KeyAction, KeyEvent as GuestKey};
use dsa_host_core::tasks::Bounds;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, DrawTextW,
    GdiFlush, GetSysColor, GetTextFaceW, ScreenToClient, SelectObject, SetBkMode, SetTextColor,
    ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, COLOR_BTNFACE, COLOR_BTNTEXT,
    COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HDC, HFONT, LOGFONTW,
    SYS_COLOR_INDEX, TRANSPARENT,
};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, GetMessageTime, IsZoomed, PostMessageW, SystemParametersInfoW,
    HTCAPTION, HTCLIENT, HTCLOSE, HTHELP, HTMAXBUTTON, HTMINBUTTON, HTSYSMENU, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, NCCALCSIZE_PARAMS, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE,
    SPI_GETHIGHCONTRAST, WINDOW_EX_STYLE, WINDOW_STYLE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCHITTEST,
    WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    WM_SYSCOMMAND,
};

use crate::compositor::present::WindowPresenter;

/// The borders Windows gives a window of `style` at `dpi`: how far its frame reaches past
/// its client area on the left, at the top (the caption with the top border), on the right
/// and at the bottom.
pub fn standard_frame(
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
    dpi: u32,
) -> (i32, i32, i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the rectangle is a local the call only writes.
    unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, ex_style, dpi) };
    (-rect.left, -rect.top, rect.right, rect.bottom)
}

/// The height of the title bar at `dpi`: the caption and the top border of a standard
/// window, which is where a standard window's client area starts.
pub fn title_bar_height(style: WINDOW_STYLE, ex_style: WINDOW_EX_STYLE, dpi: u32) -> u32 {
    standard_frame(style, ex_style, dpi).1.max(0) as u32
}

/// The frame of a task window, whose client area reaches its top edge: the standard
/// window's borders without its caption. A maximized window keeps its top border too, which
/// Windows places off the screen, as it does the other three.
pub fn task_frame(
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
    dpi: u32,
    maximized: bool,
) -> (i32, i32, i32, i32) {
    let (left, _, right, bottom) = standard_frame(style, ex_style, dpi);
    (left, if maximized { bottom } else { 0 }, right, bottom)
}

/// Whether a high contrast theme is on.
unsafe fn high_contrast() -> bool {
    let mut contrast: HIGHCONTRASTW = std::mem::zeroed();
    contrast.cbSize = std::mem::size_of::<HIGHCONTRASTW>() as u32;
    SystemParametersInfoW(
        SPI_GETHIGHCONTRAST,
        contrast.cbSize,
        &mut contrast as *mut HIGHCONTRASTW as *mut std::ffi::c_void,
        0,
    ) != 0
        && contrast.dwFlags & HCF_HIGHCONTRASTON != 0
}

/// A system color as 0xAARRGGBB, opaque.
unsafe fn system_color(index: SYS_COLOR_INDEX) -> u32 {
    let bgr = GetSysColor(index);
    0xFF00_0000 | ((bgr & 0xff) << 16) | (bgr & 0xff00) | ((bgr >> 16) & 0xff)
}

/// The colors the buttons are drawn in: the theme's under high contrast, otherwise those of
/// Windows' own caption buttons for glyphs of `tone`.
unsafe fn palette(tone: CaptionTone) -> CaptionPalette {
    if !high_contrast() {
        return CaptionPalette::of(tone);
    }
    let highlight = system_color(COLOR_HIGHLIGHT);
    let highlight_text = system_color(COLOR_HIGHLIGHTTEXT);
    CaptionPalette {
        rest: system_color(COLOR_BTNFACE),
        glyph: system_color(COLOR_BTNTEXT),
        glyph_highlighted: highlight_text,
        glyph_inactive: system_color(COLOR_GRAYTEXT),
        hover: highlight,
        pressed: highlight,
        close_hover: highlight,
        close_pressed: highlight,
        close_glyph: highlight_text,
    }
}

/// The button a hit test code names.
fn button_of(code: u32) -> Option<CaptionButton> {
    match code {
        HTMINBUTTON => Some(CaptionButton::Minimize),
        HTMAXBUTTON => Some(CaptionButton::Maximize),
        HTCLOSE => Some(CaptionButton::Close),
        _ => None,
    }
}

/// The hit test code of what a point belongs to.
fn code_of(hit: CaptionHit) -> u32 {
    match hit {
        CaptionHit::Client => HTCLIENT,
        CaptionHit::Caption => HTCAPTION,
        CaptionHit::Button(CaptionButton::Minimize) => HTMINBUTTON,
        CaptionHit::Button(CaptionButton::Maximize) => HTMAXBUTTON,
        CaptionHit::Button(CaptionButton::Close) => HTCLOSE,
        CaptionHit::Top => HTTOP,
        CaptionHit::TopLeft => HTTOPLEFT,
        CaptionHit::TopRight => HTTOPRIGHT,
    }
}

/// The hit test code of the back arrow in the icon's cell. A window without a help button
/// never answers it otherwise, and this window presses it itself, as it does its buttons.
pub const HT_BACK: u32 = HTHELP;

/// The hit test code of the client point (`x`, `y`) of a window laid out as `layout`, its
/// title bar showing `mode`. The app's icon is `HTSYSMENU`, so a press on it opens the system
/// menu as a caption icon's does; the back arrow in its place is [`HT_BACK`]; an app's control
/// in a band it draws itself is `HTCLIENT`; the rest is as the layout says.
fn hit_code(layout: &CaptionLayout, x: i32, y: i32, mode: &TitleBarMode) -> u32 {
    match title_bar_hit(layout, x, y, mode) {
        TitleBarHit::SystemMenu => HTSYSMENU,
        TitleBarHit::Back => HT_BACK,
        TitleBarHit::Window(hit) => code_of(hit),
    }
}

/// The key events a click on the back arrow sends the task: Back pressed and released, at
/// `time_ms` of the host's clock.
pub fn back_keys(task_id: i32, time_ms: u32) -> [GuestKey; 2] {
    let key = |action| GuestKey {
        task_id,
        action,
        key_code: keycode::BACK,
        repeat_count: 0,
        meta_state: 0,
        event_time_ms: time_ms,
        canceled: false,
    };
    [key(KeyAction::Down), key(KeyAction::Up)]
}

/// How long after a Back the arrow sends no other, unless the task's state is told again
/// first, in milliseconds. Android tells the new state once the screen has gone, which takes
/// about as long as its transition; until then the arrow's state is stale, and a second Back
/// on what is by then the first screen would close the app.
pub const BACK_SETTLE_MS: u32 = 700;

/// The faces the app's name is written in, the first one installed winning: Segoe UI
/// Variable at its optical size for caption text, then at its text size, then Segoe UI,
/// which Windows before 11 has.
const TITLE_FACES: [&str; 3] = [
    "Segoe UI Variable Small",
    "Segoe UI Variable Text",
    "Segoe UI",
];

/// The caption font at `dpi`, selected into nothing. Null when none could be made.
unsafe fn caption_font(dc: HDC, dpi: u32) -> HFONT {
    for (index, face) in TITLE_FACES.iter().enumerate() {
        let mut font: LOGFONTW = std::mem::zeroed();
        // A negative height is the characters' height, as a caption's text size is given.
        font.lfHeight = -(scaled(TITLE_TEXT_AT_100, dpi) as i32);
        font.lfWeight = 400;
        font.lfCharSet = DEFAULT_CHARSET as _;
        // Gray antialiasing: the text is drawn as coverage, on a transparent band.
        font.lfQuality = ANTIALIASED_QUALITY as _;
        let units = face.encode_utf16().take(31);
        for (slot, unit) in font.lfFaceName.iter_mut().zip(units) {
            *slot = unit;
        }
        let handle = CreateFontIndirectW(&font);
        if handle.is_null() {
            continue;
        }
        if index == TITLE_FACES.len() - 1 {
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

/// `title` laid out in the caption font at `dpi` in a box of `size` pixels, vertically
/// centred and cut with an ellipsis where it would run past the box, as one coverage byte
/// per pixel. `None` when there is nothing to draw or GDI cannot draw it.
unsafe fn title_mask(title: &str, size: (u32, u32), dpi: u32) -> Option<Vec<u8>> {
    if title.trim().is_empty() || size.0 == 0 || size.1 == 0 {
        return None;
    }
    let (width, height) = (size.0 as i32, size.1 as i32);
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
    let font = caption_font(dc, dpi);
    let old_font = if font.is_null() {
        None
    } else {
        Some(SelectObject(dc, font))
    };
    SetBkMode(dc, TRANSPARENT as _);
    // White on the bitmap's black: each pixel's brightness is the text's coverage.
    SetTextColor(dc, 0x00FF_FFFF);
    let mut text: Vec<u16> = title.encode_utf16().collect();
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    DrawTextW(
        dc,
        text.as_mut_ptr(),
        text.len() as i32,
        &mut rect,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    GdiFlush();
    let pixels = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
    let mask = pixels
        .chunks(4)
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

/// What a title bar was drawn from, so a message that changes nothing draws nothing: the
/// layout, the buttons' state, the colors, the DPI, the name, the fill, whether the app draws
/// its own title bar, the back arrow, and how the icon is drawn.
type Drawn = (
    CaptionLayout,
    CaptionState,
    CaptionPalette,
    u32,
    String,
    Option<u32>,
    bool,
    Option<BackButton>,
    Option<IconLook>,
);

/// The title bar's icon at one side, read once for each side: the adaptive icon's logo alone
/// (`None` for an app without one), and the whole icon with its background.
#[derive(Default)]
struct IconPixels {
    side: u32,
    logo: Option<Vec<u8>>,
    whole: Option<Vec<u8>>,
}

/// A task window's title bar: the tone and state of its buttons, the app's icon and name,
/// and the height it last reported.
/// What the title was laid out for: the text, the box it fits in and the DPI.
type TitleKey = (String, (u32, u32), u32);

pub struct CaptionWindow {
    task_id: i32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
    /// The program's icon resource: the app's, in an app's copy of the window host.
    icon_id: usize,
    /// The app's name: the window's title.
    title: RefCell<String>,
    /// What was last read from the app's frame under the band: the color the band is filled
    /// with while the app leaves it empty, and how light the band is.
    sample: Cell<BandSample>,
    /// The tone the glyphs, the name and the frame are drawn in now ([`shown_tone`]).
    shown: Cell<CaptionTone>,
    /// The icon as last read, at the side it was read at.
    icon: RefCell<Option<IconPixels>>,
    /// How the icon was last drawn: bare, or on a plate over a band it would vanish into.
    icon_look: Cell<Option<IconLook>>,
    /// The name as last laid out, and what it was laid out for: text, box and DPI.
    text: RefCell<Option<(TitleKey, Option<Vec<u8>>)>>,
    tone: Cell<CaptionTone>,
    hovered: Cell<Option<CaptionButton>>,
    pressed: Cell<Option<CaptionButton>>,
    active: Cell<bool>,
    /// The band last reported; all 0 before the first report.
    reported: Cell<CaptionBand>,
    /// Whether the app draws its own title bar, and where its controls are in the band.
    app_drawn: Cell<bool>,
    interactive: RefCell<Vec<Bounds>>,
    /// Whether the icon's cell shows the back arrow, and whether the pointer is over it or
    /// presses it.
    back: Cell<bool>,
    back_hovered: Cell<bool>,
    back_pressed: Cell<bool>,
    /// When (the message clock, in milliseconds) the arrow last sent Back, until the task's
    /// state is told again.
    back_sent: Cell<Option<u32>>,
    /// Whether the window menu is open over this window, which keeps the title bar drawn as
    /// active while the menu has the foreground.
    menu_open: Cell<bool>,
    /// Where the back arrow's Back goes: the window host's input for the guest.
    input: Sender<InputEvent>,
    /// Whether Windows will say when the pointer leaves the non-client area.
    tracking: Cell<bool>,
    /// What was last drawn, so a message that changes nothing draws nothing.
    drawn: RefCell<Option<Drawn>>,
}

impl CaptionWindow {
    pub fn new(
        task_id: i32,
        style: WINDOW_STYLE,
        ex_style: WINDOW_EX_STYLE,
        icon_id: usize,
        input: Sender<InputEvent>,
    ) -> CaptionWindow {
        CaptionWindow {
            task_id,
            style,
            ex_style,
            icon_id,
            title: RefCell::new(String::new()),
            sample: Cell::new(BandSample::default()),
            shown: Cell::new(CaptionTone::default()),
            icon: RefCell::new(None),
            icon_look: Cell::new(None),
            text: RefCell::new(None),
            tone: Cell::new(CaptionTone::default()),
            hovered: Cell::new(None),
            pressed: Cell::new(None),
            active: Cell::new(true),
            reported: Cell::new(CaptionBand::default()),
            app_drawn: Cell::new(false),
            interactive: RefCell::new(Vec::new()),
            back: Cell::new(false),
            back_hovered: Cell::new(false),
            back_pressed: Cell::new(false),
            back_sent: Cell::new(None),
            menu_open: Cell::new(false),
            input,
            tracking: Cell::new(false),
            drawn: RefCell::new(None),
        }
    }

    /// Whether the app draws its own title bar, and where its controls are in the band, in
    /// client pixels.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn set_mode(
        &self,
        hwnd: HWND,
        app_drawn: bool,
        interactive: Vec<Bounds>,
        presenter: Option<&WindowPresenter>,
    ) {
        self.app_drawn.set(app_drawn);
        *self.interactive.borrow_mut() = interactive;
        self.retone(hwnd);
        self.paint(hwnd, presenter);
    }

    /// Whether the icon's cell shows the back arrow: the task has somewhere to go back to.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn set_back(&self, hwnd: HWND, back: bool, presenter: Option<&WindowPresenter>) {
        if self.back.replace(back) != back {
            self.back_hovered.set(false);
            self.back_pressed.set(false);
        }
        // The task's state is fresh again: the arrow may send the next Back.
        self.back_sent.set(None);
        self.paint(hwnd, presenter);
    }

    /// Whether the icon's cell shows the back arrow now, for the window's accessibility.
    pub fn shows_back(&self) -> bool {
        self.back.get()
    }

    /// The pointer pressed the back arrow.
    pub fn press_back(&self) {
        self.back_pressed.set(true);
    }

    /// The pointer let go of the back arrow at `time_ms` of the message clock: the task is sent
    /// Back when the arrow was pressed, the task can go back, and its state has been told since
    /// the last Back (or that Back is [`BACK_SETTLE_MS`] old). Returns whether Back was sent.
    pub fn release_back(&self, time_ms: u32) -> bool {
        let pressed = self.back_pressed.replace(false);
        if !pressed || !self.back.get() {
            return false;
        }
        if let Some(sent) = self.back_sent.get() {
            if time_ms.wrapping_sub(sent) < BACK_SETTLE_MS {
                return false;
            }
        }
        self.back_sent.set(Some(time_ms));
        for key in back_keys(self.task_id, time_ms) {
            // The sender ends only when the window host stops; Back then has nowhere to go.
            let _ = self.input.send(InputEvent::Key(key));
        }
        true
    }

    /// The window menu opened over this window, or closed. While it is open the title bar
    /// stays drawn as active, as it does under Windows' own system menu.
    pub fn set_menu_open(&self, open: bool) {
        self.menu_open.set(open);
    }

    pub fn menu_open(&self) -> bool {
        self.menu_open.get()
    }

    /// What the title bar shows besides its layout, for its hit test.
    fn mode<'a>(&self, interactive: &'a [Bounds]) -> TitleBarMode<'a> {
        TitleBarMode {
            app_drawn: self.app_drawn.get(),
            interactive,
            back: self.back.get(),
        }
    }

    /// The back arrow as the icon's cell draws it now; `None` while it shows the icon.
    fn back_button(&self) -> Option<BackButton> {
        self.back.get().then(|| BackButton {
            hovered: self.back_hovered.get(),
            pressed: self.back_pressed.get(),
        })
    }

    /// Where the title bar and its buttons are in `hwnd` now.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn layout(&self, hwnd: HWND) -> CaptionLayout {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let maximized = IsZoomed(hwnd) != 0;
        let border = if maximized {
            0
        } else {
            standard_frame(self.style, self.ex_style, dpi).3.max(0) as u32
        };
        CaptionLayout::new(
            (client.right - client.left).max(0) as u32,
            title_bar_height(self.style, self.ex_style, dpi),
            dpi,
            border,
        )
    }

    /// Lets DWM draw the window's shadow and border around a client area that reaches the
    /// window's top edge. Called once the window exists.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn extend_frame(&self, hwnd: HWND) {
        let margins = MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: 1,
            cyBottomHeight: 0,
        };
        DwmExtendFrameIntoClientArea(hwnd, &margins);
        self.apply_tone(hwnd);
    }

    /// The app's status bar calls for `tone`. The buttons take it, and the window's frame the
    /// matching dark or light mode, unless the pixels under the band say otherwise.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn set_tone(
        &self,
        hwnd: HWND,
        tone: CaptionTone,
        presenter: Option<&WindowPresenter>,
    ) {
        self.tone.set(tone);
        self.retone(hwnd);
        self.paint(hwnd, presenter);
    }

    /// What the presenter last read from the app's frame under the band: the band takes the
    /// fill (0xAARRGGBB), the app's top color, while the app leaves the band empty, or stays
    /// transparent without one; the glyphs take the tone the fill, else the band's lightness,
    /// calls for.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn set_sample(
        &self,
        hwnd: HWND,
        sample: BandSample,
        presenter: Option<&WindowPresenter>,
    ) {
        self.sample.set(sample);
        self.retone(hwnd);
        self.paint(hwnd, presenter);
    }

    /// The app's name, which the title bar shows: the window's title.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn set_title(&self, hwnd: HWND, title: &str, presenter: Option<&WindowPresenter>) {
        *self.title.borrow_mut() = title.to_owned();
        self.paint(hwnd, presenter);
    }

    /// The color the band is filled with: the app's top color while it leaves the band
    /// empty. An app that draws its own title bar is never filled over.
    fn fill(&self) -> Option<u32> {
        self.sample.get().fill.filter(|_| !self.app_drawn.get())
    }

    /// Settles the tone the glyphs, the name and the frame are drawn in, from the app's
    /// status bar and the pixels under the band ([`shown_tone`]), and gives the frame its
    /// mode when the tone changed.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    unsafe fn retone(&self, hwnd: HWND) {
        let now = shown_tone(
            self.tone.get(),
            self.sample.get(),
            self.app_drawn.get(),
            self.shown.get(),
        );
        if self.shown.replace(now) != now {
            self.apply_tone(hwnd);
        }
    }

    /// The title bar's icon `side` pixels square, as it is drawn over the band now, and how:
    /// the adaptive icon's logo alone (enlarged to the size of a whole icon) while it stands
    /// out against the band, else the whole icon or the logo on a plate
    /// ([`caption_icon::choose`]). The icons are read once for each side.
    fn icon_pixels(&self, side: u32, palette: &CaptionPalette) -> Option<(IconLook, Vec<u8>)> {
        let mut cache = self.icon.borrow_mut();
        if cache.as_ref().map(|icon| icon.side) != Some(side) {
            *cache = Some(self.read_icon(side));
        }
        let icon = cache.as_ref()?;
        // Under high contrast the band is the theme's color.
        let background = if palette.rest >> 24 != 0 && !self.app_drawn.get() {
            let fill = BandSample {
                fill: Some(palette.rest),
                luminance: None,
            };
            caption_icon::background_luminance(fill, false, self.shown.get())
        } else {
            caption_icon::background_luminance(
                self.sample.get(),
                self.app_drawn.get(),
                self.shown.get(),
            )
        };
        let chosen = caption_icon::choose(
            icon.logo.as_deref(),
            icon.whole.as_deref(),
            side,
            background,
            self.icon_look.get(),
        );
        self.icon_look.set(chosen.as_ref().map(|(look, _)| *look));
        chosen
    }

    /// The app's logo and whole icon `side` pixels square: the ones the window manager sent
    /// since the program started, else the adaptive icon's foreground the program carries and
    /// the program's own icon.
    fn read_icon(&self, side: u32) -> IconPixels {
        if let Some(live) = super::live_icon::current() {
            let whole = super::live_icon::premultiplied(&live.icon_png, side);
            let logo = (!live.caption_icon_png.is_empty())
                .then(|| super::live_icon::caption_logo(&live.caption_icon_png, side))
                .flatten();
            if logo.is_some() || whole.is_some() {
                return IconPixels { side, logo, whole };
            }
        }
        IconPixels {
            side,
            logo: super::splash::caption_icon_pixels(side),
            whole: super::splash::icon_pixels(self.icon_id, side)
                .filter(|(_, size)| *size == (side, side))
                .map(|(pixels, _)| pixels),
        }
    }

    /// The app's name laid out for a box of `size` at `dpi`, laid out again only when one of
    /// them changes.
    unsafe fn title_pixels(&self, size: (u32, u32), dpi: u32) -> Option<Vec<u8>> {
        let key = (self.title.borrow().clone(), size, dpi);
        let mut cache = self.text.borrow_mut();
        if cache.as_ref().map(|(at, _)| at) != Some(&key) {
            let mask = title_mask(&key.0, size, dpi);
            *cache = Some((key, mask));
        }
        cache.as_ref().and_then(|(_, mask)| mask.clone())
    }

    unsafe fn apply_tone(&self, hwnd: HWND) {
        let dark: i32 = i32::from(self.shown.get() == CaptionTone::LightGlyphs);
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }

    /// Reports the title bar's band (its height, and how far the icon's cell and the buttons
    /// reach in) when it differs from the one last reported.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn report_height(&self, hwnd: HWND, report: &dyn Fn(FromHost)) {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let height = title_bar_height(self.style, self.ex_style, dpi);
        let band = CaptionBand {
            height,
            ..CaptionLayout::new(0, height, dpi, 0).band()
        };
        if self.reported.replace(band) != band {
            report(FromHost::Caption {
                task_id: self.task_id,
                band,
            });
        }
    }

    /// Draws the title bar as it is now, unless that is what was drawn last.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn paint(&self, hwnd: HWND, presenter: Option<&WindowPresenter>) {
        let Some(presenter) = presenter else {
            return;
        };
        let layout = self.layout(hwnd);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let state = CaptionState {
            hovered: self.hovered.get(),
            pressed: self.pressed.get(),
            maximized: IsZoomed(hwnd) != 0,
            active: self.active.get(),
        };
        let palette = palette(self.shown.get());
        // The icon is chosen again each time the band may have changed under it.
        let icon = self.icon_pixels(layout.icon, &palette);
        let now = Some((
            layout,
            state,
            palette,
            dpi,
            self.title.borrow().clone(),
            self.fill(),
            self.app_drawn.get(),
            self.back_button(),
            icon.as_ref().map(|(look, _)| *look),
        ));
        if *self.drawn.borrow() == now {
            return;
        }
        let title = layout
            .title_rect()
            .and_then(|(_, _, width, height)| self.title_pixels((width, height), dpi));
        let content = TitleBarContent {
            fill: self.fill(),
            icon: icon.as_ref().map(|(_, pixels)| pixels.as_slice()),
            title: title.as_deref(),
            app_drawn: self.app_drawn.get(),
            back: self.back_button(),
        };
        let pixels = paint_title_bar(&layout, dpi, &palette, &state, &content);
        if presenter
            .show_caption(0, (layout.width, layout.height), &pixels)
            .is_ok()
        {
            *self.drawn.borrow_mut() = now;
        }
    }

    /// Draws the title bar again even if nothing this window knows of changed, as after a
    /// change of theme or of the system's text settings.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn repaint(&self, hwnd: HWND, presenter: Option<&WindowPresenter>) {
        self.drawn.replace(None);
        self.text.replace(None);
        self.icon.replace(None);
        self.paint(hwnd, presenter);
    }

    /// The window became the active one, or stopped being it.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread.
    pub unsafe fn activated(&self, hwnd: HWND, active: bool, presenter: Option<&WindowPresenter>) {
        self.active.set(active);
        self.paint(hwnd, presenter);
    }

    fn hover(&self, button: Option<CaptionButton>) -> bool {
        self.hovered.replace(button) != button
    }

    /// Handles the window's non-client messages: its frame, its hit test and its buttons.
    /// `None` leaves the message to the window's other handlers.
    ///
    /// # Safety
    ///
    /// `hwnd` is this window, a live window of the calling thread, and `wparam` and
    /// `lparam` are what Windows sent with `message`.
    pub unsafe fn handle(
        &self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        presenter: Option<&WindowPresenter>,
    ) -> Option<LRESULT> {
        match message {
            WM_NCCALCSIZE => {
                // The first rectangle is the window's; Windows makes it the client area. Its
                // top is put back, so the client area reaches the window's top edge, except
                // in a maximized window, whose top border is off the screen.
                let rect: *mut RECT = if wparam != 0 {
                    std::ptr::addr_of_mut!((*(lparam as *mut NCCALCSIZE_PARAMS)).rgrc[0])
                } else {
                    lparam as *mut RECT
                };
                let top = (*rect).top;
                let result = DefWindowProcW(hwnd, message, wparam, lparam);
                let dpi = GetDpiForWindow(hwnd).max(96);
                let maximized = IsZoomed(hwnd) != 0;
                (*rect).top = top + task_frame(self.style, self.ex_style, dpi, maximized).1;
                Some(result)
            }
            WM_NCHITTEST => {
                // The side and bottom borders are Windows' own; the client area holds the
                // title bar.
                let standard = DefWindowProcW(hwnd, message, wparam, lparam);
                if standard as u32 != HTCLIENT {
                    return Some(standard);
                }
                let mut point = POINT {
                    x: (lparam & 0xffff) as i16 as i32,
                    y: ((lparam >> 16) & 0xffff) as i16 as i32,
                };
                ScreenToClient(hwnd, &mut point);
                let interactive = self.interactive.borrow();
                let mode = self.mode(&interactive);
                Some(hit_code(&self.layout(hwnd), point.x, point.y, &mode) as LRESULT)
            }
            WM_NCMOUSEMOVE => {
                if !self.tracking.get() {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    if TrackMouseEvent(&mut track) != 0 {
                        self.tracking.set(true);
                    }
                }
                let button = button_of(wparam as u32);
                let on_back = wparam as u32 == HT_BACK;
                let back_changed = self.back_hovered.replace(on_back) != on_back;
                if self.hover(button) | back_changed {
                    self.paint(hwnd, presenter);
                }
                // Over a button, Windows would draw its own; elsewhere it does the rest.
                match button {
                    Some(_) => Some(0),
                    None if on_back => Some(0),
                    None => Some(DefWindowProcW(hwnd, message, wparam, lparam)),
                }
            }
            WM_NCMOUSELEAVE => {
                self.tracking.set(false);
                let changed = self.hover(None)
                    | self.pressed.replace(None).is_some()
                    | self.back_hovered.replace(false)
                    | self.back_pressed.replace(false);
                if changed {
                    self.paint(hwnd, presenter);
                }
                Some(DefWindowProcW(hwnd, message, wparam, lparam))
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam as u32 == HT_BACK => {
                self.press_back();
                self.paint(hwnd, presenter);
                Some(0)
            }
            WM_NCLBUTTONUP if wparam as u32 == HT_BACK => {
                self.release_back(GetMessageTime() as u32);
                self.paint(hwnd, presenter);
                Some(0)
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => match button_of(wparam as u32) {
                Some(button) => {
                    self.pressed.set(Some(button));
                    self.paint(hwnd, presenter);
                    Some(0)
                }
                None => None,
            },
            WM_NCLBUTTONUP => match button_of(wparam as u32) {
                Some(button) => {
                    let pressed = self.pressed.replace(None);
                    self.paint(hwnd, presenter);
                    if pressed == Some(button) {
                        let command = match button {
                            CaptionButton::Minimize => SC_MINIMIZE,
                            CaptionButton::Maximize if IsZoomed(hwnd) != 0 => SC_RESTORE,
                            CaptionButton::Maximize => SC_MAXIMIZE,
                            CaptionButton::Close => SC_CLOSE,
                        };
                        // Posted, as a click on Windows' own button does, so the window
                        // finishes this message before it changes.
                        PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
                    }
                    Some(0)
                }
                None => {
                    if self.pressed.replace(None).is_some() | self.back_pressed.replace(false) {
                        self.paint(hwnd, presenter);
                    }
                    None
                }
            },
            WM_NCACTIVATE => {
                // Windows would paint its caption; there is none. -1 keeps it from painting
                // the frame, which DWM draws. The window menu taking the foreground leaves the
                // title bar active, as Windows' own menu does.
                let active = wparam != 0 || self.menu_open.get();
                self.activated(hwnd, active, presenter);
                Some(DefWindowProcW(hwnd, message, WPARAM::from(active), -1))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::WS_OVERLAPPEDWINDOW;

    #[test]
    fn fr22_a_hit_and_its_code_name_the_same_part_of_the_window() {
        for button in CaptionButton::ALL {
            assert_eq!(button_of(code_of(CaptionHit::Button(button))), Some(button));
        }
        assert_eq!(code_of(CaptionHit::Caption), HTCAPTION);
        assert_eq!(code_of(CaptionHit::Client), HTCLIENT);
        assert_eq!(button_of(HTCAPTION), None);
    }

    #[test]
    fn fr22_the_icon_opens_the_system_menu() {
        for dpi in [96, 144, 192] {
            let layout = CaptionLayout::new(scaled(800, dpi), scaled(31, dpi), dpi, 0);
            let (left, top, side, _) = layout.icon_rect().unwrap();
            let middle = (left + side as i32 / 2, top + side as i32 / 2);
            let icon = TitleBarMode::default();
            assert_eq!(
                hit_code(&layout, middle.0, middle.1, &icon),
                HTSYSMENU,
                "at {dpi}"
            );
            // The name and the rest of the band still drag the window, and the buttons
            // keep their codes, the maximize one showing the snap layouts.
            let (title_left, _, _, _) = layout.title_rect().unwrap();
            assert_eq!(
                hit_code(&layout, title_left + 4, middle.1, &icon),
                HTCAPTION
            );
            let (max_left, _, width, _) = layout.button_rect(CaptionButton::Maximize);
            let over_max = max_left + width as i32 / 2;
            assert_eq!(hit_code(&layout, over_max, middle.1, &icon), HTMAXBUTTON);
            assert_eq!(
                hit_code(&layout, middle.0, layout.height as i32, &icon),
                HTCLIENT
            );
        }
    }

    #[test]
    fn fr22_the_icon_becomes_a_back_button_when_the_task_can_go_back() {
        let layout = CaptionLayout::new(800, 31, 96, 0);
        let (left, top, side, _) = layout.icon_rect().unwrap();
        let middle = (left + side as i32 / 2, top + side as i32 / 2);
        let back = TitleBarMode {
            back: true,
            ..TitleBarMode::default()
        };
        // The arrow is pressed by this window, never by Windows, and the system menu stays
        // on the rest of the band (right click, Alt+Space).
        assert_eq!(hit_code(&layout, middle.0, middle.1, &back), HT_BACK);
        assert_eq!(button_of(HT_BACK), None);
        let (title_left, _, _, _) = layout.title_rect().unwrap();
        assert_eq!(
            hit_code(&layout, title_left + 4, middle.1, &back),
            HTCAPTION
        );
        // A click sends the task Back, pressed then released.
        let [down, up] = back_keys(7, 1234);
        assert_eq!((down.task_id, down.key_code), (7, 4));
        assert_eq!(down.action, KeyAction::Down);
        assert_eq!(up.action, KeyAction::Up);
        assert_eq!((up.key_code, up.event_time_ms), (4, 1234));
        assert!(!down.canceled && !up.canceled);
    }

    #[test]
    fn fr22_the_back_arrow_sends_back_only_while_the_task_can_go_back() {
        let (input, guest) = std::sync::mpsc::channel();
        let caption = CaptionWindow::new(7, WS_OVERLAPPEDWINDOW, 0, 1, input);
        let sent = || {
            let keys: Vec<InputEvent> = guest.try_iter().collect();
            keys.len()
        };
        let none = std::ptr::null_mut();
        // On the first screen the cell is the icon: a click there opens the window menu and
        // sends Android nothing, even a release over the cell.
        caption.press_back();
        assert!(!caption.release_back(1000));
        assert_eq!(sent(), 0);
        // SAFETY: with no presenter the window is never drawn, so no window is needed.
        unsafe { caption.set_back(none, true, None) };
        caption.press_back();
        assert!(caption.release_back(2000));
        assert_eq!(sent(), 2, "Back pressed and released");
        // A second click before Android has told the task's new state sends nothing: that
        // state is stale, and a Back on what may be the first screen by now closes the app.
        caption.press_back();
        assert!(!caption.release_back(2000 + BACK_SETTLE_MS - 1));
        assert_eq!(sent(), 0);
        // Told again (still past the first screen), the arrow works again at once.
        // SAFETY: as above.
        unsafe { caption.set_back(none, true, None) };
        caption.press_back();
        assert!(caption.release_back(2100));
        assert_eq!(sent(), 2);
        // A state that never comes (the task stayed past its first screen) holds it no
        // longer than the settle time.
        caption.press_back();
        assert!(caption.release_back(2100 + BACK_SETTLE_MS));
        assert_eq!(sent(), 2);
        // Back on the first screen: the cell is the icon again and sends nothing.
        // SAFETY: as above.
        unsafe { caption.set_back(none, false, None) };
        caption.press_back();
        assert!(!caption.release_back(9000));
        assert_eq!(sent(), 0);
        // A release with no press on the arrow (the press began elsewhere) sends nothing.
        // SAFETY: as above.
        unsafe { caption.set_back(none, true, None) };
        assert!(!caption.release_back(9500));
        assert_eq!(sent(), 0);
    }

    #[test]
    fn fr22_an_app_drawn_title_bar_gives_the_app_its_controls_and_keeps_the_rest() {
        let layout = CaptionLayout::new(800, 31, 96, 8);
        let tabs = [Bounds {
            left: 48,
            top: 0,
            right: 500,
            bottom: 31,
        }];
        let app_drawn = TitleBarMode {
            app_drawn: true,
            interactive: &tabs,
            back: false,
        };
        assert_eq!(hit_code(&layout, 100, 20, &app_drawn), HTCLIENT, "a tab");
        assert_eq!(
            hit_code(&layout, 600, 20, &app_drawn),
            HTCAPTION,
            "between the tabs"
        );
        assert_eq!(hit_code(&layout, 708, 20, &app_drawn), HTMAXBUTTON);
        assert_eq!(hit_code(&layout, 20, 20, &app_drawn), HTSYSMENU);
        assert_eq!(
            hit_code(&layout, 100, 3, &app_drawn),
            HTTOP,
            "the top edge first"
        );
        let withdrawn = TitleBarMode {
            interactive: &tabs,
            ..TitleBarMode::default()
        };
        assert_eq!(hit_code(&layout, 100, 20, &withdrawn), HTCAPTION);
    }

    #[test]
    fn fr22_the_apps_name_is_drawn_and_cut_before_its_box_ends() {
        // SAFETY: GDI calls on objects the function makes and releases itself.
        unsafe {
            let short = title_mask("Clock", (300, 31), 96).unwrap();
            assert_eq!(short.len(), 300 * 31);
            let inked = |mask: &[u8], from: usize, to: usize| {
                (0..31).any(|y| (from..to).any(|x| mask[y * 300 + x] > 16))
            };
            assert!(inked(&short, 0, 60), "the name starts at the left");
            assert!(!inked(&short, 100, 300), "a short name ends early");
            // Nothing above or below the line, which is centred in the band.
            assert!((0..300).all(|x| short[x] == 0 && short[30 * 300 + x] == 0));
            let long = "A name far too long to fit in the room the title bar leaves for it";
            let cut = title_mask(long, (300, 31), 96).unwrap();
            assert!(
                inked(&cut, 250, 300),
                "the ellipsis reaches the end of the box"
            );
            assert!(title_mask("", (300, 31), 96).is_none());
        }
    }

    #[test]
    fn fr22_the_title_bar_is_as_high_as_a_standard_windows_caption_at_every_scale() {
        for dpi in [96, 144, 192] {
            let height = title_bar_height(WS_OVERLAPPEDWINDOW, 0, dpi);
            let (_, top, _, bottom) = standard_frame(WS_OVERLAPPEDWINDOW, 0, dpi);
            assert_eq!(height as i32, top);
            // A caption is taller than a border, and grows with the scale.
            assert!(top > bottom, "at {dpi}: caption {top}, border {bottom}");
            assert!(
                height >= 20 * dpi / 96,
                "at {dpi} the title bar is {height} pixels"
            );
            let (_, task_top, _, task_bottom) = task_frame(WS_OVERLAPPEDWINDOW, 0, dpi, false);
            assert_eq!((task_top, task_bottom), (0, bottom));
            assert_eq!(task_frame(WS_OVERLAPPEDWINDOW, 0, dpi, true).1, bottom);
        }
    }
}

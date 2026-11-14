//! The windows of an open menu: one borderless popup per open level, drawn with
//! [`super::render`].
//!
//! A level's window keeps a standard frame that `WM_NCCALCSIZE` takes away, so DWM still
//! gives it its shadow, its rounded corners (`DWMWA_WINDOW_CORNER_PREFERENCE`) and its
//! backdrop (`DWMWA_SYSTEMBACKDROP_TYPE`, the transient acrylic). Its whole client area is
//! DWM's frame (`DwmExtendFrameIntoClientArea`), so the pixels it paints with alpha below
//! opaque show the backdrop through them. Where the backdrop is not drawn the menu paints an
//! opaque fill instead.
//!
//! [`show`] opens the menu and runs its messages until it closes, as a Windows menu does,
//! and returns what was chosen. The menu takes the foreground, so the keyboard reaches it,
//! and the mouse, so a press anywhere else closes it. It closes on Escape, on a press
//! outside it, when it loses the foreground, when its owner's window goes, and when it is
//! told to (`WM_CANCELMODE`). Submenus never take the focus: every key goes to the first
//! level's window, which acts on the deepest level. Each window stays drawn as active, so
//! DWM keeps its acrylic rather than the flat color of an inactive window.

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    GetMonitorInfoW, InvalidateRect, MonitorFromWindow, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_MENU, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowRect, IsWindow, KillTimer, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetCursor, SetForegroundWindow, SetTimer,
    SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage, HTCLIENT, HWND_TOPMOST,
    IDC_ARROW, MA_NOACTIVATE, MSG, SPI_GETMENUSHOWDELAY, SWP_NOACTIVATE, SW_SHOW,
    SW_SHOWNOACTIVATE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_APP, WM_CANCELMODE, WM_CAPTURECHANGED,
    WM_CHAR, WM_DESTROY, WM_ERASEBKGND, WM_GETOBJECT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};

use super::automation::{self, Action, View, ViewLevel, ViewRow};
use super::{
    key_of, layout_with_shortcuts, metrics, place, render, Anchor, Look, Mask, MenuEntry,
    MenuLayout, MenuState, Outcome, Rect, RowArt,
};
use crate::compositor::fluent::text::{self, Align, ICON_FACES, TEXT_FACES};
use crate::compositor::fluent::{glyph, menu_palette, read_theme, Backdrop, MenuPalette, Theme};
use crate::compositor::widgets::{px_of_dip, ScreenRect};

/// DWM's window attributes this needs, by number: windows-sys names some only on newer SDKs.
const DWMWA_TRANSITIONS_FORCEDISABLED: u32 = 3;
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
/// `DWMWCP_ROUND`: the larger of Windows 11's two corner radii, as a flyout has.
const DWMWCP_ROUND: i32 = 2;
/// `DWMSBT_TRANSIENTWINDOW`: Desktop Acrylic, the backdrop of flyouts and menus.
const DWMSBT_TRANSIENTWINDOW: i32 = 3;

/// The timer that settles a hover after the pointer rests on an item.
const SETTLE_TIMER: usize = 1;

/// The message UI Automation's calls post to the first level's window.
pub(super) const WM_MENU_ACTION: u32 = WM_APP + 0x40;

/// A menu to show.
pub struct Request<'a> {
    /// The window the menu is for, as a number; the menu closes if it goes.
    pub owner: isize,
    pub anchor: Anchor,
    pub entries: &'a [MenuEntry],
    /// What UI Automation calls the menu.
    pub name: &'a str,
    /// Whether the keyboard opened it: it then starts on its first item, with access keys
    /// underlined.
    pub keyboard: bool,
}

/// One open level's window.
struct LevelWindow {
    hwnd: HWND,
    /// The entry of the level above it was opened for.
    parent: Option<usize>,
    origin: (i32, i32),
    layout: MenuLayout,
    art: Vec<RowArt>,
    /// The UI Automation provider the window answered `WM_GETOBJECT` with.
    provider: Option<windows::Win32::UI::Accessibility::IRawElementProviderSimple>,
}

/// The menu while it is open.
struct Session {
    entries: Vec<MenuEntry>,
    state: MenuState,
    anchor: Anchor,
    name: String,
    theme: Theme,
    palette: MenuPalette,
    class: Vec<u16>,
    owner: HWND,
    windows: Vec<LevelWindow>,
    /// Where a press in the menu began: its level and the entry under it.
    press: Option<(usize, Option<usize>)>,
    /// Where the pointer last was, so a move that is not one changes nothing.
    pointer: (i32, i32),
    /// Whether the access keys are underlined.
    cues: bool,
    /// The deepest focused item UI Automation was last told of.
    announced: Option<(usize, usize)>,
    view: Arc<Mutex<View>>,
    done: bool,
    result: Option<u32>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Runs `f` on the open menu. None when no menu is open, or when the session is already in
/// use further up this thread's stack: a message sent while it is busy is left to the
/// default handling.
fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|session| match session.try_borrow_mut() {
        Ok(mut session) => session.as_mut().map(f),
        Err(_) => None,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn lo(l: LPARAM) -> i32 {
    (l & 0xffff) as u16 as i16 as i32
}

fn hi(l: LPARAM) -> i32 {
    ((l >> 16) & 0xffff) as u16 as i16 as i32
}

/// Shows the menu and returns the id of the item chosen, or None when it closed with none.
/// Called on the owner's thread, which it runs the messages of while the menu is open; no
/// state of the caller may be borrowed meanwhile. A second menu asked for while one is open
/// on the thread is not shown.
pub fn show(request: Request<'_>) -> Option<u32> {
    if request.entries.is_empty() {
        return None;
    }
    let busy = SESSION.with(|session| match session.try_borrow() {
        Ok(session) => session.is_some(),
        Err(_) => true,
    });
    if busy {
        return None;
    }
    let class = register_class();
    let theme = read_theme();
    let mut pointer = POINT { x: 0, y: 0 };
    // SAFETY: a plain read of the cursor's position.
    unsafe { GetCursorPos(&mut pointer) };
    // SAFETY: a plain query.
    let previous = unsafe { GetForegroundWindow() };
    let entries = request.entries.to_vec();
    let state = MenuState::new(&entries, request.keyboard);
    SESSION.with(|session| {
        *session.borrow_mut() = Some(Session {
            entries,
            state,
            anchor: request.anchor,
            name: request.name.to_string(),
            theme,
            palette: menu_palette(&theme),
            class,
            owner: request.owner as HWND,
            windows: Vec::new(),
            press: None,
            pointer: (pointer.x, pointer.y),
            cues: request.keyboard,
            announced: None,
            view: Arc::new(Mutex::new(View::open())),
            done: false,
            result: None,
        })
    });
    refresh();
    let root = with_session(|s| s.windows.first().map(|w| w.hwnd)).flatten();
    if let Some(root) = root {
        // SAFETY: the menu's own window, on this thread. It takes the foreground so the
        // keyboard reaches it (this thread just had the user's click or key), and the mouse
        // so a press anywhere else closes it.
        unsafe {
            SetForegroundWindow(root);
            SetFocus(root);
            SetCapture(root);
        }
        automation::raise_opened(&view(), 0);
        announce();
        run(root);
    }
    close(previous)
}

/// The menu's UI Automation view.
fn view() -> Arc<Mutex<View>> {
    with_session(|s| Arc::clone(&s.view)).unwrap_or_default()
}

/// Runs this thread's messages until the menu is done.
fn run(root: HWND) {
    loop {
        let (done, owner) = with_session(|s| (s.done, s.owner)).unwrap_or((true, null_mut()));
        // SAFETY: plain queries of windows that may have gone.
        let gone = unsafe { (!owner.is_null() && IsWindow(owner) == 0) || IsWindow(root) == 0 };
        if done || gone {
            return;
        }
        // SAFETY: the usual message loop; WM_QUIT is put back for the caller's loop.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            match GetMessageW(&mut msg, null_mut(), 0, 0) {
                0 => {
                    PostQuitMessage(msg.wParam as i32);
                    return;
                }
                -1 => return,
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// Closes every window of the menu, gives the foreground back to the window that had it
/// when the menu still has it, and returns what was chosen.
fn close(previous: HWND) -> Option<u32> {
    let (windows, result, view) = with_session(|s| {
        s.done = true;
        let windows: Vec<HWND> = s.windows.iter().map(|w| w.hwnd).collect();
        (windows, s.result, Arc::clone(&s.view))
    })?;
    let root = windows.first().copied().unwrap_or(null_mut());
    // SAFETY: the menu's windows, on this thread, and plain queries of others.
    unsafe {
        KillTimer(root, SETTLE_TIMER);
        if !root.is_null() && GetCapture() == root {
            ReleaseCapture();
        }
        let foreground = GetForegroundWindow();
        if windows.contains(&foreground) && !previous.is_null() && IsWindow(previous) != 0 {
            SetForegroundWindow(previous);
        }
    }
    for _ in 0..windows.len() {
        close_level();
    }
    if let Ok(mut view) = view.lock() {
        view.alive = false;
        view.levels.clear();
    }
    SESSION.with(|session| *session.borrow_mut() = None);
    result
}

/// Closes the deepest open level's window.
fn close_level() {
    let Some(window) = with_session(|s| s.windows.pop()).flatten() else {
        return;
    };
    let level = with_session(|s| s.windows.len()).unwrap_or(0);
    let view = view();
    automation::raise_closed(&view, level);
    automation::release(window.hwnd as isize, window.provider);
    // SAFETY: a window of this thread.
    unsafe { DestroyWindow(window.hwnd) };
}

/// Registers the menu's window class once, and returns its name.
fn register_class() -> Vec<u16> {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    let class = wide(&format!("DsaFluentMenu_{}", std::process::id()));
    REGISTERED.call_once(|| {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(menu_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            // SAFETY: the module handle of this process.
            hInstance: unsafe { GetModuleHandleW(null()) },
            hIcon: null_mut(),
            // SAFETY: a system cursor.
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        // SAFETY: the system copies the name; the class lives as long as the process.
        unsafe { RegisterClassExW(&wc) };
    });
    class
}

/// Makes the windows match the open levels: closes those no longer open (or opened for
/// another item), opens those missing, draws them all again, and tells UI Automation.
fn refresh() {
    loop {
        let stale = with_session(|s| {
            let levels = s.state.levels();
            s.windows
                .iter()
                .enumerate()
                .any(|(index, window)| levels.get(index).map(|l| l.parent) != Some(window.parent))
        })
        .unwrap_or(false);
        if !stale {
            break;
        }
        close_level();
    }
    let mut opened = Vec::new();
    loop {
        let missing = with_session(|s| s.windows.len() < s.state.depth()).unwrap_or(false);
        if !missing {
            break;
        }
        let level = with_session(|s| s.windows.len()).unwrap_or(0);
        if open_level(level).is_none() {
            with_session(|s| s.done = true);
            return;
        }
        opened.push(level);
    }
    let windows = with_session(|s| s.windows.iter().map(|w| w.hwnd).collect::<Vec<_>>());
    for hwnd in windows.unwrap_or_default() {
        // SAFETY: a window of this thread; it is painted again from the session.
        unsafe { InvalidateRect(hwnd, null(), 0) };
    }
    sync_view();
    // The menu itself says it opened once it has the foreground ([`show`]).
    for level in opened.into_iter().filter(|&level| level > 0) {
        automation::raise_opened(&view(), level);
    }
}

/// Opens the window of level `level`: at the anchor for the menu itself, beside its parent
/// item for a submenu, laid out at the DPI of the monitor it opens on.
fn open_level(level: usize) -> Option<()> {
    let (theme, class, name) = with_session(|s| (s.theme, s.class.clone(), s.name.clone()))?;
    let (entries, anchor, parent) = with_session(|s| {
        let entries = s.state.entries(&s.entries, level).to_vec();
        let parent = s.state.levels().get(level)?.parent;
        let anchor = match (level, parent) {
            (0, _) => s.anchor,
            (_, Some(entry)) => {
                let above = s.windows.get(level - 1)?;
                let row = above.layout.row_of(entry)?;
                let px = |dip: u32| px_of_dip(dip, above.layout.dpi);
                let inset = px(metrics::PRESENTER_PADDING_Y) + px(metrics::ITEM_MARGIN_Y);
                let top = above.origin.1 + row.highlight.y - inset;
                let left = above.origin.0;
                let right = left + above.layout.width;
                Anchor::Beside((left, top, right, top + row.slot.h))
            }
            (_, None) => return None,
        };
        Some((entries, anchor, parent))
    })
    .flatten()?;
    let (x, y) = anchor.point();
    let hwnd = create_level_window(&class, &name, level, (x, y));
    if hwnd.is_null() {
        return None;
    }
    // SAFETY: a window of this thread.
    let dpi = match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        dpi => dpi,
    };
    let area = work_area(hwnd).unwrap_or((x - 4000, y - 4000, x + 4000, y + 4000));
    let em = px_of_dip(metrics::TEXT_SIZE, dpi);
    let widths: Vec<i32> = entries
        .iter()
        .map(|entry| match entry.item() {
            Some(item) => text::measure(&item.label, &TEXT_FACES, em),
            None => 0,
        })
        .collect();
    let shortcut_em = px_of_dip(metrics::SHORTCUT_TEXT_SIZE, dpi);
    let shortcut_widths: Vec<i32> = entries
        .iter()
        .map(
            |entry| match entry.item().and_then(|item| item.shortcut.as_deref()) {
                Some(shortcut) => text::measure(shortcut, &TEXT_FACES, shortcut_em),
                None => 0,
            },
        )
        .collect();
    let laid = layout_with_shortcuts(&entries, &widths, &shortcut_widths, dpi);
    let art = row_art(&entries, &laid);
    let gap = px_of_dip(metrics::ANCHOR_GAP, dpi);
    let (left, top) = place(anchor, (laid.width, laid.height), area, gap);
    let (width, height) = (laid.width, laid.height);
    with_session(|s| {
        s.windows.push(LevelWindow {
            hwnd,
            parent,
            origin: (left, top),
            layout: laid,
            art,
            provider: None,
        });
    });
    // SAFETY: a window of this thread, shown once it has its place and its look.
    unsafe {
        style(hwnd, &theme);
        SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height, SWP_NOACTIVATE);
        if level == 0 {
            ShowWindow(hwnd, SW_SHOW);
        } else {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            // Drawn as active, so DWM keeps its acrylic.
            SendMessageW(hwnd, WM_NCACTIVATE, 1, 0);
        }
    }
    Some(())
}

/// The window styles of a level's window: a popup with a sizing frame, which `WM_NCCALCSIZE`
/// takes away but which keeps DWM's shadow; a tool window, so it has no taskbar button, above
/// every other window. Only the first level takes the foreground.
fn level_styles(level: usize) -> (u32, u32) {
    let ex_style = if level == 0 {
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST
    } else {
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
    };
    (WS_POPUP | WS_THICKFRAME, ex_style)
}

/// Makes the (hidden) window of level `level` at `at` on the screen, so it takes the DPI of
/// that point's monitor. Null when it cannot be made.
fn create_level_window(class: &[u16], name: &str, level: usize, at: (i32, i32)) -> HWND {
    let title = wide(name);
    let (style, ex_style) = level_styles(level);
    // SAFETY: a popup of this thread of a class registered by `register_class`; the names
    // outlive the call.
    unsafe {
        CreateWindowExW(
            ex_style,
            class.as_ptr(),
            title.as_ptr(),
            style,
            at.0,
            at.1,
            1,
            1,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    }
}

/// DWM's window attributes a level's window takes for `theme`, as (attribute, value): dark or
/// light, no open animation (a menu has none), the larger rounded corners of a flyout from
/// Windows 11, and the transient acrylic backdrop where it is drawn.
pub(super) fn frame_attributes(theme: &Theme) -> Vec<(u32, i32)> {
    let mut attributes = vec![
        (DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(theme.dark)),
        (DWMWA_TRANSITIONS_FORCEDISABLED, 1),
    ];
    if theme.rounded() {
        attributes.push((DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND));
    }
    if theme.backdrop() == Backdrop::Acrylic {
        attributes.push((DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW));
    }
    attributes
}

/// Gives a level's window its look ([`frame_attributes`]), with its whole client area DWM's
/// frame, so the see-through pixels show the backdrop. Returns what DWM answered for each
/// attribute.
unsafe fn style(hwnd: HWND, theme: &Theme) -> Vec<(u32, i32)> {
    let set = |attribute: u32, value: i32| {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            &value as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    };
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    DwmExtendFrameIntoClientArea(hwnd, &margins);
    frame_attributes(theme)
        .into_iter()
        .map(|(attribute, value)| (attribute, set(attribute, value)))
        .collect()
}

/// The work area of the monitor `hwnd` is on.
pub(crate) fn work_area(hwnd: HWND) -> Option<ScreenRect> {
    // SAFETY: a window of this thread, and a structure of the right size to fill.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let w = info.rcWork;
        Some((w.left, w.top, w.right, w.bottom))
    }
}

/// The text and glyphs of each row of `laid`, rasterized at its DPI.
pub fn row_art(entries: &[MenuEntry], laid: &MenuLayout) -> Vec<RowArt> {
    let px = |dip: u32| px_of_dip(dip, laid.dpi);
    let em = px(metrics::TEXT_SIZE);
    let glyph_mask = |symbol: Option<char>, at: Option<Rect>, size: u32| {
        let (symbol, at) = (symbol?.to_string(), at?);
        let (width, height) = (at.w.max(0) as usize, at.h.max(0) as usize);
        let em = px(size);
        let cover = text::mask(&symbol, &ICON_FACES, em, (width, height), Align::Center)?;
        Some(Mask {
            width,
            height,
            cover,
        })
    };
    laid.rows
        .iter()
        .map(|row| {
            let Some(item) = entries.get(row.entry).and_then(MenuEntry::item) else {
                return RowArt::default();
            };
            let (width, height) = (row.text.w.max(0) as usize, row.text.h.max(0) as usize);
            let label = text::mask(&item.label, &TEXT_FACES, em, (width, height), Align::Left);
            let check = (item.checked == Some(true)).then_some(glyph::CHECK);
            let chevron = item.opens_submenu().then_some(glyph::CHEVRON_RIGHT);
            let shortcut = item
                .shortcut
                .as_deref()
                .zip(row.shortcut)
                .and_then(|(text, at)| {
                    let (width, height) = (at.w.max(0) as usize, at.h.max(0) as usize);
                    let em = px(metrics::SHORTCUT_TEXT_SIZE);
                    let cover = text::mask(text, &TEXT_FACES, em, (width, height), Align::Left)?;
                    Some(Mask {
                        width,
                        height,
                        cover,
                    })
                });
            RowArt {
                shortcut,
                label: label.map(|cover| Mask {
                    width,
                    height,
                    cover,
                }),
                glyph: glyph_mask(item.glyph, row.glyph, metrics::GLYPH_SIZE),
                check: glyph_mask(check, row.check, metrics::GLYPH_SIZE),
                chevron: glyph_mask(chevron, row.chevron, metrics::CHEVRON_SIZE),
                underline: underline(&item.label, item.access_key, row.text, em),
            }
        })
        .collect()
}

/// The line under the first letter of `label` that is its access key `key`, a little below
/// the baseline of the label's line, vertically centred in `bounds`.
fn underline(label: &str, key: Option<char>, bounds: Rect, em: i32) -> Option<Rect> {
    let key = key?;
    let (at, letter) = label
        .char_indices()
        .find(|(_, c)| c.eq_ignore_ascii_case(&key))?;
    let before = text::measure(&label[..at], &TEXT_FACES, em);
    let width = text::measure(&letter.to_string(), &TEXT_FACES, em);
    if before + width > bounds.w {
        return None;
    }
    Some(Rect {
        x: bounds.x + before,
        y: bounds.y + bounds.h / 2 + em * 42 / 100 + 1,
        w: width,
        h: (em / 14).max(1),
    })
}

impl Session {
    /// The level whose window `hwnd` is.
    fn level_of(&self, hwnd: HWND) -> Option<usize> {
        self.windows.iter().position(|w| w.hwnd == hwnd)
    }

    /// The level and the entry under the screen point, the deepest level first; None
    /// outside every level.
    fn hit(&self, (x, y): (i32, i32)) -> Option<(usize, Option<usize>)> {
        self.windows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(level, w)| {
                let (left, top) = w.origin;
                let (right, bottom) = (left + w.layout.width, top + w.layout.height);
                let inside = x >= left && y >= top && x < right && y < bottom;
                let entries = self.state.entries(&self.entries, level);
                inside.then(|| (level, w.layout.hit(entries, x - left, y - top)))
            })
    }

    /// The pixels of the window `hwnd` as it looks now, with its size.
    fn pixels(&self, hwnd: HWND) -> Option<(Vec<u8>, i32, i32)> {
        let level = self.level_of(hwnd)?;
        let window = &self.windows[level];
        let pressed = match self.press {
            Some((at, entry)) if at == level => entry,
            _ => None,
        };
        let look = Look {
            layout: &window.layout,
            art: &window.art,
            entries: self.state.entries(&self.entries, level),
            focus: self.state.focus(level),
            pressed,
            cues: self.cues,
            palette: &self.palette,
        };
        Some((render(&look), window.layout.width, window.layout.height))
    }
}

/// Acts on what a key, the pointer or UI Automation did.
fn apply(outcome: Outcome) {
    match outcome {
        Outcome::Nothing => {}
        Outcome::Changed => {
            refresh();
            announce();
        }
        Outcome::Invoke(id) => finish(Some(id)),
        Outcome::Dismiss => finish(None),
    }
}

/// `outcome`, or a drawing again when it changed nothing: a press's highlight goes.
fn redrawn(outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Nothing => Outcome::Changed,
        outcome => outcome,
    }
}

/// Ends the menu with `result`, unless it ended already.
fn finish(result: Option<u32>) {
    with_session(|s| {
        if !s.done {
            s.done = true;
            s.result = result;
        }
    });
}

/// Starts or stops the pause after which a hover opens or closes a submenu.
fn settle_later() {
    let pending = with_session(|s| {
        let root = s.windows.first().map(|w| w.hwnd);
        (root, s.state.unsettled(&s.entries))
    });
    let Some((Some(root), unsettled)) = pending else {
        return;
    };
    // SAFETY: a timer of the menu's own window, and a plain read of a setting.
    unsafe {
        if unsettled {
            let mut delay = 0u32;
            SystemParametersInfoW(
                SPI_GETMENUSHOWDELAY,
                0,
                &mut delay as *mut u32 as *mut std::ffi::c_void,
                0,
            );
            let delay = if delay == 0 { 400 } else { delay };
            SetTimer(root, SETTLE_TIMER, delay, None);
        } else {
            KillTimer(root, SETTLE_TIMER);
        }
    }
}

/// What the pointer did, at a point on the screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pointer {
    Move,
    Down,
    Up,
}

fn pointer(screen: (i32, i32), action: Pointer) {
    let outcome = with_session(|s| {
        let hit = s.hit(screen);
        match action {
            Pointer::Move => {
                if s.pointer == screen {
                    return Outcome::Nothing;
                }
                s.pointer = screen;
                let deepest = s.state.depth() - 1;
                let (level, entry) = hit.unwrap_or((deepest, None));
                s.state.hover(&s.entries, level, entry)
            }
            Pointer::Down => match hit {
                None => Outcome::Dismiss,
                Some(at) => {
                    s.press = Some(at);
                    Outcome::Changed
                }
            },
            Pointer::Up => {
                let pressed = s.press.take();
                let on = match hit {
                    Some((level, Some(entry))) => Some((level, entry)),
                    _ => None,
                };
                match (pressed, on) {
                    (None, _) => Outcome::Nothing,
                    (Some(_), None) => Outcome::Changed,
                    (Some(_), Some((level, entry))) => {
                        redrawn(s.state.click(&s.entries, level, entry))
                    }
                }
            }
        }
    })
    .unwrap_or(Outcome::Nothing);
    apply(outcome);
    if action == Pointer::Move {
        settle_later();
    }
}

fn keyed(vk: u16, alt: bool) {
    // SAFETY: plain reads of the keyboard's state.
    let shift = unsafe { GetKeyState(i32::from(VK_SHIFT)) } < 0;
    // SAFETY: as above.
    let alt = alt || unsafe { GetKeyState(i32::from(VK_MENU)) } < 0;
    let Some(key) = key_of(vk, shift, alt) else {
        return;
    };
    let outcome = with_session(|s| {
        let cued = !std::mem::replace(&mut s.cues, true);
        match (cued, s.state.key(&s.entries, key)) {
            (true, Outcome::Nothing) => Outcome::Changed,
            (_, outcome) => outcome,
        }
    })
    .unwrap_or(Outcome::Nothing);
    apply(outcome);
}

/// Acts on what UI Automation asked for.
fn act(action: Action, level: usize, entry: usize) {
    let outcome = with_session(|s| match action {
        Action::Invoke => s.state.click(&s.entries, level, entry),
        Action::Expand => s.state.expand(&s.entries, level, entry),
        Action::Collapse => s.state.collapse(level),
        Action::Focus => s.state.focus_on(&s.entries, level, entry),
    })
    .unwrap_or(Outcome::Nothing);
    apply(outcome);
}

/// Writes what UI Automation reads of the menu now.
fn sync_view() {
    with_session(|s| {
        let root = s.windows.first().map(|w| w.hwnd as isize).unwrap_or(0);
        let levels = s
            .windows
            .iter()
            .enumerate()
            .map(|(level, window)| {
                let entries = s.state.entries(&s.entries, level);
                let open = s.state.levels().get(level + 1).and_then(|l| l.parent);
                let parent = s.state.levels()[level].parent;
                let above = s.state.entries(&s.entries, level.saturating_sub(1));
                let label = parent.and_then(|p| above.get(p)).and_then(MenuEntry::item);
                let name = match label {
                    Some(item) if level > 0 => item.label.clone(),
                    _ => s.name.clone(),
                };
                let rows = window
                    .layout
                    .rows
                    .iter()
                    .map(|row| {
                        let item = entries.get(row.entry).and_then(MenuEntry::item);
                        let at = row.slot;
                        ViewRow {
                            entry: row.entry,
                            name: item.map(|i| i.label.clone()).unwrap_or_default(),
                            separator: row.separator,
                            enabled: item.is_some_and(|i| i.enabled),
                            checked: item.and_then(|i| i.checked),
                            submenu: item.is_some_and(|i| i.opens_submenu()),
                            expanded: open == Some(row.entry),
                            access_key: item.and_then(|i| i.access_key),
                            shortcut: item.and_then(|i| i.shortcut.clone()),
                            bounds: (window.origin.0 + at.x, window.origin.1 + at.y, at.w, at.h),
                        }
                    })
                    .collect();
                ViewLevel {
                    hwnd: window.hwnd as isize,
                    name,
                    rows,
                    focus: s.state.focus(level),
                }
            })
            .collect();
        if let Ok(mut view) = s.view.lock() {
            view.alive = true;
            view.root = root;
            view.levels = levels;
        }
    });
}

/// Tells UI Automation of the item the keyboard or the pointer is on now, when it changed.
fn announce() {
    let changed = with_session(|s| {
        let deepest = s.state.depth() - 1;
        let now = s.state.focus(deepest).map(|entry| (deepest, entry));
        let was = std::mem::replace(&mut s.announced, now);
        (now != was).then_some(now).flatten()
    })
    .flatten();
    if let Some((level, entry)) = changed {
        automation::raise_focus(&view(), level, entry);
    }
}

/// Paints the window `hwnd` from the session.
unsafe fn paint(hwnd: HWND) {
    let pixels = with_session(|s| s.pixels(hwnd)).flatten();
    let mut paint: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    if let (false, Some((pixels, width, height))) = (dc.is_null(), pixels) {
        blit(dc, &pixels, width, height);
    }
    EndPaint(hwnd, &paint);
}

/// Copies premultiplied BGRA `pixels` of `width` by `height` to `dc`, alpha and all, which
/// the DWM frame under the client area blends with the backdrop.
pub(crate) unsafe fn blit(dc: HDC, pixels: &[u8], width: i32, height: i32) {
    let memory = CreateCompatibleDC(dc);
    if memory.is_null() {
        return;
    }
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..std::mem::zeroed()
    };
    let mut bits: *mut std::ffi::c_void = null_mut();
    let bitmap = CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    let size = (width.max(0) * height.max(0) * 4) as usize;
    if !bitmap.is_null() && !bits.is_null() && pixels.len() >= size {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, size);
        let old = SelectObject(memory, bitmap);
        BitBlt(dc, 0, 0, width, height, memory, 0, 0, SRCCOPY);
        SelectObject(memory, old);
    }
    if !bitmap.is_null() {
        DeleteObject(bitmap);
    }
    DeleteDC(memory);
}

/// The screen point of a mouse message to `hwnd`: its client area is the whole window.
fn screen_point(hwnd: HWND, lparam: LPARAM) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a window of this thread and a rectangle to fill.
    unsafe { GetWindowRect(hwnd, &mut rect) };
    (rect.left + lo(lparam), rect.top + hi(lparam))
}

/// Whether `hwnd` is one of the menu's windows.
fn ours(hwnd: HWND) -> bool {
    with_session(|s| s.level_of(hwnd).is_some()).unwrap_or(false)
}

unsafe extern "system" fn menu_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // The client area is the whole window; DWM keeps the frame's shadow and corners.
        WM_NCCALCSIZE if wparam != 0 => 0,
        WM_NCHITTEST => HTCLIENT as LRESULT,
        // Always drawn as active, so DWM keeps the acrylic. -1: nothing of a frame painted.
        WM_NCACTIVATE => DefWindowProcW(hwnd, msg, 1, -1),
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
            1
        }
        WM_MOUSEMOVE => {
            pointer(screen_point(hwnd, lparam), Pointer::Move);
            0
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            pointer(screen_point(hwnd, lparam), Pointer::Down);
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            pointer(screen_point(hwnd, lparam), Pointer::Up);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            keyed(wparam as u16, msg == WM_SYSKEYDOWN);
            0
        }
        // The characters the keys make are not wanted: no beep, no system menu.
        WM_CHAR | WM_SYSCHAR => 0,
        WM_TIMER if wparam == SETTLE_TIMER => {
            KillTimer(hwnd, SETTLE_TIMER);
            let outcome = with_session(|s| s.state.settle(&s.entries));
            apply(outcome.unwrap_or(Outcome::Nothing));
            0
        }
        WM_ACTIVATE => {
            // Another window took the foreground.
            if wparam & 0xffff == 0 && !ours(lparam as HWND) {
                finish(None);
            }
            0
        }
        WM_ACTIVATEAPP => {
            if wparam == 0 {
                finish(None);
            }
            0
        }
        WM_CAPTURECHANGED => {
            if !ours(lparam as HWND) {
                finish(None);
            }
            0
        }
        WM_CANCELMODE => {
            finish(None);
            0
        }
        WM_MENU_ACTION => {
            if let Some(action) = Action::from_code(wparam) {
                let at = lparam as usize;
                act(action, at >> 16, at & 0xffff);
            }
            0
        }
        WM_GETOBJECT => {
            let answered = with_session(|s| {
                let level = s.level_of(hwnd)?;
                let view = Arc::clone(&s.view);
                let window = &mut s.windows[level];
                let provider = window
                    .provider
                    .get_or_insert_with(|| automation::level_provider(&view, level))
                    .clone();
                Some(provider)
            })
            .flatten();
            match answered.and_then(|p| automation::answer(hwnd as isize, wparam, lparam, &p)) {
                Some(result) => result,
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::fluent::{HighContrast, BACKDROP_BUILD, ROUNDED_BUILD};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GWL_EXSTYLE, GWL_STYLE, WS_EX_APPWINDOW,
    };

    fn value(attributes: &[(u32, i32)], attribute: u32) -> Option<i32> {
        attributes
            .iter()
            .find(|(a, _)| *a == attribute)
            .map(|(_, v)| *v)
    }

    /// The tray menu is the same menu as the widgets': its window is a Windows 11 flyout.
    #[test]
    fn fr24_the_tray_menu_window_is_a_windows_11_flyout() {
        // On Windows 11 22H2 and later: rounded, acrylic, in the user's light or dark theme,
        // with no open animation.
        let light = Theme::light();
        let attributes = frame_attributes(&light);
        assert_eq!(
            value(&attributes, DWMWA_WINDOW_CORNER_PREFERENCE),
            Some(DWMWCP_ROUND)
        );
        assert_eq!(
            value(&attributes, DWMWA_SYSTEMBACKDROP_TYPE),
            Some(DWMSBT_TRANSIENTWINDOW)
        );
        assert_eq!(value(&attributes, DWMWA_USE_IMMERSIVE_DARK_MODE), Some(0));
        assert_eq!(value(&attributes, DWMWA_TRANSITIONS_FORCEDISABLED), Some(1));
        let dark = Theme {
            dark: true,
            ..light
        };
        assert_eq!(
            value(&frame_attributes(&dark), DWMWA_USE_IMMERSIVE_DARK_MODE),
            Some(1)
        );
        // Windows 11 before 22H2 rounds it with a solid fill; Windows 10 draws neither.
        let early = Theme {
            build: ROUNDED_BUILD,
            ..light
        };
        assert!(value(&frame_attributes(&early), DWMWA_WINDOW_CORNER_PREFERENCE).is_some());
        assert!(value(&frame_attributes(&early), DWMWA_SYSTEMBACKDROP_TYPE).is_none());
        let ten = Theme {
            build: 19045,
            ..light
        };
        assert!(value(&frame_attributes(&ten), DWMWA_WINDOW_CORNER_PREFERENCE).is_none());
        // High contrast and transparency effects off: a solid fill, still rounded.
        let contrast = Theme {
            high_contrast: Some(HighContrast {
                window: 0xFF00_0000,
                text: 0xFFFF_FFFF,
                highlight: 0xFF1A_EBFF,
                highlight_text: 0xFF00_0000,
                gray_text: 0xFF3F_F23F,
            }),
            ..light
        };
        assert!(value(&frame_attributes(&contrast), DWMWA_SYSTEMBACKDROP_TYPE).is_none());
        let opaque = Theme {
            transparency: false,
            ..light
        };
        assert!(value(&frame_attributes(&opaque), DWMWA_SYSTEMBACKDROP_TYPE).is_none());
        assert!(value(&frame_attributes(&opaque), DWMWA_WINDOW_CORNER_PREFERENCE).is_some());

        // A real window of the menu, on this Windows: a framed popup (DWM's shadow), a tool
        // window above the others with no taskbar button, and DWM takes every attribute this
        // build has.
        let class = register_class();
        let hwnd = create_level_window(&class, "Music", 0, (0, 0));
        assert!(!hwnd.is_null(), "the menu's window was not made");
        // SAFETY: a window of this thread, destroyed below.
        unsafe {
            let styles = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(styles & WS_POPUP, WS_POPUP);
            assert_eq!(styles & WS_THICKFRAME, WS_THICKFRAME);
            assert_eq!(ex_style & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
            assert_eq!(ex_style & WS_EX_TOPMOST, WS_EX_TOPMOST);
            assert_eq!(ex_style & WS_EX_APPWINDOW, 0);
            let theme = read_theme();
            let answers = style(hwnd, &theme);
            assert_eq!(answers.len(), frame_attributes(&theme).len());
            for (attribute, answer) in &answers {
                let known = match *attribute {
                    DWMWA_WINDOW_CORNER_PREFERENCE => theme.build >= ROUNDED_BUILD,
                    DWMWA_SYSTEMBACKDROP_TYPE => theme.build >= BACKDROP_BUILD,
                    _ => true,
                };
                if known {
                    assert!(
                        *answer >= 0,
                        "DWM refused attribute {attribute} (0x{:08x})",
                        *answer as u32
                    );
                }
            }
            DestroyWindow(hwnd);
        }
    }
}

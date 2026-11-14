//! The app's icon as the window manager last sent it while the app's windows are open.
//!
//! The program's own icon is read once, when it starts: the window class's, which the taskbar
//! and Alt+Tab show, and the title bar's. When the app's icon changes while its windows are
//! open (the app was updated, or its icon could not be drawn when the program was made), the
//! window manager sends the new one (`ToHost::AppIcon`). Each window then takes it as its
//! small and big icon (`WM_SETICON`), drawn at the sizes Windows uses at the window's DPI, so
//! the taskbar and Alt+Tab show it at once, and its title bar draws it. A window that opens
//! later, or moves to a monitor of another scale, takes it too. The program itself is made
//! again with it once its windows close.

use std::cell::RefCell;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, SendMessageW, HICON, ICONINFO, ICON_BIG, ICON_SMALL,
    SM_CXICON, SM_CXSMICON, WM_SETICON,
};

use crate::compositor::widgets::png::{size, with_com, Decoder};

/// The app's icon and its title bar's icon, as PNG images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveIcon {
    pub icon_png: Vec<u8>,
    /// An adaptive icon's foreground layer alone; empty when the icon is not adaptive.
    pub caption_icon_png: Vec<u8>,
}

impl LiveIcon {
    /// The image the title bar draws: the foreground layer when there is one.
    pub fn caption_png(&self) -> &[u8] {
        if self.caption_icon_png.is_empty() {
            &self.icon_png
        } else {
            &self.caption_icon_png
        }
    }
}

/// The largest image a window's icon is decoded from, in pixels on a side, so a broken
/// message cannot take much memory.
const MAX_SOURCE: u32 = 2048;

thread_local! {
    /// The windows' thread's icon: every window of the program is on that one thread, and a
    /// test that runs several window hosts in one process keeps each one's apart.
    static CURRENT: RefCell<Option<Arc<LiveIcon>>> = const { RefCell::new(None) };
}

/// Records the app's new icon for every window of this program, on the windows' thread. An
/// image that is not a PNG of a sensible size is ignored, and the windows keep the icon they
/// have. Returns whether it was taken.
pub fn set(icon_png: Vec<u8>, caption_icon_png: Vec<u8>) -> bool {
    let readable = |png: &[u8]| size(png).is_some_and(|(w, h)| w <= MAX_SOURCE && h <= MAX_SOURCE);
    if !readable(&icon_png) {
        return false;
    }
    let caption_icon_png = if readable(&caption_icon_png) {
        caption_icon_png
    } else {
        Vec::new()
    };
    let icon = Arc::new(LiveIcon {
        icon_png,
        caption_icon_png,
    });
    CURRENT.with(|current| *current.borrow_mut() = Some(icon));
    true
}

/// The app's icon the window manager sent, if it sent one, on the windows' thread.
pub fn current() -> Option<Arc<LiveIcon>> {
    CURRENT.with(|current| current.borrow().clone())
}

/// `png` scaled to `side` pixels square, as rows of premultiplied BGRA pixels, as the title
/// bar and the splash draw it.
pub fn premultiplied(png: &[u8], side: u32) -> Option<Vec<u8>> {
    if side == 0 {
        return None;
    }
    with_com(|| Decoder::new().ok()?.decode(png, side, side).ok())
}

/// An adaptive icon's foreground layer `png` as the title bar draws it, `side` pixels square:
/// cropped to its logo and enlarged to the size of a whole icon, as the program's own
/// foreground is ([`dsa_host_core::compositor::caption::caption_icon`]).
pub fn caption_logo(png: &[u8], side: u32) -> Option<Vec<u8>> {
    let (width, height) = size(png)?;
    if side == 0 || width == 0 || height == 0 || width > MAX_SOURCE || height > MAX_SOURCE {
        return None;
    }
    let pixels = with_com(|| Decoder::new().ok()?.decode(png, width, height).ok())?;
    Some(dsa_host_core::compositor::caption::caption_icon(
        &pixels, width, height, side, true,
    ))
}

/// The sides of a window's small and big icons at `dpi`, as Windows draws them there.
pub fn icon_sides(dpi: u32) -> (u32, u32) {
    // SAFETY: plain metric queries.
    let (small, big) = unsafe {
        (
            GetSystemMetricsForDpi(SM_CXSMICON, dpi),
            GetSystemMetricsForDpi(SM_CXICON, dpi),
        )
    };
    (small.max(16) as u32, big.max(32) as u32)
}

/// A window's small and big icons, destroyed when they are no longer its icons.
pub struct WindowIcons {
    small: HICON,
    big: HICON,
}

impl WindowIcons {
    /// Whether both icons were made.
    pub fn is_complete(&self) -> bool {
        !self.small.is_null() && !self.big.is_null()
    }
}

impl Drop for WindowIcons {
    fn drop(&mut self) {
        // SAFETY: icons made by `icon` below, which nothing shows any more.
        unsafe {
            if !self.small.is_null() {
                DestroyIcon(self.small);
            }
            if !self.big.is_null() {
                DestroyIcon(self.big);
            }
        }
    }
}

/// Gives the window `hwnd` the icon `png` as its small and big icon, drawn at the sizes of
/// its monitor's DPI, so the taskbar, Alt+Tab and the window's system menu show it at once.
/// Returns the icons, which must outlive their use by the window; `None` when none could be
/// made, and the window keeps its icon.
///
/// # Safety
///
/// `hwnd` is a live window of the calling thread.
pub unsafe fn apply(hwnd: HWND, png: &[u8]) -> Option<WindowIcons> {
    let (small_side, big_side) = icon_sides(GetDpiForWindow(hwnd).max(96));
    let icons = with_com(|| {
        let decoder = Decoder::new().ok()?;
        let made = |side: u32| {
            decoder
                .decode_straight(png, side, side)
                .ok()
                .map(|pixels| icon(&pixels, side))
                .unwrap_or(std::ptr::null_mut())
        };
        Some(WindowIcons {
            small: made(small_side),
            big: made(big_side),
        })
    })?;
    if icons.small.is_null() && icons.big.is_null() {
        return None;
    }
    if !icons.small.is_null() {
        SendMessageW(
            hwnd,
            WM_SETICON,
            ICON_SMALL as WPARAM,
            icons.small as LPARAM,
        );
    }
    if !icons.big.is_null() {
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, icons.big as LPARAM);
    }
    Some(icons)
}

/// An icon of `side` pixels square from rows of straight-alpha BGRA `pixels`; null when it
/// cannot be made. The caller destroys it.
pub fn icon(pixels: &[u8], side: u32) -> HICON {
    if pixels.len() != (side * side * 4) as usize {
        return std::ptr::null_mut();
    }
    // SAFETY: a DIB section of the given size, filled from `pixels`, and a mask bitmap, both
    // deleted once the icon has copied them.
    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side as i32,
            // Negative: the rows come top first.
            biHeight: -(side as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            if !color.is_null() {
                DeleteObject(color);
            }
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        let mask = CreateBitmap(side as i32, side as i32, 1, 1, std::ptr::null());
        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info);
        DeleteObject(color);
        if !mask.is_null() {
            DeleteObject(mask);
        }
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr7_the_title_bar_draws_the_foreground_when_there_is_one() {
        let adaptive = LiveIcon {
            icon_png: vec![1],
            caption_icon_png: vec![2],
        };
        assert_eq!(adaptive.caption_png(), &[2]);
        let plain = LiveIcon {
            icon_png: vec![1],
            caption_icon_png: Vec::new(),
        };
        assert_eq!(plain.caption_png(), &[1]);
    }

    #[test]
    fn fr7_a_window_icon_grows_with_its_monitors_scale() {
        let (small, big) = icon_sides(96);
        let (small_2x, big_2x) = icon_sides(192);
        assert!(small >= 16 && big >= 32);
        assert_eq!((small_2x, big_2x), (small * 2, big * 2));
    }

    #[test]
    fn fr7_bytes_that_are_no_png_never_replace_the_icon() {
        assert!(!set(vec![1, 2, 3], Vec::new()));
    }
}

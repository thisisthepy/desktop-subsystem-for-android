//! What a task window shows before its app's first frame: the app's icon centred on the
//! splash background of the user's theme. The guest's own splash screen, in the app's colours,
//! replaces it with the first frame. The drawing itself is `dsa_host_core::compositor::splash`.

use dsa_host_core::compositor::splash;
use windows_sys::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, LoadImageW, ICONINFO, IMAGE_ICON, LR_DEFAULTCOLOR,
};

use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, LoadResource, LockResource, SizeofResource,
};

use crate::compositor::present::SplashImage;
use dsa_exe_resources::{RT_RCDATA, SPLASH_ICON_ID};

/// The size the program's icon is read at when it carries no larger image of it.
const ICON_EDGE: u32 = 256;

/// The largest icon image the splash decodes, in pixels on a side: far above the 512 pixels
/// the guest draws, and small enough that a broken resource cannot take much memory.
const MAX_SPLASH_SOURCE: u32 = 2048;

/// The splash of a window of this program on a monitor of `dpi` dots per inch.
///
/// The icon is the app's as the guest drew it, at its full size (512 pixels), which the
/// program carries beside its 256 pixel icon; the splash scales it down to the size it has on
/// the window's monitor each time it is drawn, so a move to another monitor draws it again
/// from the same pixels. A program made before it carried that image shows its 256 pixel icon,
/// never scaled up.
pub fn image(icon_id: usize, dpi: u32) -> SplashImage {
    let dark = crate::compositor::system_settings::read().dark_mode;
    SplashImage {
        background: splash::background(dark),
        icon: full_icon().or_else(|| icon_pixels(icon_id, ICON_EDGE)),
        dpi,
    }
}

/// The app's icon PNG this program carries for its splash, decoded at its own size, as rows of
/// premultiplied BGRA pixels. `None` when it carries none or it cannot be read.
fn full_icon() -> Option<(Vec<u8>, (u32, u32))> {
    let png = splash_png()?;
    let (width, height) = crate::compositor::widgets::png::size(png)?;
    if width > MAX_SPLASH_SOURCE || height > MAX_SPLASH_SOURCE {
        return None;
    }
    let pixels = crate::compositor::widgets::png::with_com(|| {
        let decoder = crate::compositor::widgets::png::Decoder::new().ok()?;
        decoder.decode(png, width, height).ok()
    })?;
    Some((pixels, (width, height)))
}

/// The bytes of this program's splash icon resource.
fn splash_png() -> Option<&'static [u8]> {
    // SAFETY: integer resource IDs in this program's own module, which stays loaded while it
    // runs, so the locked resource lives as long as the program.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let found = FindResourceW(
            module,
            SPLASH_ICON_ID as usize as *const u16,
            RT_RCDATA as usize as *const u16,
        );
        if found.is_null() {
            return None;
        }
        let size = SizeofResource(module, found) as usize;
        let loaded = LoadResource(module, found);
        if loaded.is_null() || size == 0 {
            return None;
        }
        let data = LockResource(loaded) as *const u8;
        if data.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(data, size))
    }
}

/// The program's icon `icon_id` (the app's, in an app's copy of the window host), `edge`
/// pixels square, as rows of premultiplied BGRA pixels. `None` when it cannot be read.
pub(super) fn icon_pixels(icon_id: usize, edge: u32) -> Option<(Vec<u8>, (u32, u32))> {
    // SAFETY: plain Win32 calls; every handle made here is released here, and GetDIBits
    // writes at most `edge` rows of `edge` 32-bit pixels into a buffer of that size.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let icon = LoadImageW(
            module,
            icon_id as *const u16,
            IMAGE_ICON,
            edge as i32,
            edge as i32,
            LR_DEFAULTCOLOR,
        );
        if icon.is_null() {
            return None;
        }
        let mut info: ICONINFO = std::mem::zeroed();
        let got_info = GetIconInfo(icon, &mut info) != 0;
        let mut pixels = vec![0u8; (edge * edge * 4) as usize];
        let mut read = false;
        if got_info && !info.hbmColor.is_null() {
            let mut header: BITMAPINFO = std::mem::zeroed();
            header.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: edge as i32,
                // Negative: the rows come top first.
                biHeight: -(edge as i32),
                biPlanes: 1,
                biBitCount: 32,
                ..std::mem::zeroed()
            };
            let dc = GetDC(std::ptr::null_mut());
            let lines = GetDIBits(
                dc,
                info.hbmColor,
                0,
                edge,
                pixels.as_mut_ptr() as *mut std::ffi::c_void,
                &mut header,
                DIB_RGB_COLORS,
            );
            read = lines == edge as i32;
            ReleaseDC(std::ptr::null_mut(), dc);
        }
        if got_info {
            if !info.hbmColor.is_null() {
                DeleteObject(info.hbmColor);
            }
            if !info.hbmMask.is_null() {
                DeleteObject(info.hbmMask);
            }
        }
        DestroyIcon(icon);
        if !read {
            return None;
        }
        premultiply(&mut pixels);
        Some((pixels, (edge, edge)))
    }
}

/// An icon's colour bitmap has straight alpha, or none at all in an icon without an alpha
/// channel, which is then opaque.
fn premultiply(pixels: &mut [u8]) {
    if pixels.chunks(4).all(|p| p[3] == 0) {
        for p in pixels.chunks_mut(4) {
            p[3] = 0xFF;
        }
        return;
    }
    for p in pixels.chunks_mut(4) {
        let alpha = u32::from(p[3]);
        for channel in &mut p[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr3_the_icons_colours_are_premultiplied_for_the_splash() {
        let mut pixels = vec![0xFF, 0x80, 0x00, 0x80];
        premultiply(&mut pixels);
        assert_eq!(pixels, vec![0x80, 0x40, 0x00, 0x80]);
    }

    #[test]
    fn fr3_an_icon_without_alpha_is_opaque() {
        let mut pixels = vec![0x10, 0x20, 0x30, 0x00, 0x40, 0x50, 0x60, 0x00];
        premultiply(&mut pixels);
        assert_eq!(pixels, vec![0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0xFF]);
    }
}

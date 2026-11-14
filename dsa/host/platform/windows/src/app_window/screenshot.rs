//! The system menu's Screenshot: the window's app content, without the title bar.
//!
//! The window host reads back the frame its window shows
//! ([`WindowPresenter::capture`]), cuts off the title bar's band over its top (a window that
//! fills its monitor has none), puts the picture on the clipboard and saves it as a PNG in the
//! user's Screenshots folder (Pictures\Screenshots), where Windows' own screenshots go. That
//! is where a Windows user looks for a screenshot, and the clipboard lets it be pasted at once,
//! as after the Snipping Tool.
//!
//! The file is named as the Snipping Tool names its own: "Screenshot 2026-11-08 143022.png",
//! the date and the local time to the second, in the user's display language ("스크린샷 ..."
//! in Korean). A second screenshot in the same second gets " (2)" before the extension, the
//! next " (3)", and so on, so no file is ever written over.
//!
//! The work runs on a thread of its own: reading the frame back and encoding it take a moment,
//! and the window keeps answering meanwhile.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::compositor::log::{note, CompositorLogger};
use crate::compositor::present::{CapturedFrame, WindowPresenter};

/// A screenshot: `width` by `height` opaque pixels as RGBA rows, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The app's content of `frame`: the rows under the title bar's band, every pixel opaque.
/// `None` when nothing is left under the band.
pub fn app_content(frame: &CapturedFrame) -> Option<Shot> {
    let row = frame.width as usize * 4;
    let band = frame.band.min(frame.height);
    let height = frame.height - band;
    if frame.width == 0 || height == 0 || frame.rgba.len() < row * frame.height as usize {
        return None;
    }
    let start = band as usize * row;
    let mut rgba = frame.rgba[start..start + row * height as usize].to_vec();
    // The swapchain shows a frame without its alpha, so a screenshot is what the user saw.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = 0xFF;
    }
    Some(Shot {
        width: frame.width,
        height,
        rgba,
    })
}

/// A local date and time, to the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

/// The file name of the `copy`th screenshot taken at `time` (1 for the first), starting with
/// `prefix`: "Screenshot 2026-11-08 143022.png", then "Screenshot 2026-11-08 143022 (2).png".
pub fn file_name(prefix: &str, time: &LocalTime, copy: u32) -> String {
    let stem = format!(
        "{prefix} {:04}-{:02}-{:02} {:02}{:02}{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    );
    if copy <= 1 {
        format!("{stem}.png")
    } else {
        format!("{stem} ({copy}).png")
    }
}

/// The first path in `folder` named for a screenshot at `time` that `exists` says is free.
pub fn free_path(
    folder: &Path,
    prefix: &str,
    time: &LocalTime,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let mut copy = 1;
    loop {
        let path = folder.join(file_name(prefix, time, copy));
        if !exists(&path) || copy >= 10_000 {
            return path;
        }
        copy += 1;
    }
}

/// `shot` as a packed device-independent bitmap (`CF_DIB`): a 40 byte BITMAPINFOHEADER for 32
/// bits a pixel, uncompressed, then the rows bottom first, each pixel blue, green, red and an
/// unused byte, as every program that pastes a bitmap reads it.
pub fn dib(shot: &Shot) -> Vec<u8> {
    let image = shot.width as usize * shot.height as usize * 4;
    let mut out = Vec::with_capacity(40 + image);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(shot.width as i32).to_le_bytes());
    // A positive height: the rows go bottom first.
    out.extend_from_slice(&(shot.height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    // BI_RGB.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image as u32).to_le_bytes());
    // No resolution and no color table.
    out.extend_from_slice(&[0u8; 16]);
    let row = shot.width as usize * 4;
    for y in (0..shot.height as usize).rev() {
        for pixel in shot.rgba[y * row..(y + 1) * row].chunks_exact(4) {
            out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xFF]);
        }
    }
    out
}

/// `shot`'s pixels as blue, green and red, top row first: what the PNG encoder is given.
pub fn bgr(shot: &Shot) -> Vec<u8> {
    shot.rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect()
}

/// Takes a screenshot of the window `hwnd` shows through `presenter`, on a thread of its own.
/// `prefix` starts the file's name. What went wrong goes to the log.
pub fn take(
    presenter: Arc<WindowPresenter>,
    hwnd: isize,
    prefix: &'static str,
    logger: Option<Arc<CompositorLogger>>,
) {
    let spawned = std::thread::Builder::new()
        .name("dsa-app-window-screenshot".into())
        .spawn(move || {
            if let Err(why) = shoot(&presenter, hwnd, prefix) {
                note(
                    logger.as_deref(),
                    &format!("the screenshot was not taken: {why}"),
                );
            }
        });
    if let Err(e) = spawned {
        // The logger moved into the closure that never ran; nothing else is left to tell.
        eprintln!("could not start the screenshot: {e}");
    }
}

fn shoot(presenter: &WindowPresenter, hwnd: isize, prefix: &str) -> Result<(), String> {
    let frame = presenter
        .capture()?
        .ok_or("the app has not drawn its window yet")?;
    let shot = app_content(&frame).ok_or("the window shows nothing under its title bar")?;
    let png = crate::compositor::widgets::png::with_com(|| encode_png(&shot))?;
    // The clipboard first: a folder that cannot be written still leaves the picture to paste.
    let copied = put_on_clipboard(hwnd, &shot, &png);
    let saved = save(&png, prefix);
    copied.and(saved)
}

/// The PNG of `shot`, made with the Windows Imaging Component.
fn encode_png(shot: &Shot) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR,
        IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, STREAM_SEEK_CUR};
    let pixels = bgr(shot);
    let stride = shot.width * 3;
    let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
    // Room for the largest PNG of the picture: deflate never grows data by more than a few
    // bytes a block, and each row adds one filter byte.
    let mut out = vec![0u8; pixels.len() + shot.height as usize + 64 * 1024];
    // SAFETY: plain Windows Imaging Component calls; `out` outlives the stream.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| fail("the imaging component is not there", e))?;
        let stream = factory
            .CreateStream()
            .map_err(|e| fail("no stream for the screenshot", e))?;
        stream
            .InitializeFromMemory(&out)
            .map_err(|e| fail("no stream for the screenshot", e))?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .map_err(|e| fail("no PNG encoder", e))?;
        encoder
            .Initialize(&stream, WICBitmapEncoderNoCache)
            .map_err(|e| fail("the PNG encoder did not start", e))?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        let mut options: Option<IPropertyBag2> = None;
        encoder
            .CreateNewFrame(&mut frame, &mut options)
            .map_err(|e| fail("the PNG encoder made no frame", e))?;
        let frame = frame.ok_or("the PNG encoder made no frame")?;
        frame
            .Initialize(options.as_ref())
            .map_err(|e| fail("the PNG frame did not start", e))?;
        frame
            .SetSize(shot.width, shot.height)
            .map_err(|e| fail("the PNG frame took no size", e))?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame
            .SetPixelFormat(&mut format)
            .map_err(|e| fail("the PNG frame took no format", e))?;
        frame
            .WritePixels(shot.height, stride, &pixels)
            .map_err(|e| fail("the screenshot was not encoded", e))?;
        frame
            .Commit()
            .map_err(|e| fail("the screenshot was not encoded", e))?;
        encoder
            .Commit()
            .map_err(|e| fail("the screenshot was not encoded", e))?;
        let mut written = 0u64;
        stream
            .Seek(0, STREAM_SEEK_CUR, Some(&mut written))
            .map_err(|e| fail("the screenshot's length is unknown", e))?;
        drop(stream);
        out.truncate(written as usize);
    }
    Ok(out)
}

/// The clipboard format of a packed device-independent bitmap.
const CF_DIB: u32 = 8;

/// Puts `shot` on the clipboard, owned by the window `hwnd`: as a bitmap, which every program
/// pastes, and as a PNG, which keeps it exact in the programs that take one.
fn put_on_clipboard(hwnd: isize, shot: &Shot, png: &[u8]) -> Result<(), String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
    };
    let mut opened = false;
    for _ in 0..20 {
        // SAFETY: opening the clipboard for a window of this process.
        if unsafe { OpenClipboard(hwnd as _) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !opened {
        return Err("another program holds the clipboard".into());
    }
    let name: Vec<u16> = "PNG".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the clipboard is open; the name lives through the call.
    let result = unsafe {
        EmptyClipboard();
        let bitmap = set_data(CF_DIB, &dib(shot));
        let format = RegisterClipboardFormatW(name.as_ptr());
        if format != 0 {
            // A program that takes no PNG still has the bitmap.
            let _ = set_data(format, png);
        }
        bitmap
    };
    // SAFETY: closes what was opened above.
    unsafe { CloseClipboard() };
    result
}

/// Puts `bytes` on the open clipboard as `format`.
///
/// # Safety
///
/// The clipboard is open and was emptied by this process.
unsafe fn set_data(format: u32, bytes: &[u8]) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::SetClipboardData;
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
    if memory.is_null() {
        return Err("no memory for the screenshot on the clipboard".into());
    }
    let data = GlobalLock(memory) as *mut u8;
    if data.is_null() {
        GlobalFree(memory);
        return Err("could not fill the clipboard's screenshot".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
    GlobalUnlock(memory);
    // The clipboard owns the memory once it takes it; it is freed here only if not.
    if SetClipboardData(format, memory).is_null() {
        let why = std::io::Error::last_os_error();
        GlobalFree(memory);
        return Err(format!("the clipboard did not take the screenshot: {why}"));
    }
    Ok(())
}

/// Saves `png` in the user's Screenshots folder under a name no file has yet.
fn save(png: &[u8], prefix: &str) -> Result<(), String> {
    use std::io::Write;
    let folder = screenshots_folder()?;
    let time = local_time();
    // A file made between the look and the write is never written over: the write makes a
    // new file or fails, and the next name is tried.
    for _ in 0..3 {
        let path = free_path(&folder, prefix, &time, Path::exists);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                return file
                    .write_all(png)
                    .map_err(|e| format!("could not write {}: {e}", path.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not make {}: {e}", path.display())),
        }
    }
    Err(format!(
        "every screenshot name in {} is taken",
        folder.display()
    ))
}

/// The user's Screenshots folder, made when it is missing: the known folder Windows keeps for
/// screenshots, else Pictures\Screenshots.
fn screenshots_folder() -> Result<PathBuf, String> {
    use windows_sys::Win32::UI::Shell::{FOLDERID_Pictures, FOLDERID_Screenshots, KF_FLAG_CREATE};
    if let Some(folder) = known_folder(&FOLDERID_Screenshots, KF_FLAG_CREATE) {
        return Ok(folder);
    }
    let pictures = known_folder(&FOLDERID_Pictures, KF_FLAG_CREATE)
        .ok_or("the user has no Pictures folder")?;
    let folder = pictures.join("Screenshots");
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("could not make {}: {e}", folder.display()))?;
    Ok(folder)
}

/// The path of the known folder `id`, for this process's user.
pub(crate) fn known_folder(id: &windows_sys::core::GUID, flags: i32) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    let mut path = std::ptr::null_mut();
    // SAFETY: a known folder ID, no token (this process's user), and an out pointer the shell
    // fills and the caller frees.
    let result = unsafe { SHGetKnownFolderPath(id, flags as _, std::ptr::null_mut(), &mut path) };
    if result < 0 || path.is_null() {
        if !path.is_null() {
            // SAFETY: the shell's allocation, freed once.
            unsafe { CoTaskMemFree(path.cast()) };
        }
        return None;
    }
    // SAFETY: a NUL-terminated string the shell allocated, read before it is freed.
    let text = unsafe {
        let mut len = 0;
        while *path.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(path, len))
    };
    // SAFETY: the shell's allocation, freed once.
    unsafe { CoTaskMemFree(path.cast()) };
    Some(PathBuf::from(text))
}

/// The local date and time now.
fn local_time() -> LocalTime {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: fills a local the call only writes.
    let now = unsafe {
        let mut now: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut now);
        now
    };
    LocalTime {
        year: now.wYear,
        month: now.wMonth,
        day: now.wDay,
        hour: now.wHour,
        minute: now.wMinute,
        second: now.wSecond,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame `width` wide whose row `y` is all the colour (y, 0, 0) with alpha 0.
    fn frame(width: u32, height: u32, band: u32) -> CapturedFrame {
        let mut rgba = Vec::new();
        for y in 0..height {
            for _ in 0..width {
                rgba.extend_from_slice(&[y as u8, 0, 0, 0]);
            }
        }
        CapturedFrame {
            width,
            height,
            rgba,
            band,
        }
    }

    #[test]
    fn fr22_a_screenshot_leaves_out_the_title_bar() {
        let shot = app_content(&frame(2, 5, 2)).unwrap();
        assert_eq!((shot.width, shot.height), (2, 3));
        // The first row is the one right under the band, and every pixel is opaque.
        assert_eq!(&shot.rgba[..4], &[2, 0, 0, 0xFF]);
        assert_eq!(&shot.rgba[shot.rgba.len() - 4..], &[4, 0, 0, 0xFF]);
        // A window that fills its monitor has no band, so all of it is the app's.
        let whole = app_content(&frame(2, 5, 0)).unwrap();
        assert_eq!(whole.height, 5);
        assert_eq!(&whole.rgba[..4], &[0, 0, 0, 0xFF]);
        // Nothing under the band, or a frame shorter than it says, is no screenshot.
        assert_eq!(app_content(&frame(2, 2, 2)), None);
        assert_eq!(app_content(&frame(2, 2, 9)), None);
        let short = CapturedFrame {
            rgba: vec![0; 4],
            ..frame(2, 2, 0)
        };
        assert_eq!(app_content(&short), None);
    }

    #[test]
    fn fr22_a_screenshot_is_named_as_windows_names_its_own() {
        let time = LocalTime {
            year: 2026,
            month: 11,
            day: 8,
            hour: 9,
            minute: 5,
            second: 3,
        };
        assert_eq!(
            file_name("Screenshot", &time, 1),
            "Screenshot 2026-11-08 090503.png"
        );
        assert_eq!(
            file_name("스크린샷", &time, 2),
            "스크린샷 2026-11-08 090503 (2).png"
        );
        // A name taken goes to the next copy.
        let folder = Path::new("shots");
        let taken = [
            folder.join("Screenshot 2026-11-08 090503.png"),
            folder.join("Screenshot 2026-11-08 090503 (2).png"),
        ];
        assert_eq!(
            free_path(folder, "Screenshot", &time, |path| taken
                .iter()
                .any(|t| t == path)),
            folder.join("Screenshot 2026-11-08 090503 (3).png")
        );
        assert_eq!(free_path(folder, "Screenshot", &time, |_| false), taken[0]);
    }

    #[test]
    fn fr22_a_screenshot_goes_on_the_clipboard_as_a_bitmap() {
        let shot = Shot {
            width: 2,
            height: 2,
            rgba: vec![
                1, 2, 3, 0xFF, 4, 5, 6, 0xFF, // top row
                7, 8, 9, 0xFF, 10, 11, 12, 0xFF, // bottom row
            ],
        };
        let bytes = dib(&shot);
        assert_eq!(bytes.len(), 40 + 16);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[14..16].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 0);
        // The bottom row first, each pixel blue, green, red.
        assert_eq!(&bytes[40..48], &[9, 8, 7, 0xFF, 12, 11, 10, 0xFF]);
        assert_eq!(&bytes[48..56], &[3, 2, 1, 0xFF, 6, 5, 4, 0xFF]);
        assert_eq!(bgr(&shot)[..6], [3, 2, 1, 6, 5, 4]);
    }
}

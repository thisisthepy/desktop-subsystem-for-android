//! The Window menu's Screenshot: the window's app content, without the title bar's band.
//!
//! The window host reads the frame its window shows from the frame's `IOSurface`, cuts off
//! the band over its top (only a window whose app draws its own title bar has one; a standard
//! title bar sits above the content), puts the picture on the pasteboard as a PNG and saves it
//! where the user's own screenshots go: the folder the screenshot tool was set to
//! (`com.apple.screencapture`'s `location`), else the Desktop.
//!
//! The file is named as macOS names its own screenshots: "Screenshot 2026-11-08 at
//! 2.30.22 PM.png" (with the narrow no-break space macOS puts before AM or PM) or, with a
//! 24-hour clock, "Screenshot 2026-11-08 at 14.30.22.png"; in Korean "스크린샷 2026-11-08
//! 오후 2.30.22.png". A second screenshot in the same second gets " (2)" before the
//! extension, the next " (3)", and so on, so no file is ever written over.
//!
//! Encoding and saving run on a thread of their own, so the window keeps answering.

use std::path::{Path, PathBuf};

/// A screenshot: `width` by `height` opaque pixels as RGBA rows, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The app's content of a frame: of the BGRA `pixels` (`stride` bytes a row), the top-left
/// `content` region less its top `band` rows, every pixel opaque. `None` when nothing is left
/// under the band or the pixels are fewer than the region needs.
pub fn app_content(pixels: &[u8], stride: usize, content: (u32, u32), band: u32) -> Option<Shot> {
    let (width, height) = content;
    let row = width as usize * 4;
    let band = band.min(height);
    let rows = height - band;
    if width == 0 || rows == 0 || stride < row {
        return None;
    }
    let needed = (height as usize - 1) * stride + row;
    if pixels.len() < needed {
        return None;
    }
    let mut rgba = Vec::with_capacity(row * rows as usize);
    for y in band as usize..height as usize {
        let start = y * stride;
        for pixel in pixels[start..start + row].chunks_exact(4) {
            // The window shows a frame without its alpha, so the picture is what the user saw.
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xFF]);
        }
    }
    Some(Shot {
        width,
        height: rows,
        rgba,
    })
}

/// The rows of a task's frame that a band of `band` client pixels covers in a window that
/// shows the frame at `frame_scale` (window pixels per task pixels). The frame read back is
/// the task's, in its own pixels, so a task that keeps its density on a display of another
/// scale has fewer or more rows under the band than the band has pixels.
pub fn band_rows(band: u32, frame_scale: (u32, u32)) -> u32 {
    dsa_host_core::compositor::window_sizing::to_task_length(band, frame_scale)
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

/// The file name of the `copy`th screenshot taken at `time` (1 for the first), as macOS
/// names its own in the user's language and clock.
pub fn file_name(korean: bool, twenty_four_hour: bool, time: &LocalTime, copy: u32) -> String {
    let date = format!("{:04}-{:02}-{:02}", time.year, time.month, time.day);
    let (minute, second) = (time.minute, time.second);
    let twelve = match time.hour % 12 {
        0 => 12,
        hour => hour,
    };
    let afternoon = time.hour >= 12;
    let stem = match (korean, twenty_four_hour) {
        (false, true) => format!(
            "Screenshot {date} at {:02}.{minute:02}.{second:02}",
            time.hour
        ),
        (false, false) => format!(
            "Screenshot {date} at {twelve}.{minute:02}.{second:02}\u{202f}{}",
            if afternoon { "PM" } else { "AM" }
        ),
        (true, true) => format!("스크린샷 {date} {:02}.{minute:02}.{second:02}", time.hour),
        (true, false) => format!(
            "스크린샷 {date} {} {twelve}.{minute:02}.{second:02}",
            if afternoon { "오후" } else { "오전" }
        ),
    };
    if copy <= 1 {
        format!("{stem}.png")
    } else {
        format!("{stem} ({copy}).png")
    }
}

/// The first path in `folder` named for a screenshot at `time` that `exists` says is free.
pub fn free_path(
    folder: &Path,
    korean: bool,
    twenty_four_hour: bool,
    time: &LocalTime,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let mut copy = 1;
    loop {
        let path = folder.join(file_name(korean, twenty_four_hour, time, copy));
        if !exists(&path) || copy >= 10_000 {
            return path;
        }
        copy += 1;
    }
}

/// Where screenshots go: the folder the user set for the screenshot tool (`location`, which
/// may start with `~`), when it is a folder (`is_folder`), else the Desktop in `home`.
pub fn screenshot_folder(
    location: Option<&str>,
    home: &Path,
    is_folder: impl Fn(&Path) -> bool,
) -> PathBuf {
    let set = location.map(str::trim).filter(|l| !l.is_empty()).map(|l| {
        if l == "~" {
            home.to_path_buf()
        } else if let Some(rest) = l.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(l)
        }
    });
    match set {
        Some(folder) if folder.has_root() && is_folder(&folder) => folder,
        _ => home.join("Desktop"),
    }
}

#[cfg(target_os = "macos")]
pub use mac::take;

#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};

    use objc2::msg_send;
    use objc2::rc::{autoreleasepool, Allocated, Retained};
    use objc2::runtime::AnyObject;

    use super::{free_path, screenshot_folder, LocalTime, Shot};
    use crate::compositor::title_bar::is_korean;
    use crate::devices::settings::is_24_hour;
    use crate::objc::{bytes_of, class, ns_data, ns_string, on_main, string_of};

    /// `NSBitmapImageFileTypePNG`.
    const FILE_TYPE_PNG: usize = 4;

    /// Saves `shot` and puts it on the pasteboard, on a thread of its own. What went wrong
    /// goes to the log.
    pub fn take(shot: Shot) {
        let spawned = std::thread::Builder::new()
            .name("dsa-app-window-screenshot".into())
            .spawn(move || {
                if let Err(why) = autoreleasepool(|_| shoot(&shot)) {
                    eprintln!("dsa-app-window: the screenshot was not taken: {why}");
                }
            });
        if let Err(e) = spawned {
            eprintln!("dsa-app-window: could not start the screenshot: {e}");
        }
    }

    fn shoot(shot: &Shot) -> Result<(), String> {
        let png = unsafe { encode_png(shot)? };
        // The pasteboard first: a folder that cannot be written still leaves the picture to
        // paste.
        let pasted = png.clone();
        on_main(move || {
            if let Err(why) = put_on_pasteboard(&pasted) {
                eprintln!("dsa-app-window: the screenshot is not on the pasteboard: {why}");
            }
        });
        let (korean, twenty_four_hour, location) = unsafe { preferences() };
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("the home folder is not known")?;
        let folder = screenshot_folder(location.as_deref(), &home, Path::is_dir);
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("could not make {}: {e}", folder.display()))?;
        let path = free_path(&folder, korean, twenty_four_hour, &local_now(), |p| {
            p.exists()
        });
        std::fs::write(&path, &png).map_err(|e| format!("could not save {}: {e}", path.display()))
    }

    /// The user's language, clock and screenshot folder setting.
    unsafe fn preferences() -> (bool, bool, Option<String>) {
        let mut korean = false;
        let mut twenty_four_hour = false;
        if let Ok(locale_class) = class(c"NSLocale") {
            let languages: *mut AnyObject = msg_send![locale_class, preferredLanguages];
            if !languages.is_null() {
                let first: *mut AnyObject = msg_send![languages, firstObject];
                korean = is_korean(&string_of(first));
            }
            let locale: *mut AnyObject = msg_send![locale_class, autoupdatingCurrentLocale];
            if let Ok(formatter) = class(c"NSDateFormatter") {
                let format: *mut AnyObject = msg_send![
                    formatter,
                    dateFormatFromTemplate: &*ns_string("j"),
                    options: 0usize,
                    locale: locale
                ];
                twenty_four_hour = is_24_hour(&string_of(format));
            }
        }
        let mut location = None;
        if let Ok(defaults_class) = class(c"NSUserDefaults") {
            let allocated: Allocated<AnyObject> = msg_send![defaults_class, alloc];
            let suite = ns_string("com.apple.screencapture");
            let defaults: Option<Retained<AnyObject>> =
                msg_send![allocated, initWithSuiteName: &*suite];
            if let Some(defaults) = defaults {
                let value: *mut AnyObject =
                    msg_send![&*defaults, stringForKey: &*ns_string("location")];
                if !value.is_null() {
                    location = Some(string_of(value));
                }
            }
        }
        (korean, twenty_four_hour, location)
    }

    fn local_now() -> LocalTime {
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&now, &mut tm);
            LocalTime {
                year: (tm.tm_year + 1900).clamp(0, 9999) as u16,
                month: (tm.tm_mon + 1).clamp(1, 12) as u16,
                day: tm.tm_mday.clamp(1, 31) as u16,
                hour: tm.tm_hour.clamp(0, 23) as u16,
                minute: tm.tm_min.clamp(0, 59) as u16,
                second: tm.tm_sec.clamp(0, 60) as u16,
            }
        }
    }

    /// The PNG of `shot`, in sRGB, as the guest draws.
    unsafe fn encode_png(shot: &Shot) -> Result<Vec<u8>, String> {
        let rep_class = class(c"NSBitmapImageRep")?;
        let allocated: Allocated<AnyObject> = msg_send![rep_class, alloc];
        let row = shot.width as usize * 4;
        let color_space = ns_string("NSDeviceRGBColorSpace");
        let rep: Option<Retained<AnyObject>> = msg_send![
            allocated,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: shot.width as isize,
            pixelsHigh: shot.height as isize,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*color_space,
            bytesPerRow: row as isize,
            bitsPerPixel: 32isize
        ];
        let rep = rep.ok_or("no picture could be made for the screenshot")?;
        let data: *mut u8 = msg_send![&*rep, bitmapData];
        if data.is_null() || shot.rgba.len() < row * shot.height as usize {
            return Err("the screenshot's picture has no pixels".into());
        }
        std::ptr::copy_nonoverlapping(shot.rgba.as_ptr(), data, row * shot.height as usize);
        let srgb: *mut AnyObject = msg_send![class(c"NSColorSpace")?, sRGBColorSpace];
        let tagged: *mut AnyObject =
            msg_send![&*rep, bitmapImageRepByRetaggingWithColorSpace: srgb];
        let source: *const AnyObject = if tagged.is_null() {
            Retained::as_ptr(&rep)
        } else {
            tagged
        };
        let properties: *mut AnyObject = msg_send![class(c"NSDictionary")?, dictionary];
        let png: *mut AnyObject = msg_send![
            source,
            representationUsingType: FILE_TYPE_PNG,
            properties: properties
        ];
        let png = bytes_of(png);
        if png.is_empty() {
            return Err("the screenshot could not be made a PNG".into());
        }
        Ok(png)
    }

    /// Puts `png` on the general pasteboard as an image. On the main thread.
    fn put_on_pasteboard(png: &[u8]) -> Result<(), String> {
        unsafe {
            let board: *mut AnyObject = msg_send![class(c"NSPasteboard")?, generalPasteboard];
            if board.is_null() {
                return Err("there is no general pasteboard".into());
            }
            let data = ns_data(png)?;
            let _: isize = msg_send![board, clearContents];
            let ok: bool = msg_send![
                board,
                setData: &*data,
                forType: &*ns_string("public.png")
            ];
            if ok {
                Ok(())
            } else {
                Err("the pasteboard did not take the picture".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIME: LocalTime = LocalTime {
        year: 2026,
        month: 11,
        day: 8,
        hour: 14,
        minute: 30,
        second: 2,
    };

    #[test]
    fn fr22_a_screenshot_is_the_app_content_without_the_band() {
        // A 2x3 frame in a surface 3 pixels wide, BGRA, the top row the band.
        let stride = 12;
        let mut pixels = vec![0u8; stride * 3];
        for y in 0..3 {
            for x in 0..2 {
                let at = y * stride + x * 4;
                pixels[at..at + 4].copy_from_slice(&[x as u8, y as u8, 9, 0]);
            }
        }
        let shot = app_content(&pixels, stride, (2, 3), 1).unwrap();
        assert_eq!((shot.width, shot.height), (2, 2));
        // RGBA, opaque, from the row under the band.
        assert_eq!(&shot.rgba[..4], &[9, 1, 0, 0xFF]);
        assert_eq!(&shot.rgba[4..8], &[9, 1, 1, 0xFF]);
        assert_eq!(&shot.rgba[8..12], &[9, 2, 0, 0xFF]);
        // No band: the whole frame.
        assert_eq!(app_content(&pixels, stride, (2, 3), 0).unwrap().height, 3);
        // Nothing under the band, or too few pixels.
        assert_eq!(app_content(&pixels, stride, (2, 3), 3), None);
        assert_eq!(app_content(&pixels[..20], stride, (2, 3), 0), None);
    }

    #[test]
    fn fr22_a_screenshot_of_a_window_at_another_scale_leaves_out_the_bands_rows_of_the_frame() {
        use dsa_host_core::compositor::window_sizing::{density_for_dpi, scale_between};
        // 1:1, the band's pixels are the frame's rows.
        assert_eq!(band_rows(40, (1, 1)), 40);
        // A task kept at 130 dpi (100%) shown at 164 dpi (125%): 40 client pixels of band are
        // 32 rows of the frame.
        let kept = scale_between(density_for_dpi(120), density_for_dpi(96));
        assert_eq!(kept, (82, 65));
        assert_eq!(band_rows(40, kept), 32);
        // A task kept at a Retina display's density shown on a display at 1x: 28 client
        // pixels are 56 rows.
        let retina_on_1x = scale_between(density_for_dpi(96), density_for_dpi(192));
        assert_eq!(band_rows(28, retina_on_1x), 56);
        // And the screenshot cuts those rows: a 2x6 frame with a band of 4 pixels at 2 window
        // pixels per task pixel keeps its bottom 4 rows.
        let stride = 8;
        let pixels = vec![0u8; stride * 6];
        let shot = app_content(&pixels, stride, (2, 6), band_rows(4, (2, 1))).unwrap();
        assert_eq!(shot.height, 4);
    }

    #[test]
    fn fr22_a_screenshot_is_named_as_macos_names_its_own() {
        assert_eq!(
            file_name(false, false, &TIME, 1),
            "Screenshot 2026-11-08 at 2.30.02\u{202f}PM.png"
        );
        assert_eq!(
            file_name(false, true, &TIME, 1),
            "Screenshot 2026-11-08 at 14.30.02.png"
        );
        assert_eq!(
            file_name(true, false, &TIME, 1),
            "스크린샷 2026-11-08 오후 2.30.02.png"
        );
        assert_eq!(
            file_name(true, true, &TIME, 2),
            "스크린샷 2026-11-08 14.30.02 (2).png"
        );
        let midnight = LocalTime { hour: 0, ..TIME };
        assert_eq!(
            file_name(false, false, &midnight, 1),
            "Screenshot 2026-11-08 at 12.30.02\u{202f}AM.png"
        );
    }

    #[test]
    fn fr22_a_second_screenshot_in_the_same_second_writes_over_nothing() {
        let folder = Path::new("/Users/me/Desktop");
        let taken = folder.join(file_name(false, true, &TIME, 1));
        let path = free_path(folder, false, true, &TIME, |p| p == taken);
        assert_eq!(
            path,
            folder.join("Screenshot 2026-11-08 at 14.30.02 (2).png")
        );
    }

    #[test]
    fn fr22_screenshots_go_where_the_users_own_screenshots_go() {
        let home = Path::new("/Users/me");
        let any = |_: &Path| true;
        let none = |_: &Path| false;
        assert_eq!(
            screenshot_folder(None, home, any),
            PathBuf::from("/Users/me/Desktop")
        );
        assert_eq!(
            screenshot_folder(Some("~/Pictures/Screenshots"), home, any),
            PathBuf::from("/Users/me/Pictures/Screenshots")
        );
        assert_eq!(
            screenshot_folder(Some("/Volumes/Shots"), home, any),
            PathBuf::from("/Volumes/Shots")
        );
        // A folder that is gone, or no folder at all, falls back to the Desktop.
        assert_eq!(
            screenshot_folder(Some("/Volumes/Gone"), home, none),
            PathBuf::from("/Users/me/Desktop")
        );
        assert_eq!(
            screenshot_folder(Some("relative"), home, any),
            PathBuf::from("/Users/me/Desktop")
        );
    }
}

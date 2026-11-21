//! The names a screenshot is saved under, the same on every host that names them this way.
//!
//! The file is named as the Snipping Tool names its own: "Screenshot 2026-11-08 143022.png",
//! the date and the local time to the second. A second screenshot in the same second gets
//! " (2)" before the extension, the next " (3)", and so on, so no file is ever written over.
//!
//! The picture itself is cut here too: the app's content of a frame, without the title bar's
//! band, every pixel opaque.

use std::path::{Path, PathBuf};

/// A screenshot: `width` by `height` opaque pixels as RGBA rows, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The order a frame's four bytes a pixel are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelOrder {
    /// Blue, green, red, then a byte that is not alpha (`XRGB8888` or `BGRA` in memory).
    Bgra,
    /// Red, green, blue, then a byte that is not alpha.
    Rgba,
}

/// The app's content of a frame: of `pixels` (`stride` bytes a row, in `order`), the top-left
/// `content` region (width, height) less its top `band` rows, every pixel opaque, because the
/// window shows a frame without its alpha and the picture is what the user saw. `None` when
/// nothing is left under the band or the pixels are fewer than the region needs.
pub fn app_content(
    pixels: &[u8],
    stride: usize,
    content: (u32, u32),
    band: u32,
    order: PixelOrder,
) -> Option<Shot> {
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
            match order {
                PixelOrder::Bgra => rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xFF]),
                PixelOrder::Rgba => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xFF]),
            }
        }
    }
    Some(Shot {
        width,
        height: rows,
        rgba,
    })
}

/// A local date and time, to the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
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

/// Writes `png` into `folder` (which exists) under the first name for a screenshot at `time`
/// that no file has, and returns the path.
///
/// A file made between the look and the write is never written over: the write makes a new
/// file or fails, and the next name is tried.
pub fn save_new(
    folder: &Path,
    prefix: &str,
    time: &LocalTime,
    png: &[u8],
) -> Result<PathBuf, String> {
    use std::io::Write;
    for _ in 0..3 {
        let path = free_path(folder, prefix, time, Path::exists);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                return file
                    .write_all(png)
                    .map(|()| path.clone())
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

#[cfg(test)]
mod save_tests {
    use super::*;

    #[test]
    fn d9_two_screenshots_in_one_second_are_saved_under_two_names() {
        let folder = std::env::temp_dir().join(format!("dsa-shots-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let time = LocalTime {
            year: 2026,
            month: 11,
            day: 8,
            hour: 14,
            minute: 30,
            second: 22,
        };
        let first = save_new(&folder, "Screenshot", &time, b"one").unwrap();
        let second = save_new(&folder, "Screenshot", &time, b"two").unwrap();
        assert_eq!(first, folder.join("Screenshot 2026-11-08 143022.png"));
        assert_eq!(second, folder.join("Screenshot 2026-11-08 143022 (2).png"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

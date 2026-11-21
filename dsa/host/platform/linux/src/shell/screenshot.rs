//! The window menu's Screenshot: the window's app content, without the title bar, on the
//! clipboard (`image/png`) and in the user's Screenshots folder.
//!
//! The folder is `Screenshots` in the user's Pictures folder, as the XDG user directories name
//! it (`xdg-user-dir PICTURES`, else `XDG_PICTURES_DIR` in `user-dirs.dirs`, else
//! `~/Pictures`), which is where GNOME and KDE put their own screenshots. The file is named
//! "Screenshot 2026-11-08 143022.png", the local date and time to the second, with the word
//! in the session's interface language ("스크린샷 ..." in Korean), as the Windows host names
//! its own; a second one in the same second gets
//! " (2)" before the extension, so no file is ever written over.

use std::path::{Path, PathBuf};

use dsa_host_core::screenshot::PixelOrder;

use super::png::Rgba;

/// The app's content of a frame of `XRGB8888` rows (blue, green, red, unused in memory),
/// `width` x `height`: the rows under the title bar's `band`, as opaque RGBA. `None` when
/// nothing is left under the band.
pub fn app_content(pixels: &[u8], width: u32, height: u32, band: u32) -> Option<Rgba> {
    let stride = width as usize * 4;
    let shot = dsa_host_core::screenshot::app_content(
        pixels,
        stride,
        (width, height),
        band,
        PixelOrder::Bgra,
    )?;
    Some(Rgba {
        width: shot.width,
        height: shot.height,
        pixels: shot.rgba,
    })
}

pub use dsa_host_core::screenshot::{file_name, free_path, LocalTime};

/// The Pictures folder `user-dirs.dirs` (its text `dirs`) names for the user whose home is
/// `home`: `XDG_PICTURES_DIR="$HOME/Pictures"`, with `$HOME` at the start replaced. `None`
/// when the file names none.
pub fn pictures_from_user_dirs(dirs: &str, home: &Path) -> Option<PathBuf> {
    dirs.lines().find_map(|line| {
        let value = line.trim().strip_prefix("XDG_PICTURES_DIR=")?;
        let value = value.trim().trim_matches('"');
        if value.is_empty() {
            return None;
        }
        if let Some(rest) = value.strip_prefix("$HOME") {
            let rest = rest.trim_start_matches('/');
            return Some(if rest.is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            });
        }
        value.starts_with('/').then(|| PathBuf::from(value))
    })
}

/// The folder screenshots go to in the Pictures folder `pictures`.
pub fn screenshots_folder(pictures: &Path) -> PathBuf {
    pictures.join("Screenshots")
}

/// The user's Pictures folder: what `xdg-user-dir PICTURES` prints, else what
/// `user-dirs.dirs` names, else `~/Pictures`.
pub fn pictures_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let asked = std::process::Command::new("xdg-user-dir")
        .arg("PICTURES")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|path| path.starts_with('/'));
    if let Some(path) = asked {
        return Some(PathBuf::from(path));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let named = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|dirs| pictures_from_user_dirs(&dirs, &home));
    Some(named.unwrap_or_else(|| home.join("Pictures")))
}

/// The local time now.
#[cfg(unix)]
pub fn local_time() -> LocalTime {
    // SAFETY: `time` reads the clock, and `localtime_r` fills the `tm` given it.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    }
}

/// Saves `png` in the user's Screenshots folder under a name no file has yet, made with
/// `prefix`. Returns the file's path.
#[cfg(unix)]
pub fn save(png: &[u8], prefix: &str) -> Result<PathBuf, String> {
    let pictures = pictures_folder().ok_or("the home folder is not known")?;
    let folder = screenshots_folder(&pictures);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("could not make {}: {e}", folder.display()))?;
    dsa_host_core::screenshot::save_new(&folder, prefix, &local_time(), png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr22_a_screenshot_is_the_app_without_the_title_bar() {
        // Two rows of band, one of app: blue, green, red in memory.
        let pixels = [
            [1, 1, 1, 0],
            [2, 2, 2, 0],
            [3, 3, 3, 0],
            [4, 4, 4, 0],
            [0x10, 0x20, 0x30, 0],
            [0x40, 0x50, 0x60, 0],
        ]
        .concat();
        let shot = app_content(&pixels, 2, 3, 2).unwrap();
        assert_eq!((shot.width, shot.height), (2, 1));
        assert_eq!(
            shot.pixels,
            vec![0x30, 0x20, 0x10, 0xff, 0x60, 0x50, 0x40, 0xff]
        );
        // A window with no title bar (full screen) gives the whole frame.
        assert_eq!(app_content(&pixels, 2, 3, 0).unwrap().height, 3);
        assert_eq!(app_content(&pixels, 2, 3, 3), None);
    }

    #[test]
    fn fr22_the_pictures_folder_comes_from_the_users_dirs_file() {
        let home = Path::new("/home/user");
        let dirs = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_PICTURES_DIR=\"$HOME/사진\"\n";
        assert_eq!(pictures_from_user_dirs(dirs, home), Some(home.join("사진")));
        assert_eq!(
            pictures_from_user_dirs("XDG_PICTURES_DIR=\"/data/pictures\"", home),
            Some(PathBuf::from("/data/pictures"))
        );
        assert_eq!(
            pictures_from_user_dirs("XDG_MUSIC_DIR=\"$HOME/Music\"", home),
            None
        );
        assert_eq!(
            screenshots_folder(&home.join("Pictures")),
            home.join("Pictures/Screenshots")
        );
    }
}

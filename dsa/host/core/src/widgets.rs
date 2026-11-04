//! The widgets of a user's Android, placed on the host's desktop.
//!
//! The guest's agent hosts each widget the user placed and draws it; the host shows each one
//! in a window of its own on the desktop. This module holds what both sides of the host
//! agree on: the widgets and their frames as the host keeps them, the checks a widget's
//! description and frames pass before they go on ([`checked_provider`], [`checked_frame`]),
//! so a bad guest can neither crash the host nor make it hold more than a bounded amount,
//! the sizes in dp a window's pixels come to ([`dp_of`]), and where each user's widget
//! windows are ([`WidgetPlacements`]).
//!
//! A dp (density-independent pixel) is one pixel at Android's density of 160 dots per inch,
//! which the host maps to a scale of 100%. So a widget's size in dp is its window's size in
//! the host's device-independent pixels, and it stays the same on a monitor of another scale.
//!
//! ```text
//! <dir>/users/<user>.widgets.json   {"12": {"x": 64, "y": 64, "width_dp": 180, "height_dp": 110}}
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use dsa_protocol::control;
use serde::{Deserialize, Serialize};

use crate::settings::{read_json, user_file_stem, write_json};

/// The longest provider name the host takes, in bytes. A component name is a package and a
/// class, a few dozen bytes.
pub const MAX_PROVIDER_BYTES: usize = 512;

/// The longest label or package the host keeps, in characters. A longer one is cut.
pub const MAX_LABEL_CHARS: usize = 256;

/// The largest preview the host takes, in bytes of PNG. A larger one is left out and the
/// widget is still listed.
pub const MAX_PREVIEW_BYTES: usize = 48 * 1024;

/// The most widgets a gallery lists. A guest that offers more has the rest left out.
pub const MAX_PROVIDERS: usize = 512;

/// The most widgets the host keeps placed at once.
pub const MAX_WIDGETS: usize = 64;

/// The largest frame the host takes, in bytes of PNG.
pub const MAX_FRAME_BYTES: usize = 512 * 1024;

/// The largest side of a widget, in pixels and in dp.
pub const MAX_SIDE: u32 = 4096;

/// The smallest side of a widget window, in dp, whatever its widget asks for: a window this
/// small can still be grabbed and resized.
pub const MIN_SIDE_DP: u32 = 40;

/// Android's density at a scale of 100%.
pub const BASE_DPI: u32 = 160;

/// Resize mode bits.
pub const RESIZE_HORIZONTAL: u32 = 1;
pub const RESIZE_VERTICAL: u32 = 2;

/// One widget an app in the guest offers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WidgetProvider {
    /// The provider's component, as `package/class`.
    pub provider: String,
    pub package_name: String,
    pub label: String,
    /// A PNG image; empty for none.
    pub preview_png: Vec<u8>,
    pub min_width_dp: u32,
    pub min_height_dp: u32,
    pub default_width_dp: u32,
    pub default_height_dp: u32,
    /// 0 for no limit.
    pub max_width_dp: u32,
    pub max_height_dp: u32,
    /// [`RESIZE_HORIZONTAL`] and [`RESIZE_VERTICAL`] bits.
    pub resize_mode: u32,
    pub needs_configuration: bool,
    /// The name of the app that offers the widget; empty when the guest did not say.
    pub app_label: String,
}

/// Where a widget's window is: its top left corner in the host's physical screen pixels,
/// and its size in dp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width_dp: u32,
    pub height_dp: u32,
}

/// One widget placed on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widget {
    pub widget_id: i32,
    pub provider: WidgetProvider,
    pub placement: Placement,
}

/// What a widget shows: a PNG image of `width` by `height` pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetFrame {
    pub widget_id: i32,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// A change to the placed widgets, as a watcher hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetEvent {
    /// A widget was placed, or described again.
    Placed(Widget),
    /// What a widget shows now.
    Frame(WidgetFrame),
    /// The widget is gone.
    Removed { widget_id: i32 },
    /// Android said which widgets it has, as it does each time it connects: any other is
    /// gone. The service forgets the placements of the others.
    Listed(Vec<i32>),
}

/// Why a request about widgets was not carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetError {
    /// The Android is not running; the detail says so in words a person can act on.
    NotRunning(String),
    /// Android refused, or did not answer; the detail says why.
    Refused(String),
}

impl std::fmt::Display for WidgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WidgetError::NotRunning(why) | WidgetError::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for WidgetError {}

/// The size a widget window asks for: its pixels on its monitor, the monitor's density,
/// and the same size in dp, which the widget's app is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetSize {
    pub width: u32,
    pub height: u32,
    pub density_dpi: u32,
    pub width_dp: u32,
    pub height_dp: u32,
}

impl WidgetSize {
    /// The size of a window of `width` by `height` pixels on a monitor of `density_dpi`
    /// (0 is taken as 160), with each side kept between 1 and [`MAX_SIDE`].
    pub fn of_pixels(width: u32, height: u32, density_dpi: u32) -> WidgetSize {
        let density_dpi = if density_dpi == 0 {
            BASE_DPI
        } else {
            density_dpi.clamp(40, 1280)
        };
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        WidgetSize {
            width,
            height,
            density_dpi,
            width_dp: dp_of(width, density_dpi),
            height_dp: dp_of(height, density_dpi),
        }
    }

    pub fn to_control(self, widget_id: i32) -> control::ResizeWidget {
        control::ResizeWidget {
            widget_id,
            width: self.width,
            height: self.height,
            density_dpi: self.density_dpi,
            width_dp: self.width_dp,
            height_dp: self.height_dp,
        }
    }
}

/// `px` pixels at `density_dpi`, in dp, rounded to the nearest; at least 1 for any `px`
/// above 0.
pub fn dp_of(px: u32, density_dpi: u32) -> u32 {
    if px == 0 {
        return 0;
    }
    let dpi = u64::from(if density_dpi == 0 {
        BASE_DPI
    } else {
        density_dpi
    });
    let dp = (u64::from(px) * u64::from(BASE_DPI) + dpi / 2) / dpi;
    dp.clamp(1, u64::from(u32::MAX)) as u32
}

/// `dp` at `density_dpi`, in pixels, rounded to the nearest.
pub fn px_of(dp: u32, density_dpi: u32) -> u32 {
    let dpi = u64::from(if density_dpi == 0 {
        BASE_DPI
    } else {
        density_dpi
    });
    let px = (u64::from(dp) * dpi + u64::from(BASE_DPI) / 2) / u64::from(BASE_DPI);
    px.min(u64::from(u32::MAX)) as u32
}

/// Whether `name` can be a provider's component: `package/class`, of printable characters,
/// no longer than [`MAX_PROVIDER_BYTES`].
pub fn valid_provider_name(name: &str) -> bool {
    let Some((package, class)) = name.split_once('/') else {
        return false;
    };
    !package.is_empty()
        && !class.is_empty()
        && name.len() <= MAX_PROVIDER_BYTES
        && !class.contains('/')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '$' | '/'))
}

fn cut(text: String) -> String {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    if text.chars().count() <= MAX_LABEL_CHARS {
        text
    } else {
        text.chars().take(MAX_LABEL_CHARS).collect()
    }
}

fn is_png(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG")
}

/// Checks a widget's description from the guest: a provider that is no component is
/// refused; a label too long is cut, a preview too large or not a PNG is left out, and sizes
/// are kept within [`MAX_SIDE`].
pub fn checked_provider(message: control::WidgetProvider) -> Result<WidgetProvider, String> {
    if !valid_provider_name(&message.provider) {
        return Err(format!(
            "the provider {:?} is not a component name",
            message.provider.chars().take(80).collect::<String>()
        ));
    }
    let side = |dp: u32| dp.min(MAX_SIDE);
    let preview_png =
        if message.preview_png.len() <= MAX_PREVIEW_BYTES && is_png(&message.preview_png) {
            message.preview_png
        } else {
            Vec::new()
        };
    Ok(WidgetProvider {
        provider: message.provider,
        package_name: cut(message.package_name),
        label: cut(message.label),
        preview_png,
        min_width_dp: side(message.min_width_dp),
        min_height_dp: side(message.min_height_dp),
        default_width_dp: side(message.default_width_dp),
        default_height_dp: side(message.default_height_dp),
        max_width_dp: side(message.max_width_dp),
        max_height_dp: side(message.max_height_dp),
        resize_mode: message.resize_mode & (RESIZE_HORIZONTAL | RESIZE_VERTICAL),
        needs_configuration: message.needs_configuration,
        app_label: cut(message.app_label),
    })
}

/// Checks a widget's frame from the guest: it is a PNG image within [`MAX_FRAME_BYTES`], of
/// a size from 1 to [`MAX_SIDE`] on each side.
pub fn checked_frame(message: control::WidgetFrame) -> Result<WidgetFrame, String> {
    if message.png.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "a frame of widget {} is {} bytes, more than {MAX_FRAME_BYTES}",
            message.widget_id,
            message.png.len()
        ));
    }
    if !is_png(&message.png) {
        return Err(format!(
            "a frame of widget {} is not a PNG image",
            message.widget_id
        ));
    }
    if !(1..=MAX_SIDE).contains(&message.width) || !(1..=MAX_SIDE).contains(&message.height) {
        return Err(format!(
            "a frame of widget {} is {}x{} pixels",
            message.widget_id, message.width, message.height
        ));
    }
    Ok(WidgetFrame {
        widget_id: message.widget_id,
        width: message.width,
        height: message.height,
        png: message.png,
    })
}

/// The size in dp a widget window may have, from what the user asks for: within the
/// widget's minimum and maximum, at least [`MIN_SIDE_DP`], and the widget's default on a side
/// it cannot be resized along.
pub fn clamp_size(provider: &WidgetProvider, width_dp: u32, height_dp: u32) -> (u32, u32) {
    let side = |asked: u32, default: u32, min: u32, max: u32, resizable: bool| {
        let default = default_side(default, min);
        let asked = if resizable { asked } else { default };
        let lower = min.clamp(MIN_SIDE_DP, MAX_SIDE);
        let upper = if max == 0 {
            MAX_SIDE
        } else {
            max.clamp(lower, MAX_SIDE)
        };
        asked.clamp(lower, upper)
    };
    (
        side(
            width_dp,
            provider.default_width_dp,
            provider.min_width_dp,
            provider.max_width_dp,
            provider.resize_mode & RESIZE_HORIZONTAL != 0,
        ),
        side(
            height_dp,
            provider.default_height_dp,
            provider.min_height_dp,
            provider.max_height_dp,
            provider.resize_mode & RESIZE_VERTICAL != 0,
        ),
    )
}

fn default_side(default: u32, min: u32) -> u32 {
    if default > 0 {
        default
    } else if min > 0 {
        min
    } else {
        110
    }
}

/// Where a new widget's window goes: its widget's default size, near the top left corner of
/// the desktop, stepped past the corners of the windows already placed so none hides
/// another.
pub fn default_placement(provider: &WidgetProvider, placed: &[Placement]) -> Placement {
    let (width_dp, height_dp) = clamp_size(
        provider,
        default_side(provider.default_width_dp, provider.min_width_dp),
        default_side(provider.default_height_dp, provider.min_height_dp),
    );
    let mut x = 64;
    let mut y = 64;
    while placed.iter().any(|p| p.x == x && p.y == y) {
        x += 48;
        y += 48;
        if y > 1024 {
            break;
        }
    }
    Placement {
        x,
        y,
        width_dp,
        height_dp,
    }
}

impl WidgetProvider {
    pub fn to_control(&self) -> control::WidgetProvider {
        control::WidgetProvider {
            provider: self.provider.clone(),
            package_name: self.package_name.clone(),
            label: self.label.clone(),
            preview_png: self.preview_png.clone(),
            min_width_dp: self.min_width_dp,
            min_height_dp: self.min_height_dp,
            default_width_dp: self.default_width_dp,
            default_height_dp: self.default_height_dp,
            max_width_dp: self.max_width_dp,
            max_height_dp: self.max_height_dp,
            resize_mode: self.resize_mode,
            needs_configuration: self.needs_configuration,
            app_label: self.app_label.clone(),
        }
    }
}

/// Where each user's widget windows are, kept by the service.
pub trait WidgetPlacements: Send + Sync {
    /// The placements of `user`, by widget ID.
    fn all(&self, user: &str) -> Result<BTreeMap<i32, Placement>, String>;
    /// Remembers where the widget's window is.
    fn set(&self, user: &str, widget_id: i32, placement: Placement) -> Result<(), String>;
    /// Forgets the widget.
    fn remove(&self, user: &str, widget_id: i32) -> Result<(), String>;
}

/// The placements in a JSON file per user in the service's own folder, beside the user's
/// settings.
pub struct FileWidgetPlacements {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl FileWidgetPlacements {
    /// Keeps the placements in `dir`, the folder the settings are kept in.
    pub fn new(dir: impl Into<PathBuf>) -> FileWidgetPlacements {
        FileWidgetPlacements {
            dir: dir.into(),
            lock: Mutex::new(()),
        }
    }

    fn file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.widgets.json", user_file_stem(user)?)))
    }

    fn read(&self, user: &str) -> Result<BTreeMap<i32, Placement>, String> {
        let stored: Option<BTreeMap<String, Placement>> = read_json(&self.file(user)?)?;
        Ok(stored
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, placement)| id.parse().ok().map(|id| (id, placement)))
            .collect())
    }

    fn write(&self, user: &str, all: &BTreeMap<i32, Placement>) -> Result<(), String> {
        let stored: BTreeMap<String, Placement> = all
            .iter()
            .map(|(id, placement)| (id.to_string(), *placement))
            .collect();
        write_json(&self.file(user)?, &stored)
    }
}

impl WidgetPlacements for FileWidgetPlacements {
    fn all(&self, user: &str) -> Result<BTreeMap<i32, Placement>, String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read(user)
    }

    fn set(&self, user: &str, widget_id: i32, placement: Placement) -> Result<(), String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // A broken file is replaced rather than keeping every placement from being saved.
        let mut all = self.read(user).unwrap_or_default();
        if all.get(&widget_id) == Some(&placement) {
            return Ok(());
        }
        if !all.contains_key(&widget_id) && all.len() >= MAX_WIDGETS {
            return Err(format!("{MAX_WIDGETS} widgets are placed already"));
        }
        all.insert(widget_id, placement);
        self.write(user, &all)
    }

    fn remove(&self, user: &str, widget_id: i32) -> Result<(), String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.read(user).unwrap_or_default();
        if all.remove(&widget_id).is_none() {
            return Ok(());
        }
        self.write(user, &all)
    }
}

/// Placements kept in memory, for tests and for a service that keeps no folder.
#[derive(Default)]
pub struct MemoryWidgetPlacements {
    users: Mutex<HashMap<String, BTreeMap<i32, Placement>>>,
}

impl WidgetPlacements for MemoryWidgetPlacements {
    fn all(&self, user: &str) -> Result<BTreeMap<i32, Placement>, String> {
        Ok(self
            .users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_default())
    }

    fn set(&self, user: &str, widget_id: i32, placement: Placement) -> Result<(), String> {
        let mut users = self.users.lock().unwrap_or_else(|e| e.into_inner());
        let all = users.entry(user.to_string()).or_default();
        if !all.contains_key(&widget_id) && all.len() >= MAX_WIDGETS {
            return Err(format!("{MAX_WIDGETS} widgets are placed already"));
        }
        all.insert(widget_id, placement);
        Ok(())
    }

    fn remove(&self, user: &str, widget_id: i32) -> Result<(), String> {
        if let Some(all) = self
            .users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(user)
        {
            all.remove(&widget_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> control::WidgetProvider {
        control::WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            package_name: "com.example.clock".into(),
            label: "Clock".into(),
            preview_png: b"\x89PNG....".to_vec(),
            min_width_dp: 110,
            min_height_dp: 40,
            default_width_dp: 180,
            default_height_dp: 110,
            max_width_dp: 0,
            max_height_dp: 0,
            resize_mode: 3,
            needs_configuration: false,
            app_label: "Clock App".into(),
        }
    }

    #[test]
    fn fr12_a_window_size_comes_to_dp_at_its_monitors_density() {
        assert_eq!(dp_of(360, 240), 240);
        assert_eq!(dp_of(220, 240), 147);
        assert_eq!(dp_of(180, 160), 180);
        assert_eq!(dp_of(180, 0), 180);
        assert_eq!(dp_of(1, 640), 1);
        assert_eq!(dp_of(0, 240), 0);
        assert_eq!(px_of(180, 240), 270);
        let size = WidgetSize::of_pixels(450, 275, 200);
        assert_eq!((size.width_dp, size.height_dp), (360, 220));
        let size = WidgetSize::of_pixels(0, 99_999, 0);
        assert_eq!(
            (size.width, size.height, size.density_dpi),
            (1, MAX_SIDE, 160)
        );
    }

    #[test]
    fn fr12_a_widget_description_is_checked_and_bounded() {
        let provider = checked_provider(clock()).unwrap();
        assert_eq!(provider.label, "Clock");
        assert_eq!(provider.app_label, "Clock App");
        assert_eq!(provider.preview_png, b"\x89PNG....");
        let big = checked_provider(control::WidgetProvider {
            label: "x".repeat(10_000),
            app_label: "y".repeat(10_000),
            preview_png: [b"\x89PNG".to_vec(), vec![0; MAX_PREVIEW_BYTES]].concat(),
            default_width_dp: u32::MAX,
            resize_mode: 0xff,
            ..clock()
        })
        .unwrap();
        assert_eq!(big.label.chars().count(), MAX_LABEL_CHARS);
        assert_eq!(big.app_label.chars().count(), MAX_LABEL_CHARS);
        assert!(big.preview_png.is_empty());
        assert_eq!(big.default_width_dp, MAX_SIDE);
        assert_eq!(big.resize_mode, 3);
        let gif = checked_provider(control::WidgetProvider {
            preview_png: b"GIF89a".to_vec(),
            ..clock()
        })
        .unwrap();
        assert!(gif.preview_png.is_empty());
    }

    #[test]
    fn nfr5_a_widget_that_names_no_component_is_refused() {
        for bad in ["", "com.example", "/x", "a/", "a/b/c", "a/b c", "a/b\n"] {
            assert!(
                checked_provider(control::WidgetProvider {
                    provider: bad.into(),
                    ..clock()
                })
                .is_err(),
                "{bad:?}"
            );
        }
        assert!(valid_provider_name("com.example/com.example.Clock$Widget"));
        assert!(!valid_provider_name(&format!(
            "a/{}",
            "b".repeat(MAX_PROVIDER_BYTES)
        )));
    }

    #[test]
    fn nfr5_a_frame_that_is_not_a_bounded_png_is_dropped() {
        let frame = |png: Vec<u8>, width, height| control::WidgetFrame {
            widget_id: 1,
            width,
            height,
            png,
        };
        assert!(checked_frame(frame(b"\x89PNG".to_vec(), 2, 1)).is_ok());
        assert!(checked_frame(frame(b"GIF89a".to_vec(), 2, 1)).is_err());
        assert!(checked_frame(frame(Vec::new(), 2, 1)).is_err());
        assert!(checked_frame(frame(b"\x89PNG".to_vec(), 0, 1)).is_err());
        assert!(checked_frame(frame(b"\x89PNG".to_vec(), 2, MAX_SIDE + 1)).is_err());
        let big = [b"\x89PNG".to_vec(), vec![0; MAX_FRAME_BYTES]].concat();
        assert!(checked_frame(frame(big, 2, 1)).is_err());
    }

    #[test]
    fn fr12_a_size_stays_within_what_the_widget_allows() {
        let provider = checked_provider(clock()).unwrap();
        assert_eq!(clamp_size(&provider, 300, 200), (300, 200));
        assert_eq!(clamp_size(&provider, 10, 10), (110, MIN_SIDE_DP));
        let fixed = WidgetProvider {
            resize_mode: RESIZE_VERTICAL,
            max_height_dp: 150,
            ..provider.clone()
        };
        assert_eq!(clamp_size(&fixed, 300, 400), (180, 150));
        let nothing = WidgetProvider::default();
        assert_eq!(clamp_size(&nothing, 500, 500), (110, 110));
    }

    #[test]
    fn fr12_new_widgets_are_placed_apart() {
        let provider = checked_provider(clock()).unwrap();
        let first = default_placement(&provider, &[]);
        assert_eq!(
            first,
            Placement {
                x: 64,
                y: 64,
                width_dp: 180,
                height_dp: 110
            }
        );
        let second = default_placement(&provider, &[first]);
        assert_eq!((second.x, second.y), (112, 112));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-widgets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fr12_placements_are_kept_per_user_across_restarts() {
        let dir = scratch("kept");
        let store = FileWidgetPlacements::new(&dir);
        let here = Placement {
            x: -100,
            y: 40,
            width_dp: 200,
            height_dp: 120,
        };
        store.set("uid:1000", 12, here).unwrap();
        store.set("uid:1000", 15, Placement::default()).unwrap();
        let again = FileWidgetPlacements::new(&dir);
        assert_eq!(again.all("uid:1000").unwrap().get(&12), Some(&here));
        assert!(again.all("uid:1001").unwrap().is_empty());
        again.remove("uid:1000", 12).unwrap();
        let left = FileWidgetPlacements::new(&dir).all("uid:1000").unwrap();
        assert_eq!(left.keys().copied().collect::<Vec<_>>(), vec![15]);
        assert!(store.all("../x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr5_a_broken_placements_file_is_reported_and_then_replaced() {
        let dir = scratch("broken");
        std::fs::create_dir_all(dir.join("users")).unwrap();
        std::fs::write(dir.join("users").join("uid_1000.widgets.json"), "{").unwrap();
        let store = FileWidgetPlacements::new(&dir);
        assert!(store.all("uid:1000").is_err());
        store.set("uid:1000", 3, Placement::default()).unwrap();
        assert_eq!(store.all("uid:1000").unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr5_the_placements_kept_are_bounded() {
        let store = MemoryWidgetPlacements::default();
        for id in 0..MAX_WIDGETS as i32 {
            store.set("u", id, Placement::default()).unwrap();
        }
        assert!(store.set("u", 9999, Placement::default()).is_err());
        assert!(store
            .set(
                "u",
                0,
                Placement {
                    x: 1,
                    ..Placement::default()
                }
            )
            .is_ok());
    }
}

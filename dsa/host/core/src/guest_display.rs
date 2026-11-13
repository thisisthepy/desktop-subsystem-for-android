//! The size of the guest's one display, chosen from the user's monitors when the VM starts.
//!
//! A task cannot be larger than the guest's display, so the display has to be at least as
//! large, in guest pixels, as the largest window the user can make. A task that follows its
//! monitor's density is drawn one guest pixel per window pixel. A task that keeps the main
//! monitor's density (every app starts at it) is drawn at the main monitor's scale wherever
//! its window is, so on a monitor of a lower scale it needs more guest pixels than the monitor
//! has: a 2560 pixel wide monitor at 125% beside a main monitor at 200% holds a window 4096
//! guest pixels wide. So a monitor's size in guest pixels is the larger of its physical size
//! and its physical size times the main monitor's DPI over its own ([`Monitor::guest_side`]).
//!
//! The display is square, as long on each side as the longest such side of any monitor: a
//! monitor turned to portrait while Android runs then still fits, without a restart. What a
//! task's frames cost follows the task's own size, not this one. It is never longer than
//! [`MAX_SIDE`].
//!
//! The display's size cannot change while Android runs. On a monitor larger than the display,
//! whether past [`MAX_SIDE`] or plugged in later, a window the user drags is kept at what the
//! display holds, and a maximised, snapped or full screen window keeps the size the desktop
//! gives it and shows the largest frame Android draws in it, 1:1 and centred, or for a task
//! that keeps its density scaled to fit. That lasts until the next start of Android.
//!
//! The display's density is the main monitor's, as a task's window gives it
//! ([`density_for_dpi`]): 130 dpi at a scale of 100%, 196 at 150%. Every app starts at the
//! display's density. Android relaunches an activity whose density changes unless the app
//! says it handles that itself, so an app that started at another density than its monitor's
//! would be relaunched the moment its window asked for the right one. Starting at the main
//! monitor's density means a window that opens there asks for nothing. SurfaceFlinger takes
//! the density from `ro.sf.lcd_density`, which the guest sets from `androidboot.lcd_density`,
//! and the hwcomposer reports `androidboot.dsa.display_dpi`; both carry the same number.
//!
//! A dp is Android's desktop dp, 1/117.23 inch, and a host DIP 1/96 inch, so at that density
//! a dp is 0.819 DIP. Android scales text separately from everything else, by its font scale.
//! Text follows the same ratio as everything else, so the guest's font scale is the host's
//! text size alone ([`font_scale`]).

use crate::compositor::window_sizing::{density_for_dpi, BASE_DPI};

/// The smallest and the largest text size a host may ask for, in percent of its default.
/// Windows' text size setting goes from 100 to 225; a Linux desktop's text scaling factor may
/// go below 100 and up to 300.
pub const MIN_TEXT_SCALE_PERCENT: u32 = 50;
pub const MAX_TEXT_SCALE_PERCENT: u32 = 300;

/// The guest's font scale for a host text size of `text_scale_percent` (0 is unknown and
/// counts as 100): the text size itself, 1 at 100%, 1.5 at 150%, 2.25 at 225%. Text scales
/// with a dp like everything else, so Android's body text of 16 sp is about 13 DIP, near the
/// host's own body text. Scaling text alone would overflow the rows an app sizes in dp.
pub fn font_scale(text_scale_percent: u32) -> f32 {
    let percent = if text_scale_percent == 0 {
        100
    } else {
        text_scale_percent.clamp(MIN_TEXT_SCALE_PERCENT, MAX_TEXT_SCALE_PERCENT)
    };
    (f64::from(percent) / 100.0) as f32
}

/// The shortest side the display has, whatever the monitors.
pub const MIN_SIDE: u32 = 1920;

/// The longest side the display has: 4K. A longer monitor caps its windows here. A task
/// window's texture and the guest's buffers for a task grow with the task up to this size,
/// so a longer side would cost memory for every window that grows that far.
pub const MAX_SIDE: u32 = 3840;

/// The kernel command line words that set the display's size. Android's `ro.boot` properties
/// are write-once and init reads the command line in order, so the first of each wins.
const WIDTH_KEY: &str = "androidboot.dsa.display_width";
const HEIGHT_KEY: &str = "androidboot.dsa.display_height";

/// The most monitors read from a record, far past any desktop.
const MAX_RECORDED: usize = 64;

/// The kernel command line words that set the display's density: the one the hwcomposer
/// reports, and the one SurfaceFlinger, and so every app, starts at.
const DPI_KEY: &str = "androidboot.dsa.display_dpi";
const LCD_DENSITY_KEY: &str = "androidboot.lcd_density";

/// The lowest and the highest density the guest's hwcomposer accepts.
pub const MIN_DENSITY: u32 = 72;
pub const MAX_DENSITY: u32 = 960;

/// The density Android starts at for a main monitor of `dpi` (96 is 100%): the density a
/// task's window on that monitor gives its task, within what the guest accepts. A main
/// monitor that is not known counts as 100%.
pub fn boot_density(main_monitor_dpi: Option<u32>) -> u32 {
    density_for_dpi(main_monitor_dpi.unwrap_or(BASE_DPI)).clamp(MIN_DENSITY, MAX_DENSITY)
}

/// `cmdline` with the display's density set to `density`, for the hwcomposer and for
/// SurfaceFlinger alike. The image's own values are removed, because an appended value would
/// lose to them.
pub fn with_display_density(cmdline: &str, density: u32) -> String {
    let is_density = |word: &str| {
        [DPI_KEY, LCD_DENSITY_KEY]
            .iter()
            .any(|key| word.strip_prefix(key).is_some_and(|v| v.starts_with('=')))
    };
    let mut words: Vec<String> = cmdline
        .split_whitespace()
        .filter(|word| !is_density(word))
        .map(str::to_string)
        .collect();
    words.push(format!("{DPI_KEY}={density}"));
    words.push(format!("{LCD_DENSITY_KEY}={density}"));
    words.join(" ")
}

/// The most a recorded monitor DPI can be; anything above is not a monitor's.
const MAX_RECORDED_DPI: u32 = 2000;

/// The text a user's session records its main monitor's DPI in (96 is 100%), where a service
/// that cannot see the user's monitors reads it.
pub fn density_record_text(main_monitor_dpi: u32) -> String {
    format!("{main_monitor_dpi}\n")
}

/// The main monitor's DPI in a record, or `None` when it holds no plausible DPI. The user's
/// own session writes it, so anything else is ignored.
pub fn parse_density_record(text: &str) -> Option<u32> {
    let dpi: u32 = text.lines().next()?.trim().parse().ok()?;
    (1..=MAX_RECORDED_DPI).contains(&dpi).then_some(dpi)
}

/// One of the user's monitors, as the user's session records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    /// Its size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// Its DPI (96 is 100%), when known.
    pub dpi: Option<u32>,
    /// Whether it is the main monitor, whose density every app starts at.
    pub main: bool,
}

impl Monitor {
    /// A monitor of `width` by `height` physical pixels whose scale is not known.
    pub fn sized(width: u32, height: u32) -> Monitor {
        Monitor {
            width,
            height,
            dpi: None,
            main: false,
        }
    }

    /// A monitor of `width` by `height` physical pixels at `dpi`.
    pub fn at_dpi(width: u32, height: u32, dpi: u32) -> Monitor {
        Monitor {
            dpi: Some(dpi),
            ..Monitor::sized(width, height)
        }
    }

    /// The same monitor, as the main one.
    pub fn as_main(self) -> Monitor {
        Monitor { main: true, ..self }
    }

    /// Whether it has a size at all.
    fn usable(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// Its longest side in guest pixels when the main monitor is at `main_dpi`: its physical
    /// side, which a task that follows its density fills one to one, or that side times
    /// `main_dpi` over its own DPI, which a task drawn at the main monitor's density fills on
    /// it, whichever is longer. The physical side alone when either DPI is unknown.
    pub fn guest_side(&self, main_dpi: Option<u32>) -> u32 {
        let physical = self.width.max(self.height);
        match (main_dpi, self.dpi) {
            (Some(main), Some(own)) if own > 0 && main > own => {
                let scaled = (u64::from(physical) * u64::from(main)).div_ceil(u64::from(own));
                scaled.min(u64::from(u32::MAX)) as u32
            }
            _ => physical,
        }
    }
}

impl From<(u32, u32)> for Monitor {
    fn from((width, height): (u32, u32)) -> Monitor {
        Monitor::sized(width, height)
    }
}

/// The display for these monitors: a square as long as the longest side any of them has in
/// guest pixels ([`Monitor::guest_side`], at the DPI of the one marked main), within
/// [`MIN_SIDE`] and [`MAX_SIDE`]. `None` when there is no usable monitor among them, which
/// leaves the size the image names.
pub fn display_for(monitors: &[Monitor]) -> Option<(u32, u32)> {
    let main_dpi = monitors
        .iter()
        .find(|monitor| monitor.main && monitor.usable())
        .and_then(|monitor| monitor.dpi);
    let longest = monitors
        .iter()
        .filter(|monitor| monitor.usable())
        .map(|monitor| monitor.guest_side(main_dpi))
        .max()?;
    let side = longest.clamp(MIN_SIDE, MAX_SIDE);
    Some((side, side))
}

/// The display when the monitors may be unknown: [`display_for`] the ones there are, and
/// otherwise the smallest square. The image's own default is a landscape 1920 by 1080, which
/// would cap every window's height at 1080 pixels, so it is never left in place.
pub fn display_or_smallest(monitors: Option<&[Monitor]>) -> (u32, u32) {
    monitors
        .and_then(display_for)
        .unwrap_or((MIN_SIDE, MIN_SIDE))
}

/// `cmdline` with the display set to `size`. An appended value would lose to the image's
/// earlier one, so the image's are removed.
pub fn with_display_size(cmdline: &str, size: (u32, u32)) -> String {
    let is_size = |word: &str| {
        [WIDTH_KEY, HEIGHT_KEY]
            .iter()
            .any(|key| word.strip_prefix(key).is_some_and(|v| v.starts_with('=')))
    };
    let mut words: Vec<String> = cmdline
        .split_whitespace()
        .filter(|word| !is_size(word))
        .map(str::to_string)
        .collect();
    words.push(format!("{WIDTH_KEY}={}", size.0));
    words.push(format!("{HEIGHT_KEY}={}", size.1));
    words.join(" ")
}

/// The word that marks the main monitor in a record.
const MAIN_WORD: &str = "main";

/// The text a user's session records its monitors in, one a line: `<width>x<height>`, then
/// `@<dpi>` when its DPI is known, then ` main` for the main monitor.
pub fn record_text(monitors: &[Monitor]) -> String {
    monitors
        .iter()
        .map(|monitor| {
            let mut line = format!("{}x{}", monitor.width, monitor.height);
            if let Some(dpi) = monitor.dpi {
                line.push_str(&format!("@{dpi}"));
            }
            if monitor.main {
                line.push(' ');
                line.push_str(MAIN_WORD);
            }
            line.push('\n');
            line
        })
        .collect()
}

/// The monitors in a record. The user's own session writes it, so anything that is not a
/// size is skipped, a DPI that no monitor has is unknown, and [`display_for`] bounds what the
/// rest can do. A record written before monitors carried their DPI (`<width>x<height>` alone)
/// gives monitors of unknown scale, which count at their physical size.
pub fn parse_record(text: &str) -> Vec<Monitor> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let size = words.next()?;
            let main = words.any(|word| word == MAIN_WORD);
            let (size, dpi) = match size.split_once('@') {
                Some((size, dpi)) => (
                    size,
                    dpi.parse::<u32>()
                        .ok()
                        .filter(|dpi| (1..=MAX_RECORDED_DPI).contains(dpi)),
                ),
                None => (size, None),
            };
            let (width, height) = size.split_once('x')?;
            Some(Monitor {
                width: width.parse().ok()?,
                height: height.parse().ok()?,
                dpi,
                main,
            })
        })
        .take(MAX_RECORDED)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(sizes: &[(u32, u32)]) -> Vec<Monitor> {
        sizes.iter().copied().map(Monitor::from).collect()
    }

    #[test]
    fn fr4_the_display_holds_the_largest_monitor_in_both_directions() {
        assert_eq!(display_for(&sized(&[(2880, 1800)])), Some((2880, 2880)));
        assert_eq!(display_for(&sized(&[(1800, 2880)])), Some((2880, 2880)));
        assert_eq!(
            display_for(&sized(&[(1920, 1080), (1440, 2560)])),
            Some((2560, 2560))
        );
    }

    #[test]
    fn fr4_the_display_is_never_below_1920_or_above_3840() {
        assert_eq!(display_for(&sized(&[(1280, 720)])), Some((1920, 1920)));
        assert_eq!(display_for(&sized(&[(7680, 4320)])), Some((3840, 3840)));
    }

    #[test]
    fn fr4_without_known_monitors_the_display_is_still_square_so_height_is_not_capped_at_1080() {
        assert_eq!(display_or_smallest(None), (1920, 1920));
        assert_eq!(display_or_smallest(Some(&[])), (1920, 1920));
        assert_eq!(
            display_or_smallest(Some(&sized(&[(2560, 1440)]))),
            (2560, 2560)
        );
    }

    #[test]
    fn fr4_no_usable_monitor_leaves_the_size_the_image_names() {
        assert_eq!(display_for(&[]), None);
        assert_eq!(display_for(&sized(&[(0, 0), (1920, 0)])), None);
    }

    #[test]
    fn fr4_the_display_is_sized_in_guest_pixels_for_monitors_of_mixed_scales() {
        // A main monitor at 200% and two at 125%: a task drawn at the main monitor's density
        // takes 192 / 120 = 1.6 guest pixels per pixel of a 125% monitor, so a window
        // maximised there needs 4096 guest pixels across. That is past the cap, so the display
        // is as large as it may be, not the main monitor's 2880.
        let monitors = [
            Monitor::at_dpi(2880, 1800, 192).as_main(),
            Monitor::at_dpi(2560, 1440, 120),
            Monitor::at_dpi(2560, 1440, 120),
        ];
        assert_eq!(monitors[1].guest_side(Some(192)), 4096);
        let (side, _) = display_for(&monitors).unwrap();
        assert!(side >= 3840 || side == MAX_SIDE, "{side}");
        assert_eq!(display_for(&monitors), Some((MAX_SIDE, MAX_SIDE)));
        // Below the cap the guest side itself is the display: 1920 at 100% beside a 150%
        // main monitor is 2880 guest pixels.
        assert_eq!(
            display_for(&[
                Monitor::at_dpi(1920, 1080, 144).as_main(),
                Monitor::at_dpi(1920, 1080, 96),
            ]),
            Some((2880, 2880))
        );
        // A monitor at a higher scale than the main one still holds a window that follows its
        // own density one to one, so it keeps its physical size.
        assert_eq!(
            display_for(&[
                Monitor::at_dpi(1920, 1080, 96).as_main(),
                Monitor::at_dpi(3840, 2160, 192),
            ]),
            Some((3840, 3840))
        );
        // Without a main monitor's DPI the scales say nothing, and the physical sizes count.
        assert_eq!(
            display_for(&[
                Monitor::at_dpi(2880, 1800, 192),
                Monitor::at_dpi(2560, 1440, 120),
            ]),
            Some((2880, 2880))
        );
    }

    #[test]
    fn fr4_the_chosen_size_replaces_the_images_because_the_first_value_wins() {
        let cmdline = "init=/init androidboot.dsa.display_width=1920 \
                       androidboot.dsa.display_height=1080 androidboot.dsa.display_dpi=130";
        let out = with_display_size(cmdline, (2880, 2880));
        assert_eq!(
            out,
            "init=/init androidboot.dsa.display_dpi=130 \
             androidboot.dsa.display_width=2880 androidboot.dsa.display_height=2880"
        );
    }

    #[test]
    fn fr4_only_the_size_words_are_replaced() {
        let out = with_display_size("androidboot.dsa.display_width_x=1 a=b", (1920, 1920));
        assert!(out.starts_with("androidboot.dsa.display_width_x=1 a=b "));
    }

    #[test]
    fn fr4_android_starts_at_the_main_monitors_density() {
        // Android's desktop dp is 1/117.23 inch and a DIP 1/96 inch: Android draws at 131 x
        // the scale, rounded to an even density.
        assert_eq!(boot_density(Some(96)), 130);
        assert_eq!(boot_density(Some(120)), 164);
        assert_eq!(boot_density(Some(144)), 196);
        assert_eq!(boot_density(Some(168)), 228);
        assert_eq!(boot_density(Some(192)), 262);
        assert_eq!(boot_density(Some(240)), 328);
        assert_eq!(boot_density(None), 130, "an unknown main monitor is 100%");
        assert_eq!(boot_density(Some(1)), MIN_DENSITY);
        assert_eq!(boot_density(Some(2000)), MAX_DENSITY);
        // Text follows the same ratio: at a text size of 100% the font scale is 1.
        assert!((font_scale(100) - 1.0).abs() < 1e-6);
        assert!((font_scale(0) - 1.0).abs() < 1e-6);
        // The image's 130 is replaced, for the hwcomposer and for SurfaceFlinger both, so the
        // first value init reads is the main monitor's.
        let cmdline = "init=/init androidboot.dsa.display_dpi=130 androidboot.lcd_density=130 \
                       androidboot.dsa.display_dpi_x=1";
        assert_eq!(
            with_display_density(cmdline, boot_density(Some(144))),
            "init=/init androidboot.dsa.display_dpi_x=1 androidboot.dsa.display_dpi=196 \
             androidboot.lcd_density=196"
        );
        // A record of the main monitor reads back, and nothing else counts as one.
        assert_eq!(parse_density_record(&density_record_text(144)), Some(144));
        assert_eq!(parse_density_record("junk\n"), None);
        assert_eq!(parse_density_record("0\n"), None);
        assert_eq!(parse_density_record("99999\n"), None);
    }

    #[test]
    fn fr4_the_font_scale_is_the_text_size() {
        for (percent, scale) in [(100, 1.0), (125, 1.25), (150, 1.5), (200, 2.0), (225, 2.25)] {
            assert!(
                (font_scale(percent) - scale).abs() < 1e-5,
                "{percent}%: {}",
                font_scale(percent)
            );
        }
        // What no desktop asks for is kept within what one may.
        assert!((font_scale(1) - font_scale(MIN_TEXT_SCALE_PERCENT)).abs() < 1e-6);
        assert!((font_scale(9999) - font_scale(MAX_TEXT_SCALE_PERCENT)).abs() < 1e-6);
    }

    #[test]
    fn fr4_a_record_of_monitors_reads_back() {
        let monitors = [
            Monitor::at_dpi(2880, 1800, 192).as_main(),
            Monitor::at_dpi(2560, 1440, 120),
            Monitor::sized(1080, 1920),
        ];
        assert_eq!(
            record_text(&monitors),
            "2880x1800@192 main\n2560x1440@120\n1080x1920\n"
        );
        assert_eq!(parse_record(&record_text(&monitors)), monitors);
    }

    #[test]
    fn fr4_a_record_written_before_monitors_carried_their_scale_still_reads() {
        // What 0.2.4-dev.1 and earlier wrote: physical sizes alone.
        let old = "2880x1800\n2560x1440\n2560x1440\n";
        assert_eq!(
            parse_record(old),
            sized(&[(2880, 1800), (2560, 1440), (2560, 1440)])
        );
        // It sizes the display as it always did, until the session writes a new record.
        assert_eq!(display_for(&parse_record(old)), Some((2880, 2880)));
    }

    #[test]
    fn fr4_a_damaged_record_gives_only_what_is_a_size() {
        assert_eq!(
            parse_record("junk\n1920x1080\nx\n-1x5\n99999999999x1\n"),
            sized(&[(1920, 1080)])
        );
        // A DPI no monitor has is unknown, and the size still counts.
        assert_eq!(
            parse_record("1920x1080@0\n1920x1080@99999 main\n1920x1080@x\n"),
            [
                Monitor::sized(1920, 1080),
                Monitor::sized(1920, 1080).as_main(),
                Monitor::sized(1920, 1080),
            ]
        );
    }
}

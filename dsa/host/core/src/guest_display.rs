//! The size of the guest's one display, chosen from the user's monitors when the VM starts.
//!
//! A task window's client area is one guest pixel per pixel, and a task cannot be larger than
//! the guest's display, so the display has to be at least as large as the largest window the
//! user can make. The display is square, as long on each side as the longest side of any
//! monitor: a monitor turned to portrait while Android runs then still fits, without a
//! restart. What a task's frames cost follows the task's own size, not this one.
//!
//! The display's size cannot change while Android runs. A monitor larger than the display
//! that appears later caps its windows at the display's size, shown 1:1 and centred when
//! maximised, until the next start of Android.

/// The shortest side the display has, whatever the monitors.
pub const MIN_SIDE: u32 = 1920;

/// The longest side the display has: 4K. A longer monitor caps its windows here.
pub const MAX_SIDE: u32 = 3840;

/// The kernel command line words that set the display's size. Android's `ro.boot` properties
/// are write-once and init reads the command line in order, so the first of each wins.
const WIDTH_KEY: &str = "androidboot.dsa.display_width";
const HEIGHT_KEY: &str = "androidboot.dsa.display_height";

/// The most monitors read from a record, far past any desktop.
const MAX_RECORDED: usize = 64;

/// The display for these monitors, each a width and height in physical pixels. `None` when
/// there is no usable monitor among them, which leaves the size the image names.
pub fn display_for(monitors: &[(u32, u32)]) -> Option<(u32, u32)> {
    let longest = monitors
        .iter()
        .filter(|(width, height)| *width > 0 && *height > 0)
        .map(|(width, height)| *width.max(height))
        .max()?;
    let side = longest.clamp(MIN_SIDE, MAX_SIDE);
    Some((side, side))
}

/// The display when the monitors may be unknown: [`display_for`] the ones there are, and
/// otherwise the smallest square. The image's own default is a landscape 1920 by 1080, which
/// would cap every window's height at 1080 pixels, so it is never left in place.
pub fn display_or_smallest(monitors: Option<&[(u32, u32)]>) -> (u32, u32) {
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

/// The text a user's session records its monitors in, one `<width>x<height>` a line.
pub fn record_text(monitors: &[(u32, u32)]) -> String {
    monitors
        .iter()
        .map(|(width, height)| format!("{width}x{height}\n"))
        .collect()
}

/// The monitors in a record. The user's own session writes it, so anything that is not a
/// size is skipped, and [`display_for`] bounds what the rest can do.
pub fn parse_record(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|line| {
            let (width, height) = line.trim().split_once('x')?;
            Some((width.parse().ok()?, height.parse().ok()?))
        })
        .take(MAX_RECORDED)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr4_the_display_holds_the_largest_monitor_in_both_directions() {
        assert_eq!(display_for(&[(2880, 1800)]), Some((2880, 2880)));
        assert_eq!(display_for(&[(1800, 2880)]), Some((2880, 2880)));
        assert_eq!(
            display_for(&[(1920, 1080), (1440, 2560)]),
            Some((2560, 2560))
        );
    }

    #[test]
    fn fr4_the_display_is_never_below_1920_or_above_3840() {
        assert_eq!(display_for(&[(1280, 720)]), Some((1920, 1920)));
        assert_eq!(display_for(&[(7680, 4320)]), Some((3840, 3840)));
    }

    #[test]
    fn fr4_without_known_monitors_the_display_is_still_square_so_height_is_not_capped_at_1080() {
        assert_eq!(display_or_smallest(None), (1920, 1920));
        assert_eq!(display_or_smallest(Some(&[])), (1920, 1920));
        assert_eq!(display_or_smallest(Some(&[(2560, 1440)])), (2560, 2560));
    }

    #[test]
    fn fr4_no_usable_monitor_leaves_the_size_the_image_names() {
        assert_eq!(display_for(&[]), None);
        assert_eq!(display_for(&[(0, 0), (1920, 0)]), None);
    }

    #[test]
    fn fr4_the_chosen_size_replaces_the_images_because_the_first_value_wins() {
        let cmdline = "init=/init androidboot.dsa.display_width=1920 \
                       androidboot.dsa.display_height=1080 androidboot.dsa.display_dpi=160";
        let out = with_display_size(cmdline, (2880, 2880));
        assert_eq!(
            out,
            "init=/init androidboot.dsa.display_dpi=160 \
             androidboot.dsa.display_width=2880 androidboot.dsa.display_height=2880"
        );
    }

    #[test]
    fn fr4_only_the_size_words_are_replaced() {
        let out = with_display_size("androidboot.dsa.display_width_x=1 a=b", (1920, 1920));
        assert!(out.starts_with("androidboot.dsa.display_width_x=1 a=b "));
    }

    #[test]
    fn fr4_a_record_of_monitors_reads_back() {
        let monitors = [(2880, 1800), (1080, 1920)];
        assert_eq!(parse_record(&record_text(&monitors)), monitors);
    }

    #[test]
    fn fr4_a_damaged_record_gives_only_what_is_a_size() {
        assert_eq!(
            parse_record("junk\n1920x1080\nx\n-1x5\n99999999999x1\n"),
            [(1920, 1080)]
        );
    }
}

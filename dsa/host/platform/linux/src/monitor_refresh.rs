//! The refresh rate of the user's fastest monitor, for the guest display's refresh rate.
//!
//! The service runs as root outside the user's graphical session and cannot ask the desktop
//! about its monitors. The window manager, which runs in that session, hears each output's
//! current mode from the Wayland compositor, and records the fastest one's rate in the user's
//! `tmp` folder whenever it changes ([`RefreshRecorder`]). The service reads it every few
//! seconds while the user's Android runs ([`read_record`]) and hands it to the session, which
//! with the power saver profile sets the rate the guest's display refreshes at. Without a
//! record the guest keeps the rate it boots with.
//!
//! An X11 session without Wayland has no Wayland outputs to hear. There the user's device
//! lender (`dsa-devices`), which runs in every session, reads the current mode of each output
//! the X screen shows through RandR and records the fastest the same way ([`follow_x11`]).

use std::path::{Path, PathBuf};

use dsa_host_core::guest_refresh;

/// The record's name in the user's `tmp` folder.
const RECORD_NAME: &str = "monitor-refresh.txt";

/// More than any real record.
const MAX_RECORD_BYTES: usize = 64;

/// Where the record is, in the user's `tmp` folder `tmp`.
pub fn record_path(tmp: &Path) -> PathBuf {
    tmp.join(RECORD_NAME)
}

/// The rate recorded in `tmp`, in hertz, when there is a record with a rate in it.
pub fn read_record(tmp: &Path) -> Option<u32> {
    let bytes = std::fs::read(record_path(tmp)).ok()?;
    let text = std::str::from_utf8(bytes.get(..MAX_RECORD_BYTES.min(bytes.len()))?).ok()?;
    guest_refresh::parse_record(text)
}

/// Writes the record whole, so the service never reads half of one.
pub fn write_record(tmp: &Path, hz: u32) -> std::io::Result<()> {
    std::fs::create_dir_all(tmp)?;
    let path = record_path(tmp);
    let partial = path.with_extension("txt.new");
    std::fs::write(&partial, guest_refresh::record_text(hz))?;
    std::fs::rename(&partial, &path)
}

/// A Wayland output mode's refresh rate, which `wl_output` gives in millihertz, in whole
/// hertz: 59940 is 60. Zero or less is a rate the compositor does not know.
pub fn hz_from_millihertz(refresh: i32) -> u32 {
    if refresh <= 0 {
        return 0;
    }
    (refresh as u32).saturating_add(500) / 1000
}

/// Keeps the record up to date from the window manager's outputs.
pub struct RefreshRecorder {
    tmp: Option<PathBuf>,
    last: Option<u32>,
}

impl RefreshRecorder {
    /// Records in `tmp`, the user's `tmp` folder; with none, records nothing.
    pub fn new(tmp: Option<PathBuf>) -> RefreshRecorder {
        RefreshRecorder { tmp, last: None }
    }

    /// Records the fastest of `rates_mhz`, each output's current rate in millihertz, if it
    /// changed. Outputs whose rate is unknown leave the last record. A failure is returned
    /// once, not on every call.
    pub fn record(&mut self, rates_mhz: impl IntoIterator<Item = i32>) -> Result<(), String> {
        let Some(hz) = guest_refresh::fastest(rates_mhz.into_iter().map(hz_from_millihertz)) else {
            return Ok(());
        };
        if self.last == Some(hz) {
            return Ok(());
        }
        self.last = Some(hz);
        match &self.tmp {
            Some(tmp) => write_record(tmp, hz)
                .map_err(|e| format!("recording the monitors' refresh rate: {e}")),
            None => Ok(()),
        }
    }
}

/// An X11 mode's refresh rate from its timings, in millihertz, the unit the record takes: the
/// pixel clock over the pixels in a frame, as `xrandr` computes it. A double-scanned mode draws
/// each line twice and an interlaced one half the lines per field. Zero is a mode whose
/// timings name no rate.
pub fn x11_mode_millihertz(
    dot_clock: u32,
    htotal: u16,
    vtotal: u16,
    double_scan: bool,
    interlace: bool,
) -> i32 {
    // Interlacing halves the lines by doubling the clock's share instead, so an odd line count
    // loses nothing.
    let pixels = u64::from(htotal) * u64::from(vtotal) * if double_scan { 2 } else { 1 };
    if pixels == 0 {
        return 0;
    }
    let clock = u64::from(dot_clock) * 1000 * if interlace { 2 } else { 1 };
    let millihertz = (clock + pixels / 2) / pixels;
    millihertz.min(i32::MAX as u64) as i32
}

/// The current rates, in millihertz, of the outputs an X11 screen shows: for each CRTC that
/// drives an output, the rate of its mode in `modes` (each mode's ID and rate). A CRTC with no
/// mode (0) or no output shows nothing, so it has no rate.
pub fn x11_active_rates(crtcs: &[(u32, usize)], modes: &[(u32, i32)]) -> Vec<i32> {
    crtcs
        .iter()
        .filter(|(mode, outputs)| *mode != 0 && *outputs > 0)
        .filter_map(|(mode, _)| {
            modes
                .iter()
                .find(|(id, _)| id == mode)
                .map(|(_, rate)| *rate)
        })
        .collect()
}

/// Whether the user's session is X11 without Wayland, as its environment says: an X display,
/// and neither a Wayland display nor a session type of `wayland`. In a Wayland session the
/// window manager records the outputs' rates from Wayland, and an X display there is
/// XWayland, whose modes are not the monitors'.
pub fn x11_only_session(
    display: Option<&str>,
    wayland_display: Option<&str>,
    session_type: Option<&str>,
) -> bool {
    let set = |value: Option<&str>| value.is_some_and(|v| !v.trim().is_empty());
    set(display) && !set(wayland_display) && session_type.map(str::trim) != Some("wayland")
}

/// Whether this process runs in an X11 session without Wayland ([`x11_only_session`]).
pub fn in_x11_only_session() -> bool {
    let var = |name: &str| std::env::var(name).ok();
    x11_only_session(
        var("DISPLAY").as_deref(),
        var("WAYLAND_DISPLAY").as_deref(),
        var("XDG_SESSION_TYPE").as_deref(),
    )
}

/// How often [`follow_x11`] looks for RandR's change events.
#[cfg(target_os = "linux")]
const X11_EVENT_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Records the fastest active X11 output's current refresh rate in the user's `tmp` folder,
/// the record the window manager keeps in a Wayland session, until `stop` is set or the X
/// server goes away. It reads the outputs again when RandR says the screen, a CRTC or an
/// output changed, and every [`guest_refresh::MONITOR_POLL`] in case a change goes unsaid.
/// Run it on a thread of its own, in an X11 session only ([`in_x11_only_session`]).
#[cfg(target_os = "linux")]
pub fn follow_x11(
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    log: &dyn Fn(&str),
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use x11rb::connection::Connection;
    use x11rb::protocol::randr::{self, ConnectionExt as _};

    let (connection, screen) =
        x11rb::connect(None).map_err(|e| format!("cannot reach the X display: {e}"))?;
    let root = connection.setup().roots[screen].root;
    let version = connection
        .randr_query_version(1, 3)
        .map_err(|e| e.to_string())
        .and_then(|cookie| cookie.reply().map_err(|e| e.to_string()))
        .map_err(|e| format!("the X display has no RandR extension: {e}"))?;
    if (version.major_version, version.minor_version) < (1, 3) {
        return Err(format!(
            "the X display's RandR is {}.{}; its outputs' rates need 1.3",
            version.major_version, version.minor_version
        ));
    }
    let changes = randr::NotifyMask::SCREEN_CHANGE
        | randr::NotifyMask::CRTC_CHANGE
        | randr::NotifyMask::OUTPUT_CHANGE;
    if let Err(error) = connection.randr_select_input(root, changes) {
        log(&format!(
            "display: RandR does not say when the outputs change, so they are read every {} s: \
             {error}",
            guest_refresh::MONITOR_POLL.as_secs()
        ));
    }
    let _ = connection.flush();

    let read = || -> Result<Vec<i32>, String> {
        let resources = connection
            .randr_get_screen_resources_current(root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let flag = |mode: &randr::ModeInfo, which: randr::ModeFlag| {
            u32::from(mode.mode_flags) & u32::from(which) != 0
        };
        let modes: Vec<(u32, i32)> = resources
            .modes
            .iter()
            .map(|mode| {
                let rate = x11_mode_millihertz(
                    mode.dot_clock,
                    mode.htotal,
                    mode.vtotal,
                    flag(mode, randr::ModeFlag::DOUBLE_SCAN),
                    flag(mode, randr::ModeFlag::INTERLACE),
                );
                (mode.id, rate)
            })
            .collect();
        let mut crtcs = Vec::with_capacity(resources.crtcs.len());
        for crtc in &resources.crtcs {
            let info = connection
                .randr_get_crtc_info(*crtc, resources.config_timestamp)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?;
            crtcs.push((info.mode, info.outputs.len()));
        }
        Ok(x11_active_rates(&crtcs, &modes))
    };

    let mut recorder = RefreshRecorder::new(
        dsa_host_unix::paths::current_user_tmp(dsa_host_unix::paths::Layout::Linux).ok(),
    );
    let mut changed = true;
    let mut read_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if changed || read_at.elapsed() >= guest_refresh::MONITOR_POLL {
            let rates = read().map_err(|e| format!("reading the X display's outputs: {e}"))?;
            if let Err(why) = recorder.record(rates) {
                log(&why);
            }
            changed = false;
            read_at = Instant::now();
        }
        std::thread::sleep(X11_EVENT_POLL);
        while let Some(_event) = connection
            .poll_for_event()
            .map_err(|e| format!("the X display went away: {e}"))?
        {
            changed = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-refresh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fr10_a_wayland_output_rate_in_millihertz_is_whole_hertz() {
        assert_eq!(hz_from_millihertz(59_940), 60);
        assert_eq!(hz_from_millihertz(120_000), 120);
        assert_eq!(hz_from_millihertz(143_856), 144);
        assert_eq!(hz_from_millihertz(0), 0);
        assert_eq!(hz_from_millihertz(-1), 0);
    }

    #[test]
    fn fr10_battery_saver_caps_the_guest_refresh_rate_the_window_manager_recorded() {
        use dsa_host_core::guest_refresh::refresh_rate;
        let dir = empty_dir("record");
        assert_eq!(read_record(&dir), None);
        let mut recorder = RefreshRecorder::new(Some(dir.clone()));
        recorder.record([60_000, 119_880]).unwrap();
        let hz = read_record(&dir).unwrap();
        assert_eq!(hz, 120);
        assert_eq!(refresh_rate(hz, true), 60);
        assert_eq!(refresh_rate(hz, false), 120);
        // Outputs that name no rate leave the record as it was.
        recorder.record([0]).unwrap();
        assert_eq!(read_record(&dir), Some(120));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr10_an_x11_modes_timings_give_its_refresh_rate() {
        // 1920x1080 at 60 Hz (CEA-861): 148.5 MHz over 2200 x 1125.
        assert_eq!(
            x11_mode_millihertz(148_500_000, 2200, 1125, false, false),
            60_000
        );
        // 2560x1440 at 144 Hz, reduced blanking: 586.6 MHz over 2720 x 1497.
        assert_eq!(
            hz_from_millihertz(x11_mode_millihertz(586_586_000, 2720, 1497, false, false)),
            144
        );
        // 59.94 Hz is 60 once in whole hertz.
        assert_eq!(
            hz_from_millihertz(x11_mode_millihertz(148_352_000, 2200, 1125, false, false)),
            60
        );
        // Double scan draws each line twice; interlace half the lines per field.
        assert_eq!(
            x11_mode_millihertz(25_175_000, 800, 525, true, false),
            29_970
        );
        assert_eq!(
            x11_mode_millihertz(74_250_000, 2200, 1125, false, true),
            60_000
        );
        // No timings, no rate.
        assert_eq!(x11_mode_millihertz(148_500_000, 0, 1125, false, false), 0);
        assert_eq!(x11_mode_millihertz(0, 2200, 1125, false, false), 0);
    }

    #[test]
    fn fr10_only_the_x11_outputs_shown_count_and_the_fastest_is_recorded() {
        use dsa_host_core::guest_refresh::refresh_rate;
        let modes = [(10, 60_000), (11, 119_880), (12, 144_000)];
        // A 60 Hz and a 120 Hz monitor shown; a CRTC off (mode 0) and one without an output
        // show nothing, whatever their mode.
        let crtcs = [(10, 1), (11, 1), (0, 0), (12, 0)];
        let rates = x11_active_rates(&crtcs, &modes);
        assert_eq!(rates, vec![60_000, 119_880]);

        let dir = empty_dir("x11");
        let mut recorder = RefreshRecorder::new(Some(dir.clone()));
        recorder.record(rates).unwrap();
        let hz = read_record(&dir).unwrap();
        assert_eq!(hz, 120);
        // The record is what the service reads for the guest, so the saver caps it as in a
        // Wayland session.
        assert_eq!(refresh_rate(hz, true), 60);
        assert_eq!(refresh_rate(hz, false), 120);
        // A mode the screen does not list names no rate, and leaves the record.
        recorder
            .record(x11_active_rates(&[(99, 1)], &modes))
            .unwrap();
        assert_eq!(read_record(&dir), Some(120));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr10_the_x11_path_runs_in_an_x11_session_only() {
        assert!(x11_only_session(Some(":0"), None, Some("x11")));
        assert!(x11_only_session(Some(":0"), Some(""), None));
        // In a Wayland session the X display is XWayland, whose modes are not the monitors'.
        assert!(!x11_only_session(
            Some(":0"),
            Some("wayland-0"),
            Some("wayland")
        ));
        assert!(!x11_only_session(Some(":0"), None, Some("wayland")));
        // Without an X display there is nothing to read.
        assert!(!x11_only_session(None, None, Some("x11")));
        assert!(!x11_only_session(Some(""), None, None));
    }
}

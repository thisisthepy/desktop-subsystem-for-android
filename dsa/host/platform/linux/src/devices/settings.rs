//! The user's desktop settings for their Android: the time zone, the languages, the regional
//! format, the clock, the first day of the week, the dark theme, the accent color and the
//! text size.
//!
//! The text size is GNOME's `text-scaling-factor` through the portal, else KDE's
//! `forceFontDPI` (through the portal, else from its configuration files), else 100%.
//!
//! The time zone comes from systemd's `timedated`, else `/etc/localtime`. The languages and
//! formats come from the session's locale (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LC_TIME`,
//! `LANG`), as every program in the session uses them. The theme and the accent come from
//! the desktop portal's settings (`org.freedesktop.appearance`), which GNOME, KDE and the
//! other desktops fill in, and its change signal says when they change. The clock follows
//! the desktop's own setting where its portal gives it, else the locale's time format.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use dsa_host_core::system_settings::{self, SystemSettings, SystemSettingsSource};
use dsa_transport::Connector;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

use super::dbus::proxy;
use super::Log;

/// The build the follower names itself by.
const BUILD: &str = "dsa-devices settings";

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";

/// Gives the user's Android their desktop settings until `stop` is set.
pub fn follow(mut connector: Box<dyn Connector>, stop: Arc<AtomicBool>, log: Log) {
    let mut source = DesktopSettings::open();
    let log = move |line: &str| log(line);
    system_settings::follow(&mut *connector, BUILD, &mut source, &stop, &log);
}

/// The desktop's settings, read when asked, with word of their changes.
pub struct DesktopSettings {
    session: Option<Connection>,
    changes: Option<Receiver<()>>,
    localtime: Option<SystemTime>,
}

/// Whether the desktop prefers dark windows, and whether its animations are turned off, as
/// the window shown while Android starts asks once when it opens.
pub fn starting_look() -> (bool, bool) {
    let settings = DesktopSettings::open();
    let dark = settings
        .portal_read(APPEARANCE, "color-scheme")
        .and_then(|v| u32::try_from(v).ok())
        == Some(1);
    let animations = settings
        .portal_read("org.gnome.desktop.interface", "enable-animations")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(true);
    (dark, !animations)
}

impl DesktopSettings {
    pub fn open() -> DesktopSettings {
        let session = Connection::session().ok();
        let changes = session.as_ref().and_then(watch_portal);
        DesktopSettings {
            session,
            changes,
            localtime: localtime_changed_at(),
        }
    }

    fn portal_read(&self, namespace: &str, key: &str) -> Option<OwnedValue> {
        let session = self.session.as_ref()?;
        let portal = proxy(session, PORTAL, PORTAL_PATH.into(), SETTINGS).ok()?;
        let value: OwnedValue = portal
            .call("ReadOne", &(namespace, key))
            .or_else(|_| portal.call("Read", &(namespace, key)))
            .ok()?;
        Some(unwrap_variant(value))
    }
}

/// The value inside however many variants the portal wrapped it in (`Read` wraps it twice).
fn unwrap_variant(value: OwnedValue) -> OwnedValue {
    let mut value = value;
    loop {
        let inner = match &*value {
            Value::Value(inner) => inner.try_to_owned().ok(),
            _ => None,
        };
        match inner {
            Some(inner) => value = inner,
            None => return value,
        }
    }
}

/// A thread that hears the portal's change signal, for as long as the process runs.
fn watch_portal(session: &Connection) -> Option<Receiver<()>> {
    let portal = proxy(session, PORTAL, PORTAL_PATH.into(), SETTINGS).ok()?;
    let signals = portal.receive_signal("SettingChanged").ok()?;
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("dsa-settings-watch".into())
        .spawn(move || {
            for _ in signals {
                if tx.send(()).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(rx)
}

fn localtime_changed_at() -> Option<SystemTime> {
    std::fs::symlink_metadata("/etc/localtime")
        .and_then(|m| m.modified())
        .ok()
}

/// A POSIX locale name (`ko_KR.UTF-8@euro`) as a BCP 47 tag (`ko-KR`); none for `C` and
/// `POSIX`, which name no language.
pub fn bcp47(locale: &str) -> Option<String> {
    let name = locale.split(['.', '@']).next().unwrap_or_default().trim();
    if name.is_empty() || name == "C" || name == "POSIX" {
        return None;
    }
    let tag = name.replace('_', "-");
    system_settings::is_language_tag(&tag).then_some(tag)
}

/// The display languages, preferred first, from the session's locale variables (`var`
/// returns one).
pub fn languages(var: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |tag: Option<String>| {
        if let Some(tag) = tag {
            if !out.contains(&tag) {
                out.push(tag);
            }
        }
    };
    // LANGUAGE lists the languages in order, but only counts when LC_ALL, LC_MESSAGES or
    // LANG names a language, as gettext reads it.
    let messages = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| var(name).filter(|v| !v.is_empty()));
    if messages.as_deref().and_then(bcp47).is_some() {
        if let Some(list) = var("LANGUAGE") {
            for entry in list.split(':') {
                push(bcp47(entry));
            }
        }
    }
    push(messages.as_deref().and_then(bcp47));
    out
}

/// The locale dates and numbers are formatted by, when it is not the first language.
pub fn regional_format(var: &dyn Fn(&str) -> Option<String>, first_language: &str) -> String {
    ["LC_ALL", "LC_TIME", "LANG"]
        .iter()
        .find_map(|name| var(name).filter(|v| !v.is_empty()))
        .and_then(|v| bcp47(&v))
        .filter(|tag| tag != first_language)
        .unwrap_or_default()
}

/// Whether a strftime time format shows a 24-hour clock.
pub fn is_24_hour_format(format: &str) -> bool {
    ["%H", "%k", "%T", "%R"].iter().any(|f| format.contains(f))
        && !["%I", "%l", "%p", "%r"].iter().any(|f| format.contains(f))
}

/// The first day of the week (1 Monday through 7 Sunday) from glibc's `first_weekday` and
/// `week-1stday` (a date, usually 19971130, a Sunday).
pub fn first_day(first_weekday: u32, week_start_date: &str) -> u32 {
    if !(1..=7).contains(&first_weekday) {
        return 0;
    }
    // The weekday of the reference date, Monday 1 to Sunday 7.
    let base = match week_start_date.trim() {
        "19971130" => 7,
        "19971201" => 1,
        _ => return 0,
    };
    (base - 1 + first_weekday - 1) % 7 + 1
}

/// The time zone, from `TZ`, timedated or `/etc/localtime`.
fn time_zone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        let name = tz.rsplit_once("zoneinfo/").map_or(tz, |(_, name)| name);
        if system_settings::is_time_zone(name) {
            return name.to_string();
        }
    }
    let timedated = Connection::system().ok().and_then(|system| {
        proxy(
            &system,
            "org.freedesktop.timedate1",
            "/org/freedesktop/timedate1".into(),
            "org.freedesktop.timedate1",
        )
        .ok()?
        .get_property::<String>("Timezone")
        .ok()
    });
    if let Some(name) = timedated.filter(|n| system_settings::is_time_zone(n)) {
        return name;
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| {
            let target = target.to_string_lossy().into_owned();
            target
                .rsplit_once("zoneinfo/")
                .map(|(_, name)| name.to_string())
        })
        .or_else(|| {
            std::fs::read_to_string(Path::new("/etc/timezone"))
                .ok()
                .map(|t| t.trim().to_string())
        })
        .filter(|n| system_settings::is_time_zone(n))
        .unwrap_or_default()
}

/// What `locale` prints for `keywords`, one line each.
fn locale_values(keywords: &[&str]) -> Vec<String> {
    Command::new("locale")
        .args(keywords)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl SystemSettingsSource for DesktopSettings {
    fn read(&mut self) -> SystemSettings {
        let locales = languages(&env);
        let first = locales.first().cloned().unwrap_or_default();
        let time_24_hour = match self
            .portal_read("org.gnome.desktop.interface", "clock-format")
            .and_then(|v| String::try_from(v).ok())
        {
            Some(format) => format == "24h",
            None => locale_values(&["t_fmt"])
                .first()
                .is_some_and(|f| is_24_hour_format(f)),
        };
        let week = locale_values(&["first_weekday", "week-1stday"]);
        let first_day_of_week = match week.as_slice() {
            [first_weekday, date, ..] => first_weekday.parse().map_or(0, |fw| first_day(fw, date)),
            _ => 0,
        };
        let dark_mode = self
            .portal_read(APPEARANCE, "color-scheme")
            .and_then(|v| u32::try_from(v).ok())
            == Some(1);
        let accent_color = self
            .portal_read(APPEARANCE, "accent-color")
            .and_then(|v| accent(&v))
            .unwrap_or(0);
        let text_scale_percent = self
            .portal_read("org.gnome.desktop.interface", "text-scaling-factor")
            .and_then(|v| f64::try_from(v).ok())
            .and_then(text_scale_from_factor)
            .or_else(|| {
                self.portal_read("org.kde.kdeglobals.General", "forceFontDPI")
                    .and_then(|v| i32::try_from(v).ok())
                    .and_then(text_scale_from_font_dpi)
            })
            .or_else(|| {
                let home = env("XDG_CONFIG_HOME")
                    .filter(|v| !v.is_empty())
                    .map(std::path::PathBuf::from)
                    .or_else(|| env("HOME").map(|home| Path::new(&home).join(".config")))?;
                ["kdeglobals", "kcmfonts"].iter().find_map(|file| {
                    let text = std::fs::read_to_string(home.join(file)).ok()?;
                    kde_font_dpi(&text).and_then(text_scale_from_font_dpi)
                })
            })
            .unwrap_or(100);
        SystemSettings {
            time_zone: time_zone(),
            regional_format: regional_format(&env, &first),
            locales,
            time_24_hour,
            first_day_of_week,
            dark_mode,
            accent_color,
            text_scale_percent,
        }
        .checked()
    }

    fn wait_for_change(&mut self, timeout: Duration) -> bool {
        let changed_by_portal = match &self.changes {
            Some(changes) => {
                let got = changes.recv_timeout(timeout).is_ok();
                // Several settings often change at once; one read covers them all.
                while changes.try_recv().is_ok() {}
                got
            }
            None => {
                thread::sleep(timeout);
                false
            }
        };
        let localtime = localtime_changed_at();
        let zone_changed = localtime != self.localtime;
        self.localtime = localtime;
        changed_by_portal || zone_changed
    }
}

/// The portal's accent color, three numbers from 0 to 1, as 0xAARRGGBB; none when it is out
/// of that range, which is how the portal says the user chose none.
fn accent(value: &OwnedValue) -> Option<u32> {
    let Value::Structure(fields) = &**value else {
        return None;
    };
    let channels: Vec<f64> = fields
        .fields()
        .iter()
        .filter_map(|f| match f {
            Value::F64(x) => Some(*x),
            _ => None,
        })
        .collect();
    accent_from_rgb(&channels)
}

/// The text size in percent from GNOME's `text-scaling-factor` (1.0 is the default); none
/// when it is not a size.
pub fn text_scale_from_factor(factor: f64) -> Option<u32> {
    (factor.is_finite() && factor > 0.0).then(|| (factor * 100.0).round() as u32)
}

/// The text size in percent from KDE's `forceFontDPI` (96 is 100%); none when it is not set,
/// which KDE writes as 0.
pub fn text_scale_from_font_dpi(dpi: i32) -> Option<u32> {
    (dpi > 0).then(|| (dpi as u32 * 100 + 48) / 96)
}

/// `forceFontDPI` in the `[General]` group of a KDE configuration file.
pub fn kde_font_dpi(text: &str) -> Option<i32> {
    let mut general = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            general = line == "[General]";
        } else if general {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "forceFontDPI" {
                    return value.trim().parse().ok();
                }
            }
        }
    }
    None
}

/// Red, green and blue from 0 to 1 as 0xAARRGGBB.
pub fn accent_from_rgb(channels: &[f64]) -> Option<u32> {
    let [r, g, b] = <[f64; 3]>::try_from(channels).ok()?;
    if [r, g, b].iter().any(|c| !(0.0..=1.0).contains(c)) {
        return None;
    }
    let byte = |c: f64| (c * 255.0).round() as u32;
    Some(0xff00_0000 | (byte(r) << 16) | (byte(g) << 8) | byte(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn fr30_posix_locales_become_bcp47_tags() {
        assert_eq!(bcp47("ko_KR.UTF-8"), Some("ko-KR".into()));
        assert_eq!(bcp47("de_DE@euro"), Some("de-DE".into()));
        assert_eq!(bcp47("C.UTF-8"), None);
        assert_eq!(bcp47("POSIX"), None);
    }

    #[test]
    fn fr30_the_languages_follow_language_then_lang() {
        let var = vars(&[("LANGUAGE", "ko:en_US"), ("LANG", "ko_KR.UTF-8")]);
        assert_eq!(languages(&var), vec!["ko", "en-US", "ko-KR"]);
        // LANGUAGE does not count under the C locale.
        let var = vars(&[("LANGUAGE", "fr"), ("LANG", "C")]);
        assert!(languages(&var).is_empty());
    }

    #[test]
    fn fr30_a_regional_format_other_than_the_language_is_passed_on() {
        let var = vars(&[("LANG", "en_US.UTF-8"), ("LC_TIME", "en_GB.UTF-8")]);
        assert_eq!(regional_format(&var, "en-US"), "en-GB");
        let var = vars(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(regional_format(&var, "en-US"), "");
    }

    #[test]
    fn fr30_the_clock_and_the_first_day_follow_the_locale() {
        assert!(is_24_hour_format("%H:%M:%S"));
        assert!(is_24_hour_format("%T"));
        assert!(!is_24_hour_format("%I:%M:%S %p"));
        assert!(!is_24_hour_format("%r"));
        assert_eq!(first_day(2, "19971130"), 1);
        assert_eq!(first_day(1, "19971130"), 7);
        assert_eq!(first_day(1, "19971201"), 1);
        assert_eq!(first_day(9, "19971130"), 0);
    }

    #[test]
    fn fr4_the_text_size_follows_the_desktops_text_scaling() {
        assert_eq!(text_scale_from_factor(1.0), Some(100));
        assert_eq!(text_scale_from_factor(1.5), Some(150));
        assert_eq!(text_scale_from_factor(1.25), Some(125));
        assert_eq!(text_scale_from_factor(0.0), None);
        assert_eq!(text_scale_from_factor(f64::NAN), None);
        assert_eq!(text_scale_from_font_dpi(96), Some(100));
        assert_eq!(text_scale_from_font_dpi(144), Some(150));
        assert_eq!(text_scale_from_font_dpi(0), None);
        assert_eq!(
            kde_font_dpi("[KDE]\nforceFontDPI=1\n[General]\nfont=Noto\nforceFontDPI=120\n"),
            Some(120)
        );
        assert_eq!(kde_font_dpi("[General]\nfont=Noto\n"), None);
    }

    #[test]
    fn fr30_the_portals_accent_becomes_argb() {
        assert_eq!(accent_from_rgb(&[0.0, 0.47, 0.83]), Some(0xff00_78d4));
        assert_eq!(accent_from_rgb(&[-1.0, 0.0, 0.0]), None);
        assert_eq!(accent_from_rgb(&[1.0]), None);
    }
}

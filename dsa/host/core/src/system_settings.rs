//! The host's system settings, which each user's Android follows: the time zone, the display
//! languages, the regional format, the app theme (light or dark), the accent color and the
//! text size.
//!
//! The platform reads them in the user's own session, where they belong to the user (on
//! Windows, the window manager reads them and watches for `WM_SETTINGCHANGE`), and hands them
//! to the service over the local API. The service keeps the latest for the user's Android and
//! gives them to its agent each time the agent connects and whenever they change; the agent
//! makes them the guest's. The user does not change them in the guest.
//!
//! What arrives from a client is checked here, so a value the guest could not take never
//! reaches it.
//!
//! Nothing here names a host operating system API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dsa_protocol::control;
use dsa_protocol::host_api::types;
use dsa_transport::Connector;

use crate::guest_display;
use crate::host_api::HostApiClient;

/// How long [`follow`] waits for the service to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The most display languages passed on.
pub const MAX_LOCALES: usize = 16;

/// The longest BCP 47 tag or time zone name passed on, in bytes.
pub const MAX_NAME: usize = 64;

/// A user's host system settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemSettings {
    /// An IANA time zone name, such as `Asia/Seoul`; empty when the host cannot name it.
    pub time_zone: String,
    /// The display languages as BCP 47 tags, preferred first.
    pub locales: Vec<String>,
    /// The locale dates, times and numbers are formatted by; empty when it is the first
    /// language.
    pub regional_format: String,
    /// Whether times are shown on a 24-hour clock.
    pub time_24_hour: bool,
    /// The first day of the week: 1 is Monday through 7, Sunday; 0 when unknown.
    pub first_day_of_week: u32,
    /// Whether apps use the dark theme.
    pub dark_mode: bool,
    /// The accent color as 0xAARRGGBB; 0 when there is none.
    pub accent_color: u32,
    /// The size of text the user asked for, in percent of the default (Windows' text size,
    /// a desktop's text scaling factor); 0 when unknown, which counts as 100. The guest's
    /// font scale follows it ([`guest_display::font_scale`]).
    pub text_scale_percent: u32,
}

/// Whether `tag` reads as a BCP 47 language tag: ASCII letters and digits in parts of one to
/// eight, joined by hyphens, starting with a language of two or three letters.
pub fn is_language_tag(tag: &str) -> bool {
    let parts: Vec<&str> = tag.split('-').collect();
    let first = parts[0];
    tag.len() <= MAX_NAME
        && (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.iter().all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// Whether `name` reads as an IANA time zone name: letters, digits and `/_+-`, such as
/// `America/Argentina/Buenos_Aires` or `Etc/GMT+9`.
pub fn is_time_zone(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('/')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'+' | b'-'))
}

impl SystemSettings {
    /// These settings with every value the guest could not take left out: a time zone or a
    /// language that does not read as one, languages past [`MAX_LOCALES`] or named twice, and
    /// a day of the week past Sunday.
    pub fn checked(&self) -> SystemSettings {
        let mut locales: Vec<String> = Vec::new();
        for tag in &self.locales {
            let tag = tag.trim().replace('_', "-");
            if is_language_tag(&tag)
                && !locales.iter().any(|known| known.eq_ignore_ascii_case(&tag))
                && locales.len() < MAX_LOCALES
            {
                locales.push(tag);
            }
        }
        let regional_format = self.regional_format.trim().replace('_', "-");
        SystemSettings {
            time_zone: if is_time_zone(self.time_zone.trim()) {
                self.time_zone.trim().to_string()
            } else {
                String::new()
            },
            locales,
            regional_format: if is_language_tag(&regional_format) {
                regional_format
            } else {
                String::new()
            },
            time_24_hour: self.time_24_hour,
            first_day_of_week: if self.first_day_of_week <= 7 {
                self.first_day_of_week
            } else {
                0
            },
            dark_mode: self.dark_mode,
            accent_color: self.accent_color,
            text_scale_percent: match self.text_scale_percent {
                0 => 0,
                percent => percent.clamp(
                    guest_display::MIN_TEXT_SCALE_PERCENT,
                    guest_display::MAX_TEXT_SCALE_PERCENT,
                ),
            },
        }
    }

    /// The control message that carries these settings to the guest. The font scale is
    /// always set, even when the text size is unknown (it is then 1), so the guest never keeps
    /// a scale the host no longer asks for.
    pub fn to_control(&self) -> control::SystemSettings {
        control::SystemSettings {
            time_zone: self.time_zone.clone(),
            locales: self.locales.clone(),
            regional_format: self.regional_format.clone(),
            time_24_hour: self.time_24_hour,
            first_day_of_week: self.first_day_of_week,
            dark_mode: self.dark_mode,
            accent_color: self.accent_color,
            font_scale: guest_display::font_scale(self.text_scale_percent),
        }
    }

    /// The settings a local API message carries, checked.
    pub fn from_wire(settings: &types::SystemSettings) -> SystemSettings {
        SystemSettings {
            time_zone: settings.time_zone.clone(),
            locales: settings.locales.clone(),
            regional_format: settings.regional_format.clone(),
            time_24_hour: settings.time_24_hour,
            first_day_of_week: settings.first_day_of_week,
            dark_mode: settings.dark_mode,
            accent_color: settings.accent_color,
            text_scale_percent: settings.text_scale_percent,
        }
        .checked()
    }

    /// The local API message that carries these settings.
    pub fn to_wire(&self) -> types::SystemSettings {
        types::SystemSettings {
            time_zone: self.time_zone.clone(),
            locales: self.locales.clone(),
            regional_format: self.regional_format.clone(),
            time_24_hour: self.time_24_hour,
            first_day_of_week: self.first_day_of_week,
            dark_mode: self.dark_mode,
            accent_color: self.accent_color,
            text_scale_percent: self.text_scale_percent,
        }
    }
}

/// Reads the host's system settings for [`follow`], and says when they may have changed.
pub trait SystemSettingsSource {
    /// The settings now.
    fn read(&mut self) -> SystemSettings;
    /// Waits at most `timeout` for word that the settings may have changed (on Windows,
    /// `WM_SETTINGCHANGE` or `WM_TIMECHANGE`). Returns whether such word came.
    fn wait_for_change(&mut self, timeout: Duration) -> bool;
}

/// How long [`follow`] waits before trying the service again.
const RETRY: Duration = Duration::from_secs(2);

/// How often [`follow`] reads the settings even without word of a change, in case a change
/// came without it.
pub const REREAD: Duration = Duration::from_secs(30);

/// How long [`follow`] waits for a change at a time, so it sees `stop`.
const POLL: Duration = Duration::from_millis(250);

/// Gives the user's Android the host's system settings until `stop` is set: when it starts,
/// each time `source` says they may have changed and they did, every [`REREAD`] when they
/// changed without word, and again on each new connection to the service.
pub fn follow(
    connector: &mut dyn Connector,
    build: &str,
    source: &mut dyn SystemSettingsSource,
    stop: &AtomicBool,
    log: &dyn Fn(&str),
) {
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let mut client = match HostApiClient::connect(connector, build, CONNECT_TIMEOUT) {
            Ok(client) => client,
            Err(why) => {
                if !warned {
                    log(&format!(
                        "system settings: cannot give Android the settings yet: {why}"
                    ));
                    warned = true;
                }
                sleep_unless(stop, RETRY);
                continue;
            }
        };
        warned = false;
        let mut told: Option<SystemSettings> = None;
        let mut last_read = Instant::now();
        let mut changed = true;
        while !stop.load(Ordering::Relaxed) {
            let reread = last_read.elapsed() >= REREAD;
            if changed || reread {
                last_read = Instant::now();
                let settings = source.read().checked();
                // Sent again now and then even when nothing changed: a service that restarted
                // has none of them.
                if told.as_ref() != Some(&settings) || reread {
                    if let Err(why) = client.set_system_settings(&settings) {
                        log(&format!(
                            "system settings: the service connection ended: {why}"
                        ));
                        break;
                    }
                    log(&format!(
                        "system settings: Android follows time zone {:?}, languages {:?}, \
                         {} theme, text size {}%",
                        settings.time_zone,
                        settings.locales,
                        if settings.dark_mode { "dark" } else { "light" },
                        if settings.text_scale_percent == 0 {
                            100
                        } else {
                            settings.text_scale_percent
                        }
                    ));
                    told = Some(settings);
                }
            }
            changed = source.wait_for_change(POLL);
        }
        sleep_unless(stop, RETRY);
    }
}

fn sleep_unless(stop: &AtomicBool, wait: Duration) {
    let deadline = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seoul() -> SystemSettings {
        SystemSettings {
            time_zone: "Asia/Seoul".into(),
            locales: vec!["ko-KR".into(), "en-US".into()],
            regional_format: "ko-KR".into(),
            time_24_hour: false,
            first_day_of_week: 7,
            dark_mode: true,
            accent_color: 0xFF00_78D4,
            text_scale_percent: 100,
        }
    }

    #[test]
    fn fr30_settings_the_guest_can_take_pass_unchanged() {
        assert_eq!(seoul().checked(), seoul());
        let control = seoul().to_control();
        assert_eq!(control.time_zone, "Asia/Seoul");
        assert_eq!(control.locales, vec!["ko-KR", "en-US"]);
        assert!(control.dark_mode);
        assert_eq!(control.accent_color, 0xFF00_78D4);
    }

    #[test]
    fn fr30_values_the_guest_could_not_take_are_left_out() {
        let odd = SystemSettings {
            time_zone: "../../etc/passwd".into(),
            locales: vec![
                "en_GB".into(),
                "en-gb".into(),
                "not a tag".into(),
                "x".into(),
            ],
            regional_format: "!!".into(),
            first_day_of_week: 9,
            ..seoul()
        }
        .checked();
        assert_eq!(odd.time_zone, "");
        assert_eq!(odd.locales, vec!["en-GB"]);
        assert_eq!(odd.regional_format, "");
        assert_eq!(odd.first_day_of_week, 0);
        let many = SystemSettings {
            locales: (0..40).map(|i| format!("en-x{i:02}")).collect(),
            ..seoul()
        }
        .checked();
        assert_eq!(many.locales.len(), MAX_LOCALES);
    }

    #[test]
    fn fr30_time_zone_names_read_as_iana_names() {
        assert!(is_time_zone("America/Argentina/Buenos_Aires"));
        assert!(is_time_zone("Etc/GMT+9"));
        assert!(is_time_zone("UTC"));
        assert!(!is_time_zone(""));
        assert!(!is_time_zone("/etc/localtime"));
        assert!(!is_time_zone("Asia/Seoul; rm"));
    }
}

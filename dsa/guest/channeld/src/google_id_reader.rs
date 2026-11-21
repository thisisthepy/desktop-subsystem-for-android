//! `dsa-channeld --google-device-id`: reads the Google Services Framework Android ID from where
//! Google Play services keep it, as init runs it as root in a domain of its own.
//!
//! The services' check-in preferences belong to their app user, and neither the platform's
//! policy nor file permissions let the daemon read them (see [`crate::google_device_id`]). init
//! starts this when the daemon sets the ID's property to the request, and it ends after one look:
//! it sets the property to the ID, in decimal, or to the word for none. It reads one file and
//! writes one property, nothing else.

use std::path::Path;

use crate::google_device_id::{is_id, NONE, PROPERTY};

/// Where Google Play services keep their check-in preferences.
pub const CHECKIN_PREFERENCES: &str = "/data/data/com.google.android.gms/shared_prefs/Checkin.xml";

/// The most of the preferences file that is read. The file is about a kilobyte; anything much
/// larger is not the file this expects.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// The Android ID in `xml`, the text of a Google Play services `Checkin.xml`: the element
/// `<string name="android_id">4562097385933913176</string>`. `None` when there is none, or it is
/// not a decimal number, so nothing else is ever passed on as an ID.
pub fn parse_checkin(xml: &str) -> Option<String> {
    let mut rest = xml;
    while let Some(start) = rest.find("<string") {
        rest = &rest[start + "<string".len()..];
        let tag_end = rest.find('>')?;
        let (attributes, after) = rest.split_at(tag_end);
        if attributes.ends_with('/') {
            continue;
        }
        let named_android_id =
            attributes.contains("name=\"android_id\"") || attributes.contains("name='android_id'");
        let after = &after[1..];
        let close = after.find("</string>")?;
        if named_android_id {
            let id = after[..close].trim();
            return is_id(id).then(|| id.to_string());
        }
        rest = &after[close..];
    }
    None
}

/// The Android ID in the preferences file at `path`, or `None` while there is no such file or
/// it has no ID yet (the device has not checked in with Google).
pub fn read_from(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    parse_checkin(&text)
}

/// One look at the preferences at `path`: sets the property through `set` to the ID or to the
/// word for none. Returns the process's exit code.
pub fn run(path: &Path, set: &dyn Fn(&str, &str) -> Result<(), String>) -> i32 {
    let answer = read_from(path);
    match set(PROPERTY, answer.as_deref().unwrap_or(NONE)) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("dsa-channeld: could not say the Google device ID: {why}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const CHECKIN: &str = "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n\
        <long name=\"CheckinInterval_FlexSec\" value=\"10800\" />\n\
        <string name=\"android_id\">4562097385933913176</string>\n\
        <string name=\"CheckinService_deviceDataVersionInfo\">ABF</string>\n</map>\n";

    #[test]
    fn fr32_the_android_id_is_the_decimal_string_in_the_checkin_preferences() {
        assert_eq!(
            parse_checkin(CHECKIN).as_deref(),
            Some("4562097385933913176")
        );
    }

    #[test]
    fn fr32_a_checkin_file_without_an_id_gives_none() {
        let before_checkin = "<map><long name=\"CheckinTask_bookmark\" value=\"0\" /></map>";
        assert_eq!(parse_checkin(before_checkin), None);
        assert_eq!(parse_checkin(""), None);
        let empty = "<map><string name=\"android_id\"></string></map>";
        assert_eq!(parse_checkin(empty), None);
    }

    #[test]
    fn fr32_a_value_that_is_not_decimal_is_never_passed_on_as_an_id() {
        let hex = "<map><string name=\"android_id\">3f2a9c0d11e4b7a8</string></map>";
        assert_eq!(parse_checkin(hex), None);
    }

    #[test]
    fn fr32_the_id_is_found_after_empty_and_other_string_elements() {
        let xml = "<map><string name=\"empty\" /><string name=\"other\">12</string>\
                   <string name=\"android_id\">987</string></map>";
        assert_eq!(parse_checkin(xml).as_deref(), Some("987"));
    }

    #[test]
    fn fr32_the_helper_sets_the_property_to_the_id_or_to_none() {
        let dir = std::env::temp_dir().join(format!("dsa-channeld-gid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Checkin.xml");
        let sets = RefCell::new(Vec::new());
        let set = |name: &str, value: &str| {
            sets.borrow_mut()
                .push((name.to_string(), value.to_string()));
            Ok(())
        };
        assert_eq!(run(&path, &set), 0);
        std::fs::write(&path, CHECKIN).unwrap();
        assert_eq!(run(&path, &set), 0);
        assert_eq!(
            *sets.borrow(),
            vec![
                (PROPERTY.to_string(), NONE.to_string()),
                (PROPERTY.to_string(), "4562097385933913176".to_string()),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

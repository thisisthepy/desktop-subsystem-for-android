//! The Google Services Framework Android ID, read from where Google Play services keep it.
//!
//! Google's page for uncertified devices asks for this ID before it lets the user sign in. The
//! agent asks Google's gservices provider for it first, but current Google Play services refuse
//! every caller that is not on their own list, the agent included, so the answer is often empty.
//! The services also write the ID, in decimal, into their own preferences file after the
//! device's first check-in with Google. The daemon may read that file, so the agent's empty
//! answer is filled in from here on its way to the host.
//!
//! The ID is kept in decimal there and shown in decimal: that is the form Google's page takes.
//! Other tools print it in hexadecimal, which that page does not accept.

use std::path::Path;

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
            return (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                .then(|| id.to_string());
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

/// The Android ID Google Play services keep on this device, if they have made one.
pub fn read() -> Option<String> {
    read_from(Path::new(CHECKIN_PREFERENCES))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fr32_the_id_is_read_from_the_file_where_google_keeps_it() {
        let dir = std::env::temp_dir().join(format!("dsa-channeld-gid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Checkin.xml");
        assert_eq!(read_from(&path), None);
        std::fs::write(&path, CHECKIN).unwrap();
        assert_eq!(read_from(&path).as_deref(), Some("4562097385933913176"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

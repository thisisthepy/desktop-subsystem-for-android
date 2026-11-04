//! What the permissions an app asks for let it do, in words a person reads before
//! installing it, and which of them Android asks the user about when the app runs.

/// A permission an APK asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    /// The permission's name, such as `android.permission.CAMERA`.
    pub name: String,
    /// What it lets the app do, as a short sentence; the name itself when it is not known.
    pub description: String,
    /// Whether Android asks the user before the app may use it (a runtime permission).
    pub sensitive: bool,
}

/// The framework permissions an app commonly asks for: the name after `android.permission.`,
/// whether Android asks the user about it, and what it lets the app do.
const KNOWN: &[(&str, bool, &str)] = &[
    ("ACCESS_BACKGROUND_LOCATION", true, "Know your location while the app is not in use"),
    ("ACCESS_COARSE_LOCATION", true, "Know your approximate location"),
    ("ACCESS_FINE_LOCATION", true, "Know your precise location"),
    ("ACCESS_MEDIA_LOCATION", true, "Read the locations stored in your photos"),
    ("ACCESS_NETWORK_STATE", false, "See which networks you are connected to"),
    ("ACCESS_WIFI_STATE", false, "See Wi-Fi connections"),
    ("ACTIVITY_RECOGNITION", true, "Recognize your physical activity"),
    ("ANSWER_PHONE_CALLS", true, "Answer phone calls"),
    ("BLUETOOTH", false, "Pair with Bluetooth devices"),
    ("BLUETOOTH_ADMIN", false, "Change Bluetooth settings"),
    ("BLUETOOTH_ADVERTISE", true, "Make this device visible to nearby Bluetooth devices"),
    ("BLUETOOTH_CONNECT", true, "Connect to paired Bluetooth devices"),
    ("BLUETOOTH_SCAN", true, "Find nearby Bluetooth devices"),
    ("BODY_SENSORS", true, "Read body sensors such as heart rate"),
    ("BODY_SENSORS_BACKGROUND", true, "Read body sensors while the app is not in use"),
    ("CALL_PHONE", true, "Call phone numbers"),
    ("CAMERA", true, "Take pictures and record video"),
    ("CHANGE_NETWORK_STATE", false, "Change network connectivity"),
    ("CHANGE_WIFI_STATE", false, "Connect to and disconnect from Wi-Fi"),
    ("FOREGROUND_SERVICE", false, "Keep working while it shows a notification"),
    ("GET_ACCOUNTS", true, "Find the accounts on the device"),
    ("INTERNET", false, "Have full network access"),
    ("MANAGE_EXTERNAL_STORAGE", false, "Ask to manage all files in shared storage"),
    ("MODIFY_AUDIO_SETTINGS", false, "Change audio settings"),
    ("NEARBY_WIFI_DEVICES", true, "Find and connect to nearby Wi-Fi devices"),
    ("POST_NOTIFICATIONS", true, "Show notifications"),
    ("QUERY_ALL_PACKAGES", false, "See every app installed"),
    ("READ_CALENDAR", true, "Read your calendar"),
    ("READ_CALL_LOG", true, "Read your call log"),
    ("READ_CONTACTS", true, "Read your contacts"),
    ("READ_EXTERNAL_STORAGE", true, "Read the files in shared storage"),
    ("READ_MEDIA_AUDIO", true, "Read music and audio files"),
    ("READ_MEDIA_IMAGES", true, "Read photos"),
    ("READ_MEDIA_VIDEO", true, "Read videos"),
    ("READ_MEDIA_VISUAL_USER_SELECTED", true, "Read the photos and videos you choose"),
    ("READ_PHONE_NUMBERS", true, "Read the device's phone numbers"),
    ("READ_PHONE_STATE", true, "Read phone status and identity"),
    ("READ_SMS", true, "Read your text messages"),
    ("RECEIVE_BOOT_COMPLETED", false, "Start when Android starts"),
    ("RECEIVE_MMS", true, "Receive multimedia messages"),
    ("RECEIVE_SMS", true, "Receive text messages"),
    ("RECORD_AUDIO", true, "Record audio with the microphone"),
    ("REQUEST_INSTALL_PACKAGES", false, "Ask to install other apps"),
    ("SCHEDULE_EXACT_ALARM", false, "Set alarms at exact times"),
    ("SEND_SMS", true, "Send text messages"),
    ("SYSTEM_ALERT_WINDOW", false, "Ask to show over other apps"),
    ("USE_BIOMETRIC", false, "Use biometric hardware"),
    ("USE_FINGERPRINT", false, "Use fingerprint hardware"),
    ("USE_FULL_SCREEN_INTENT", false, "Show full-screen notifications"),
    ("VIBRATE", false, "Control vibration"),
    ("WAKE_LOCK", false, "Keep the device from sleeping"),
    ("WRITE_CALENDAR", true, "Add or change calendar events"),
    ("WRITE_CALL_LOG", true, "Change your call log"),
    ("WRITE_CONTACTS", true, "Change your contacts"),
    ("WRITE_EXTERNAL_STORAGE", true, "Change or delete the files in shared storage"),
    ("WRITE_SETTINGS", false, "Ask to change system settings"),
];

/// Describes the permission `name`.
pub fn describe(name: &str) -> Permission {
    let known = name
        .strip_prefix("android.permission.")
        .and_then(|short| KNOWN.iter().find(|(known, _, _)| *known == short));
    match known {
        Some((_, sensitive, description)) => Permission {
            name: name.to_string(),
            description: (*description).to_string(),
            sensitive: *sensitive,
        },
        None => Permission {
            name: name.to_string(),
            description: name.to_string(),
            sensitive: false,
        },
    }
}

/// The Android version an API level stands for, as people know it.
pub fn android_version(api_level: u32) -> Option<&'static str> {
    Some(match api_level {
        1 => "1.0",
        2 => "1.1",
        3 => "1.5",
        4 => "1.6",
        5..=7 => "2.0",
        8 => "2.2",
        9 | 10 => "2.3",
        11..=13 => "3",
        14 | 15 => "4.0",
        16 => "4.1",
        17 => "4.2",
        18 => "4.3",
        19 | 20 => "4.4",
        21 => "5.0",
        22 => "5.1",
        23 => "6",
        24 => "7.0",
        25 => "7.1",
        26 => "8.0",
        27 => "8.1",
        28 => "9",
        29 => "10",
        30 => "11",
        31 => "12",
        32 => "12L",
        33 => "13",
        34 => "14",
        35 => "15",
        36 => "16",
        37 => "17",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr11_a_permission_is_described_in_plain_words() {
        let camera = describe("android.permission.CAMERA");
        assert_eq!(camera.description, "Take pictures and record video");
        assert!(camera.sensitive);
        assert!(!describe("android.permission.INTERNET").sensitive);
        let own = describe("com.example.notes.permission.SYNC");
        assert_eq!(own.description, "com.example.notes.permission.SYNC");
    }

    #[test]
    fn fr11_an_api_level_reads_as_its_android_version() {
        assert_eq!(android_version(34), Some("14"));
        assert_eq!(android_version(32), Some("12L"));
        assert_eq!(android_version(99), None);
    }
}

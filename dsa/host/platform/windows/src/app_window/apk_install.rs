//! Installing an APK opened in Explorer.
//!
//! The installer associates `.apk` files with the installed window host, which it runs with
//! [`INSTALL_APK_ARGUMENT`] and the file's path when the user opens one. The window host then
//! opens the APK, makes sure the window manager and Android run (as when an app is opened from
//! the Start menu), streams the APK to the service, and tells the user how the install ended
//! in a message box. An installed app then gets its Start menu entry from the window manager,
//! as every app Android reports does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dsa_host_core::apk::{ApkFile, InstallOutcome};
use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;

use super::argument;

/// The argument that names an APK to install.
pub const INSTALL_APK_ARGUMENT: &str = "--install-apk";

/// The build the window host names itself by when it installs an APK.
const BUILD: &str = "dsa-app-window install";

/// The APK `args` asks to install, if they ask for one.
pub fn install_request(args: &[String]) -> Option<PathBuf> {
    argument(args, INSTALL_APK_ARGUMENT).map(PathBuf::from)
}

/// Opens the APK at `path`, refusing it before Android is started when it cannot be installed.
pub fn open(path: &Path) -> Result<ApkFile, String> {
    ApkFile::open(path)
}

/// Streams `apk` to the caller's Android through the service at `api`, and returns how the
/// install ended. An error means the APK did not get there.
pub fn install(api: &mut dyn Connector, apk: &mut ApkFile) -> Result<InstallOutcome, String> {
    let name = apk.name.clone();
    let size = apk.size;
    HostApiClient::connect(api, BUILD, Duration::from_secs(5))
        .map_err(|e| format!("the DSA service cannot be reached: {e}"))?
        .install_apk(&name, size, apk.reader())
        .map_err(|e| e.to_string())
}

/// What the message box says when the install ended, and whether it is good news.
pub fn result_text(name: &str, outcome: &InstallOutcome) -> (String, bool) {
    if outcome.installed() {
        let package = outcome
            .package_name
            .as_deref()
            .map(|package| format!(" ({package})"))
            .unwrap_or_default();
        (
            format!("{name}{package} is installed. Open it from the Start menu."),
            true,
        )
    } else {
        (format!("{name} was not installed. {outcome}."), false)
    }
}

/// What the message box says when the APK did not reach Android.
pub fn failure_text(name: &str, why: &str) -> String {
    format!("{name} was not installed. {why}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::apk::InstallStatus;

    #[test]
    fn fr11_opening_an_apk_names_it_to_the_window_host() {
        let args = vec![
            INSTALL_APK_ARGUMENT.to_string(),
            r"C:\Users\me\Downloads\My Notes.apk".to_string(),
        ];
        assert_eq!(
            install_request(&args),
            Some(PathBuf::from(r"C:\Users\me\Downloads\My Notes.apk"))
        );
        assert_eq!(install_request(&["--package".into(), "a.b".into()]), None);
    }

    #[test]
    fn fr11_the_message_says_the_result_and_the_reason() {
        let (text, good) = result_text(
            "notes.apk",
            &InstallOutcome {
                status: InstallStatus::Installed,
                package_name: Some("com.example.notes".into()),
                message: String::new(),
            },
        );
        assert!(good);
        assert!(text.contains("com.example.notes") && text.contains("Start menu"), "{text}");
        let (text, good) = result_text(
            "notes.apk",
            &InstallOutcome {
                status: InstallStatus::Incompatible,
                package_name: None,
                message: "INSTALL_FAILED_NO_MATCHING_ABIS".into(),
            },
        );
        assert!(!good);
        assert!(text.contains("was not installed"), "{text}");
        assert!(text.contains("INSTALL_FAILED_NO_MATCHING_ABIS"), "{text}");
    }
}

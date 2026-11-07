//! Installing an APK, or a split-APK bundle, opened in Explorer.
//!
//! The installer associates `.apk` files, and the bundles `.xapk`, `.apks` and `.apkm`, with
//! the APK Installer
//! (`settings\DsaApkInstaller.exe`), which it runs with [`INSTALL_APK_ARGUMENT`] and the file's path when the user opens one.
//! Earlier releases associated them with the window host instead, with the same argument; the
//! window host hands such an APK on to the APK Installer ([`install_window`]) when it is
//! installed. Without it, the window host opens the APK itself, makes sure the window manager
//! and Android run (as when an app is opened from the Start menu), streams the APK to the
//! service, and tells the user how the install ended in a message box. An installed app then
//! gets its Start menu entry from the window manager, as every app Android reports does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dsa_host_core::apk::InstallOutcome;
use dsa_host_core::apk_bundle::{Device, InstallFile};
use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;

use super::argument;

/// The argument that names an APK to install.
pub const INSTALL_APK_ARGUMENT: &str = "--install-apk";

/// The build the window host names itself by when it installs an APK.
const BUILD: &str = "dsa-app-window install";

/// The APK Installer beside the window host at `program`: its own program in the
/// program folder's `settings` folder, beside the settings app. None when it is not there.
pub fn install_window(program: &Path) -> Option<PathBuf> {
    let window = program
        .parent()?
        .join("settings")
        .join("DsaApkInstaller.exe");
    window.is_file().then_some(window)
}

/// The arguments that open the install window on the APK at `path`.
pub fn install_window_arguments(path: &Path) -> String {
    format!("{INSTALL_APK_ARGUMENT} \"{}\"", path.display())
}

/// The APK `args` asks to install, if they ask for one.
pub fn install_request(args: &[String]) -> Option<PathBuf> {
    argument(args, INSTALL_APK_ARGUMENT).map(PathBuf::from)
}

/// Opens the APK or the split-APK bundle at `path`, refusing it before Android is started when
/// it cannot be installed, with the reason.
pub fn open(path: &Path) -> Result<InstallFile, String> {
    InstallFile::open(path).map_err(|refusal| refusal.to_string())
}

/// Streams `file` to the caller's Android through the service at `api`, and returns how the
/// install ended. Of a bundle, the APKs this computer's Android takes go, with its expansion
/// files, as one install. An error means the app did not get there.
pub fn install(api: &mut dyn Connector, file: &mut InstallFile) -> Result<InstallOutcome, String> {
    let mut client = HostApiClient::connect(api, BUILD, Duration::from_secs(5))
        .map_err(|e| format!("the DSA service cannot be reached: {e}"))?;
    match file {
        InstallFile::Apk(apk) => {
            let name = apk.name.clone();
            let size = apk.size;
            let libraries = apk.native_libraries.clone();
            client
                .install_apk_with_libraries(&name, size, &libraries, apk.reader())
                .map_err(|e| e.to_string())
        }
        InstallFile::Bundle(bundle) => {
            let translation = client.arm_translation().ok();
            let device = Device::of_host(
                translation.as_ref(),
                cfg!(target_arch = "aarch64"),
                crate::monitors::system_density(),
                &crate::compositor::system_settings::read().locales,
            );
            let selection = bundle.select(&device);
            let parts = bundle.parts(&selection);
            let libraries = bundle.libraries(&selection);
            let mut content = bundle.content(&selection);
            client
                .install_parts(&bundle.name, &parts, &libraries, &mut content)
                .map_err(|e| e.to_string())
        }
    }
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
    fn fr11_an_apk_opened_through_the_window_host_goes_to_the_apk_installer() {
        let dir = std::env::temp_dir().join(format!("dsa-install-window-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("settings")).unwrap();
        let program = dir.join("dsa-app-window.exe");
        assert_eq!(install_window(&program), None);
        std::fs::write(dir.join("settings").join("DsaApkInstaller.exe"), b"MZ").unwrap();
        assert_eq!(
            install_window(&program),
            Some(dir.join("settings").join("DsaApkInstaller.exe"))
        );
        assert_eq!(
            install_window_arguments(Path::new(r"C:\Users\me\My Notes.apk")),
            r#"--install-apk "C:\Users\me\My Notes.apk""#
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(
            text.contains("com.example.notes") && text.contains("Start menu"),
            "{text}"
        );
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

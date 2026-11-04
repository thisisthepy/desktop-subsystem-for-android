//! The installer's association of .apk files with the APK install window, read from its
//! source: a double-clicked APK must reach the settings app with the argument that opens the
//! install window, and the window host must take the same argument from earlier releases.
#![cfg(windows)]

use dsa_host_windows::app_window::apk_install::{install_request, INSTALL_APK_ARGUMENT};

const INSTALLER: &str = include_str!("../installer/dsa.wxs");
const SETTINGS_APP: &str = include_str!("../settings/DsaSettings/App.xaml.cs");

#[test]
fn fr11_opening_an_apk_opens_the_install_window_with_the_install_argument() {
    assert!(
        INSTALLER.contains(r#"Key="SOFTWARE\Classes\.apk""#),
        "the installer does not associate .apk files"
    );
    let command = INSTALLER
        .lines()
        .skip_while(|line| !line.contains(r"Dsa.ApkFile\shell\open\command"))
        .nth(1)
        .expect("the command that opens an APK");
    assert!(
        command.contains(&format!(
            "DsaSettings.exe&quot; {INSTALL_APK_ARGUMENT} &quot;%1&quot;"
        )),
        "{command}"
    );
    assert!(
        SETTINGS_APP.contains(&format!("InstallApkArgument = \"{INSTALL_APK_ARGUMENT}\"")),
        "the settings app does not take {INSTALL_APK_ARGUMENT}"
    );
    // What Explorer passes is what the window host reads.
    let args = vec![
        INSTALL_APK_ARGUMENT.to_string(),
        r"D:\apps\notes.apk".to_string(),
    ];
    assert_eq!(
        install_request(&args).as_deref(),
        Some(std::path::Path::new(r"D:\apps\notes.apk"))
    );
}

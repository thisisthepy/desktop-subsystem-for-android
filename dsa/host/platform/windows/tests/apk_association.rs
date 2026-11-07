//! The installer's association of .apk files with the APK install window, read from its
//! source: a double-clicked APK must reach APK Installer, the install window's own app, with
//! the argument it reads, and the window host must take the same argument from earlier
//! releases.
#![cfg(windows)]

use dsa_host_windows::app_window::apk_install::{install_request, INSTALL_APK_ARGUMENT};

const INSTALLER: &str = include_str!("../installer/dsa.wxs");
const APK_INSTALLER: &str = include_str!("../settings/DsaApkInstaller/App.xaml.cs");

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
            "DsaApkInstaller.exe&quot; {INSTALL_APK_ARGUMENT} &quot;%1&quot;"
        )),
        "{command}"
    );
    assert!(
        APK_INSTALLER.contains(&format!("InstallApkArgument = \"{INSTALL_APK_ARGUMENT}\"")),
        "APK Installer does not take {INSTALL_APK_ARGUMENT}"
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

//! The installer's association of .apk files with the window host, read from its source: a
//! double-clicked APK must reach the window host with the argument that installs it.
#![cfg(windows)]

use dsa_host_windows::app_window::apk_install::{install_request, INSTALL_APK_ARGUMENT};

const INSTALLER: &str = include_str!("../installer/dsa.wxs");

#[test]
fn fr11_opening_an_apk_runs_the_window_host_with_the_install_argument() {
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
            "dsa-app-window.exe&quot; {INSTALL_APK_ARGUMENT} &quot;%1&quot;"
        )),
        "{command}"
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

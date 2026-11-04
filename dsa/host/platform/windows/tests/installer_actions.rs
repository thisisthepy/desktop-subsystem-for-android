//! What the installer ends before it replaces the program's files.

use std::path::{Path, PathBuf};

use dsa_host_windows::installer_actions::is_dsa_program;

fn apps() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Users\ada\AppData\Local\DSA\apps"),
        PathBuf::from(r"D:\Profiles\bob\Local\DSA\apps"),
    ]
}

#[test]
fn fr1_programs_running_from_the_program_folder_are_ended_before_an_upgrade() {
    let program = Path::new(r"C:\Program Files\DSA\");
    for image in [
        r"C:\Program Files\DSA\dsa-wm.exe",
        r"c:\program files\dsa\DSA-APP-WINDOW.EXE",
        r"\\?\C:\Program Files\DSA\settings\DsaSettings.exe",
        "C:/Program Files/DSA/dsa.exe",
    ] {
        assert!(is_dsa_program(Path::new(image), program, &apps()), "{image}");
    }
}

#[test]
fn fr1_each_apps_window_program_is_ended_in_every_users_profile() {
    let program = Path::new(r"C:\Program Files\DSA");
    for image in [
        r"C:\Users\ada\AppData\Local\DSA\apps\com.example.notes\Notes.exe",
        r"D:\Profiles\bob\Local\DSA\apps\org.example\Example.exe",
    ] {
        assert!(is_dsa_program(Path::new(image), program, &apps()), "{image}");
    }
}

#[test]
fn fr1_other_programs_are_left_running() {
    let program = Path::new(r"C:\Program Files\DSA");
    for image in [
        r"C:\Program Files\DSA Other\dsa-wm.exe",
        r"C:\Program Files\DSAX\tool.exe",
        r"C:\Windows\explorer.exe",
        r"C:\Users\ada\AppData\Local\DSA\android.vhdx",
        r"C:\Program Files\DSA",
    ] {
        assert!(!is_dsa_program(Path::new(image), program, &apps()), "{image}");
    }
    // An empty folder from a missing argument matches nothing.
    let explorer = Path::new(r"C:\Windows\explorer.exe");
    assert!(!is_dsa_program(explorer, Path::new(""), &[]));
}

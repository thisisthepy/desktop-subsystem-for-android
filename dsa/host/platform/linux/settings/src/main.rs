//! dsa-settings: DSA Settings for Linux. See the library documentation.

#[cfg(target_os = "linux")]
fn main() -> gtk::glib::ExitCode {
    dsa_settings_linux::ui::run()
}

/// The window is built only on Linux; the Windows and macOS hosts have settings apps of their
/// own.
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-settings is the settings app of the Linux host; run it on Linux");
    std::process::ExitCode::FAILURE
}

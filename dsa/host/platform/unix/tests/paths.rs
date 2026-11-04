//! Where a Unix host keeps its files.

use std::path::PathBuf;

use dsa_host_core::host_paths::HostPaths;
use dsa_host_unix::paths::{Layout, UnixHostPaths};

fn home(user: &str) -> Result<PathBuf, String> {
    match user {
        "1000" => Ok(PathBuf::from("/home/alice")),
        _ => Err(format!("no home for {user}")),
    }
}

#[test]
fn fr19_linux_follows_the_xdg_base_directories() {
    let paths = UnixHostPaths::standard(Layout::Linux, Box::new(home));
    let machine = PathBuf::from("/var/lib").join("dsa");
    assert_eq!(paths.machine_data(), machine);
    assert_eq!(paths.images(), machine.join("images"));
    let user = PathBuf::from("/home/alice")
        .join(".local")
        .join("share")
        .join("dsa");
    assert_eq!(paths.user_data("1000").unwrap(), user);
    assert_eq!(paths.apps("1000").unwrap(), user.join("apps"));
}

#[test]
fn fr19_linux_xdg_data_home_wins_for_its_user() {
    let data = PathBuf::from("/data/alice");
    if !data.is_absolute() {
        // A path from the root only counts as absolute on Unix.
        return;
    }
    let paths =
        UnixHostPaths::standard(Layout::Linux, Box::new(home)).with_xdg_data_home("1000", &data);
    assert_eq!(paths.user_data("1000").unwrap(), data.join("dsa"));
}

#[test]
fn fr19_macos_uses_application_support() {
    let paths = UnixHostPaths::standard(Layout::MacOs, Box::new(home));
    assert_eq!(
        paths.settings(),
        PathBuf::from("/Library/Application Support")
            .join("DSA")
            .join("settings")
    );
    assert_eq!(
        paths.user_logs("1000").unwrap(),
        PathBuf::from("/home/alice")
            .join("Library")
            .join("Application Support")
            .join("DSA")
            .join("logs")
    );
}

#[test]
fn fr19_a_user_without_a_home_is_reported() {
    let paths = UnixHostPaths::standard(Layout::Linux, Box::new(home));
    assert!(paths.user_tmp("2000").is_err());
}

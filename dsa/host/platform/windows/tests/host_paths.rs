//! The Windows adapter of the storage port gives the same places as the paths module.

use std::path::{Path, PathBuf};

use dsa_host_core::host_paths::HostPaths;
use dsa_host_windows::paths::{MachineRoot, UserRoot, WindowsHostPaths};

const SID: &str = "S-1-5-21-1-2-3-1001";

fn paths() -> WindowsHostPaths {
    WindowsHostPaths::new(
        MachineRoot::at(r"C:\ProgramData\DSA"),
        Box::new(|sid| match sid {
            SID => Ok(UserRoot::in_local_app_data(Path::new(
                r"C:\Users\alice\AppData\Local",
            ))),
            _ => Err(format!("no profile for {sid}")),
        }),
    )
}

#[test]
fn fr19_windows_host_paths_match_the_machine_root() {
    let paths = paths();
    let machine = MachineRoot::at(r"C:\ProgramData\DSA");
    assert_eq!(paths.machine_data(), PathBuf::from(r"C:\ProgramData\DSA"));
    assert_eq!(paths.images(), machine.images());
    assert_eq!(paths.updates(), machine.updates());
    assert_eq!(paths.settings(), machine.settings());
    assert_eq!(paths.machine_logs(), machine.logs());
    assert_eq!(paths.machine_tmp(), machine.tmp());
}

#[test]
fn fr19_windows_host_paths_match_the_user_root() {
    let paths = paths();
    let user = UserRoot::in_local_app_data(Path::new(r"C:\Users\alice\AppData\Local"));
    assert_eq!(paths.user_data(SID).unwrap(), user.dir());
    assert_eq!(paths.apps(SID).unwrap(), user.apps());
    assert_eq!(paths.user_logs(SID).unwrap(), user.logs());
    assert_eq!(paths.user_tmp(SID).unwrap(), user.tmp());
    assert!(paths.user_data("S-1-5-21-9").is_err());
}

//! "Android" in Explorer's navigation pane, read from the installer's source: the machine's
//! entry must be the one the window manager writes for each user, in the desktop namespace,
//! and point at the files door the service opens.
#![cfg(windows)]

use dsa_host_windows::android_files::{NAVIGATION_CLSID, NAVIGATION_NAME, NAVIGATION_SORT_ORDER};

const INSTALLER: &str = include_str!("../installer/dsa.wxs");

/// Whether a line holding `first` is followed by one holding `second`.
fn follows(first: &str, second: &str) -> bool {
    let lines: Vec<&str> = INSTALLER.lines().collect();
    lines
        .windows(2)
        .any(|pair| pair[0].contains(first) && pair[1].contains(second))
}

#[test]
fn fr29_the_installer_puts_android_in_explorers_navigation_pane() {
    let class = format!(r"SOFTWARE\Classes\CLSID\{NAVIGATION_CLSID}");
    assert!(
        follows(
            &format!(r#"Key="{class}""#),
            &format!(r#"Type="string" Value="{NAVIGATION_NAME}""#)
        ),
        "the installer does not name the entry {NAVIGATION_NAME}"
    );
    assert!(
        INSTALLER.contains(&format!(r"Explorer\Desktop\NameSpace\{NAVIGATION_CLSID}")),
        "the entry is not in the desktop namespace, which the navigation pane shows"
    );
    assert!(
        INSTALLER.contains(r#"Name="System.IsPinnedToNameSpaceTree" Type="integer" Value="1""#),
        "the entry is not pinned to the navigation pane"
    );
    assert!(
        INSTALLER.contains(&format!(
            r#"Name="TargetFolderPath" Type="string" Value="\\127.0.0.1@{}\DavWWWRoot""#,
            dsa_host_core::files_door::DOOR_PORT
        )),
        "the machine's entry does not open the files door"
    );
    assert!(
        follows(
            r"HideDesktopIcons\NewStartPanel",
            &format!(r#"Name="{NAVIGATION_CLSID}""#)
        ),
        "the entry would show on the desktop too"
    );
}

#[test]
fn fr29_the_machines_entry_sorts_where_each_users_does() {
    assert!(
        INSTALLER.contains(&format!(
            r#"Name="SortOrderIndex" Type="integer" Value="{NAVIGATION_SORT_ORDER}""#
        )),
        "the machine's entry and each user's entry sort to different places"
    );
}

#[test]
fn fr29_dsa_files_has_no_start_menu_shortcut_and_an_old_one_is_removed() {
    assert!(
        !INSTALLER.contains(r#"Name="DSA Files""#),
        "a Start menu shortcut would let Windows search find DSA Files"
    );
    assert!(
        INSTALLER.contains(r#"Name="DSA Files.lnk""#),
        "the shortcut an earlier version made would stay after an upgrade"
    );
}

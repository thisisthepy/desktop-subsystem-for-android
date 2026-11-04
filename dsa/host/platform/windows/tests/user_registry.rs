//! The names DSA registers in the user's classes, and the removal of the names earlier
//! versions used: real keys under the current user's `Software\Classes`.
#![cfg(windows)]

use dsa_host_windows::compositor::notifications::{activation_uri, OLD_PROTOCOL, PROTOCOL};
use dsa_host_windows::start_menu::{app_user_model_id, APP_ID_PREFIX, OLD_APP_ID_PREFIX};
use dsa_host_windows::user_registry::remove_old_names_for_current_user;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn create(path: &str) {
    let path_w = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated path; the key is closed once.
    let code = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(code, ERROR_SUCCESS, "creating HKCU\\{path}");
    // SAFETY: opened above.
    unsafe { RegCloseKey(key) };
}

fn exists(path: &str) -> bool {
    let path_w = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated path; the key is closed once.
    let code = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, KEY_READ, &mut key) };
    if code == ERROR_SUCCESS {
        // SAFETY: opened above.
        unsafe { RegCloseKey(key) };
    }
    code == ERROR_SUCCESS
}

fn delete(path: &str) {
    let path_w = wide(path);
    // SAFETY: a NUL-terminated path.
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path_w.as_ptr()) };
}

#[test]
fn fr19_the_notification_protocol_is_a_vendor_qualified_scheme() {
    // RFC 3986: a letter, then letters, digits, `+`, `-` and `.`; a packaged app's protocol
    // is also lowercase and between 2 and 39 characters.
    assert!(PROTOCOL.starts_with("io.github.thisisthepy."));
    assert!(PROTOCOL.len() >= 2 && PROTOCOL.len() <= 39, "{PROTOCOL}");
    assert!(PROTOCOL.chars().next().unwrap().is_ascii_lowercase());
    assert!(PROTOCOL
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c)));
    assert!(activation_uri("k").starts_with(&format!("{PROTOCOL}:")));
    assert_ne!(PROTOCOL, OLD_PROTOCOL);
}

#[test]
fn fr19_an_apps_id_is_company_product_and_package() {
    let id = app_user_model_id("com.example.clock");
    assert_eq!(id, "Thisisthepy.DSA.com.example.clock");
    assert!(id.starts_with(APP_ID_PREFIX));
    assert!(!id.starts_with(OLD_APP_ID_PREFIX));
    assert!(!id.contains(' '));
    let long = app_user_model_id(&format!("com.{}", "a".repeat(200)));
    assert_eq!(long.chars().count(), 128, "Windows takes at most 128 characters");
}

#[test]
fn fr19_the_window_manager_removes_the_names_earlier_versions_registered() {
    let package = format!("com.example.fr19.p{}", std::process::id());
    let old_protocol = format!("Software\\Classes\\{OLD_PROTOCOL}");
    let new_protocol = format!("Software\\Classes\\{PROTOCOL}");
    let old_id = format!("Software\\Classes\\AppUserModelId\\{OLD_APP_ID_PREFIX}{package}");
    let new_id = format!(
        "Software\\Classes\\AppUserModelId\\{}",
        app_user_model_id(&package)
    );
    let foreign_id = format!("Software\\Classes\\AppUserModelId\\Example.Other.{package}");
    let had_new_protocol = exists(&new_protocol);
    create(&format!("{old_protocol}\\shell\\open\\command"));
    create(&new_protocol);
    create(&old_id);
    create(&new_id);
    create(&foreign_id);

    let problems = remove_old_names_for_current_user();

    let (old_protocol_left, old_id_left) = (exists(&old_protocol), exists(&old_id));
    let (new_protocol_left, new_id_left, foreign_left) =
        (exists(&new_protocol), exists(&new_id), exists(&foreign_id));
    delete(&new_id);
    delete(&foreign_id);
    if !had_new_protocol {
        delete(&new_protocol);
    }
    assert!(problems.is_empty(), "{problems:?}");
    assert!(!old_protocol_left, "the old protocol key is removed");
    assert!(!old_id_left, "the old app ID key is removed");
    assert!(new_protocol_left, "the protocol of this version stays");
    assert!(new_id_left, "the app IDs of this version stay");
    assert!(foreign_left, "another program's app ID is never touched");
}

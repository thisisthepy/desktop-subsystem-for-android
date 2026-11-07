//! Each Android app in Windows' list of installed apps: the values of its key, and keeping the
//! keys in step with the apps, against a fake and against real keys under the current user.
#![cfg(windows)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use dsa_host_core::tasks::AppInfo;
use dsa_host_windows::installed_apps::{
    entries, entry, install_date, is_ours, key_name, remove_all, sync, uninstall_command, Entry,
    InstalledAppKeys, RegistryKeys, Value, PUBLISHER,
};

fn notes() -> AppInfo {
    AppInfo {
        package_name: "com.example.notes".into(),
        label: "Notes".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
        version_name: "4.1".into(),
        version_code: 41,
        // 2023-11-14T22:13:20Z.
        first_install_time_ms: 1_700_000_000_000,
        last_update_time_ms: 1_710_000_000_000,
        system_app: false,
        app_bytes: 3_000_000,
        data_bytes: 20_480,
        cache_bytes: 1_024,
        ..Default::default()
    }
}

fn value<'a>(entry: &'a Entry, name: &str) -> Option<&'a Value> {
    entry
        .values
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
}

#[test]
fn fr23_each_app_is_listed_with_its_name_icon_version_publisher_size_and_uninstall() {
    let program = Path::new(r"C:\Users\u\AppData\Local\DSA\apps\com.example.notes\Notes.exe");
    let uninstaller = Path::new(r"C:\Program Files\DSA\settings\DsaSettings.exe");
    let entry = entry(&notes(), program, uninstaller);
    assert_eq!(entry.key, "Thisisthepy.DSA.com.example.notes");
    assert_eq!(
        value(&entry, "DisplayName"),
        Some(&Value::Text("Notes".into()))
    );
    assert_eq!(
        value(&entry, "DisplayIcon"),
        Some(&Value::Text(format!("\"{}\",0", program.display())))
    );
    assert_eq!(
        value(&entry, "DisplayVersion"),
        Some(&Value::Text("4.1".into()))
    );
    assert_eq!(
        value(&entry, "Publisher"),
        Some(&Value::Text(PUBLISHER.into()))
    );
    assert_eq!(
        value(&entry, "InstallDate"),
        Some(&Value::Text("20231114".into()))
    );
    assert_eq!(value(&entry, "EstimatedSize"), Some(&Value::Number(2950)));
    assert_eq!(value(&entry, "NoModify"), Some(&Value::Number(1)));
    assert_eq!(value(&entry, "NoRepair"), Some(&Value::Number(1)));
    assert_eq!(
        value(&entry, "UninstallString"),
        Some(&Value::Text(
            r#""C:\Program Files\DSA\settings\DsaSettings.exe" --uninstall-app com.example.notes"#
                .into()
        ))
    );
    assert_eq!(
        uninstall_command(uninstaller, "com.example.notes"),
        r#""C:\Program Files\DSA\settings\DsaSettings.exe" --uninstall-app com.example.notes"#
    );
}

/// Windows runs the UninstallString as it is, so a console program there flashes a console
/// window; the settings app is a windowed program and shows its own uninstall window.
#[test]
fn fr23_uninstalling_from_windows_runs_the_windowed_settings_app_not_the_console_cli() {
    let wm = Path::new(r"C:\Program Files\DSA\dsa-wm.exe");
    let uninstaller = dsa_host_windows::start_menu::uninstaller_beside(wm).unwrap();
    assert_eq!(
        uninstaller,
        Path::new(r"C:\Program Files\DSA\settings\DsaSettings.exe")
    );
    let command = uninstall_command(&uninstaller, "com.example.notes");
    assert!(!command.contains("dsa.exe"), "{command}");
    assert!(
        command.ends_with("--uninstall-app com.example.notes"),
        "{command}"
    );
}

#[test]
fn fr23_an_app_android_says_little_of_still_gets_a_name_and_no_empty_values() {
    let bare = AppInfo {
        package_name: "com.example.bare".into(),
        ..Default::default()
    };
    let entry = entry(&bare, Path::new(r"C:\a.exe"), Path::new(r"C:\dsa.exe"));
    assert_eq!(
        value(&entry, "DisplayName"),
        Some(&Value::Text("com.example.bare".into()))
    );
    assert_eq!(value(&entry, "DisplayVersion"), None);
    assert_eq!(value(&entry, "InstallDate"), None);
}

#[test]
fn fr23_install_dates_are_utc_days() {
    assert_eq!(install_date(0), None);
    assert_eq!(install_date(1).as_deref(), Some("19700101"));
    // 2000-02-29T23:59:59Z, a leap day.
    assert_eq!(install_date(951_868_799_000).as_deref(), Some("20000229"));
    // 2024-12-31T12:00:00Z.
    assert_eq!(install_date(1_735_646_400_000).as_deref(), Some("20241231"));
}

#[test]
fn fr23_the_entries_point_at_each_apps_window_program_and_skip_bad_names() {
    let bad = AppInfo {
        package_name: "..\\evil".into(),
        label: "Evil".into(),
        ..Default::default()
    };
    let list = entries(
        &[notes(), bad],
        Path::new(r"C:\apps"),
        Path::new(r"C:\dsa.exe"),
    );
    assert_eq!(list.len(), 1);
    assert_eq!(
        value(&list[0], "DisplayIcon"),
        Some(&Value::Text(
            r#""C:\apps\com.example.notes\Notes.exe",0"#.into()
        ))
    );
}

/// Keys in memory.
#[derive(Default)]
struct FakeKeys {
    keys: RefCell<BTreeMap<String, Vec<(&'static str, Value)>>>,
}

impl InstalledAppKeys for FakeKeys {
    fn names(&self) -> Result<Vec<String>, String> {
        Ok(self.keys.borrow().keys().cloned().collect())
    }
    fn write(&self, name: &str, values: &[(&'static str, Value)]) -> Result<(), String> {
        self.keys
            .borrow_mut()
            .insert(name.to_string(), values.to_vec());
        Ok(())
    }
    fn remove(&self, name: &str) -> Result<(), String> {
        self.keys.borrow_mut().remove(name);
        Ok(())
    }
}

#[test]
fn fr23_keys_follow_the_apps_and_only_ours_are_touched() {
    let keys = FakeKeys::default();
    keys.write(
        "{1234-another-program}",
        &[("DisplayName", Value::Text("Other".into()))],
    )
    .unwrap();
    let cli = Path::new(r"C:\dsa.exe");
    let clock = AppInfo {
        package_name: "com.example.clock".into(),
        label: "Clock".into(),
        ..Default::default()
    };
    assert!(sync(
        &entries(&[notes(), clock], Path::new(r"C:\apps"), cli),
        &keys
    )
    .is_empty());
    assert_eq!(
        keys.names().unwrap(),
        vec![
            "Thisisthepy.DSA.com.example.clock".to_string(),
            "Thisisthepy.DSA.com.example.notes".to_string(),
            "{1234-another-program}".to_string(),
        ]
    );

    // The clock is uninstalled in Android.
    assert!(sync(&entries(&[notes()], Path::new(r"C:\apps"), cli), &keys).is_empty());
    assert_eq!(
        keys.names().unwrap(),
        vec![
            "Thisisthepy.DSA.com.example.notes".to_string(),
            "{1234-another-program}".to_string(),
        ]
    );

    // Uninstalling DSA removes every key of ours and nothing else.
    assert!(remove_all(&keys).is_empty());
    assert_eq!(
        keys.names().unwrap(),
        vec!["{1234-another-program}".to_string()]
    );
    assert!(is_ours(&key_name("com.example.notes")));
    assert!(!is_ours("Thisisthepy.DSA."));
}

#[test]
fn fr23_registry_keys_are_written_updated_and_removed() {
    let path = format!(
        "Software\\Thisisthepy-DSA-tests\\installed-apps-{}",
        std::process::id()
    );
    let keys = RegistryKeys::current_user_at(&path);
    let cli = Path::new(r"C:\dsa.exe");
    keys.write("{other}", &[("DisplayName", Value::Text("Other".into()))])
        .unwrap();
    let problems = sync(&entries(&[notes()], Path::new(r"C:\apps"), cli), &keys);
    assert!(problems.is_empty(), "{problems:?}");
    let mut names = keys.names().unwrap();
    names.sort();
    assert_eq!(
        names,
        vec![
            "Thisisthepy.DSA.com.example.notes".to_string(),
            "{other}".to_string()
        ]
    );

    // An update without a version leaves no stale DisplayVersion behind; reading it back is
    // through the names only, so writing again over the old key must succeed.
    let unversioned = AppInfo {
        version_name: String::new(),
        ..notes()
    };
    assert!(sync(&entries(&[unversioned], Path::new(r"C:\apps"), cli), &keys).is_empty());

    assert!(remove_all(&keys).is_empty());
    assert_eq!(keys.names().unwrap(), vec!["{other}".to_string()]);
    keys.remove("{other}").unwrap();
    let parent = RegistryKeys::current_user_at("Software\\Thisisthepy-DSA-tests");
    parent
        .remove(&format!("installed-apps-{}", std::process::id()))
        .unwrap();
}

//! The Google Play item of DSA Settings for Linux, without a display: the requests it makes
//! through the local API, what it reads from the answers, and what it shows in each state.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure};
use dsa_settings_linux::model::{Client, Gapps, GappsState};
use dsa_settings_linux::view;
use serde_json::json;

/// A stand-in for the service: it records each command line and answers with `answer`.
struct Fake {
    calls: Mutex<Vec<Vec<String>>>,
    answer: String,
}

impl Fake {
    fn new(answer: serde_json::Value) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answer: answer.to_string(),
        })
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

impl Dsa for Fake {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        self.calls.lock().unwrap().push(args.to_vec());
        Ok(self.answer.clone())
    }
}

fn line(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn installed_json(active: bool, restart_needed: bool) -> serde_json::Value {
    let device_id = if active { "3987654321012345678" } else { "" };
    json!({
        "package": {
            "variant": "core",
            "version": "20261001",
            "android_version": "16.0",
            "sdk": 36,
            "arch": "x86_64",
            "file": "LiteGapps-core-x86_64-16.0-20261001-official.zip",
            "source": "https://sourceforge.net/projects/litegapps/files/litegapps/x86_64/36/core/2026-10-01/LiteGapps-core-x86_64-16.0-20261001-official.zip/download",
            "md5": "ea9945a6d63849fa9e51782edfe613a4"
        },
        "installed": true,
        "android": {
            "installed": true,
            "active": active,
            "sdk": 36,
            "abi": "x86_64",
            "abilist": "x86_64,arm64-v8a",
            "device_id": device_id
        },
        "restart_needed": restart_needed,
        "terms_url": "https://policies.google.com/terms",
        "registration_url": "https://www.google.com/android/uncertified/"
    })
}

#[test]
fn fr32_install_status_and_remove_use_the_same_command_lines() {
    let fake = Fake::new(installed_json(true, false));
    let c = Client::new(fake.clone(), fake.clone(), true);
    c.gapps().unwrap();
    c.install_gapps().unwrap();
    c.remove_gapps().unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            line(&["gapps", "status", "--json"]),
            line(&["gapps", "install", "--json"]),
            line(&["gapps", "remove", "--json"]),
        ]
    );
}

#[test]
fn fr32_the_report_is_read_and_missing_fields_are_tolerated() {
    let gapps = Gapps::read(&installed_json(true, false));
    let package = gapps.package.clone().expect("a package");
    assert_eq!(package.variant, "core");
    assert_eq!(package.version, "20261001");
    assert_eq!(package.android_version, "16.0");
    assert_eq!(package.sdk, 36);
    assert_eq!(package.arch, "x86_64");
    assert_eq!(
        package.file,
        "LiteGapps-core-x86_64-16.0-20261001-official.zip"
    );
    assert_eq!(gapps.active, Some(true));
    assert_eq!(gapps.device_id.as_deref(), Some("3987654321012345678"));
    assert!(!gapps.restart_needed);

    // An empty answer is a computer with nothing installed and Android not running; the
    // pages Google names still open.
    let empty = Gapps::read(&json!({}));
    assert_eq!(empty.package, None);
    assert_eq!(empty.active, None);
    assert_eq!(empty.device_id, None);
    assert_eq!(empty.state(), GappsState::NotInstalled);
    assert_eq!(empty.terms_url, "https://policies.google.com/terms");
    assert_eq!(
        empty.registration_url,
        "https://www.google.com/android/uncertified/"
    );
    let partial = Gapps::read(&json!({ "package": { "version": "20261001" }, "android": null }));
    assert_eq!(partial.package.unwrap().version, "20261001");
    assert_eq!(partial.active, None);
}

#[test]
fn fr32_the_item_shows_before_install_after_install_and_while_a_restart_is_pending() {
    let before = Gapps::read(&json!({
        "package": null,
        "installed": false,
        "android": { "installed": false, "active": false, "device_id": "" },
        "restart_needed": false
    }));
    assert_eq!(before.state(), GappsState::NotInstalled);
    assert_eq!(view::gapps_status(&before), "Not installed.");
    assert_eq!(view::gapps_source(&before), "");
    assert_eq!(
        view::gapps_device_id(&before),
        "It shows here once Google Play is installed and Android has started."
    );

    let after = Gapps::read(&installed_json(true, false));
    assert_eq!(after.state(), GappsState::Installed);
    assert_eq!(
        view::gapps_status(&after),
        "Installed: LiteGapps core 20261001 for Android 16.0 (x86_64)."
    );
    assert_eq!(
        view::gapps_source(&after),
        "Downloaded from the LiteGapps project: LiteGapps-core-x86_64-16.0-20261001-official.zip"
    );
    assert_eq!(
        view::gapps_device_id(&after),
        "Enter this number on Google's registration page."
    );

    let waiting = Gapps::read(&installed_json(false, true));
    assert_eq!(waiting.state(), GappsState::RestartPending);
    assert_eq!(
        view::gapps_status(&waiting),
        "Installed: LiteGapps core 20261001 for Android 16.0 (x86_64). Restart Android to start using it."
    );

    let removed = Gapps::read(&json!({
        "package": null,
        "installed": false,
        "android": { "installed": false, "active": true, "device_id": "3987654321012345678" },
        "restart_needed": true
    }));
    assert_eq!(removed.state(), GappsState::RestartPending);
    assert_eq!(
        view::gapps_status(&removed),
        "Removed. The Google apps stay in Android until it restarts."
    );

    assert!(!view::gapps_busy_text().is_empty());
}

#[test]
fn fr32_install_works_only_without_a_package_and_remove_only_with_one() {
    let none = Gapps::read(&json!({ "package": null }));
    let some = Gapps::read(&installed_json(true, false));
    assert!(none.can_install(false));
    assert!(!none.can_remove(false));
    assert!(!some.can_install(false));
    assert!(some.can_remove(false));
    // Neither while a request is under way.
    for gapps in [&none, &some] {
        assert!(!gapps.can_install(true));
        assert!(!gapps.can_remove(true));
    }
}

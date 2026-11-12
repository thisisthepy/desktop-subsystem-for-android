//! The protected content item of DSA Settings for Linux, without a display: the command lines its
//! buttons run, the state it shows before an install, after one and while a restart is due, and
//! what it says of Google's terms and of L3.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure};
use dsa_settings_linux::model::{Client, Widevine, WidevineState};
use dsa_settings_linux::view;
use serde_json::json;

/// A stand-in for the service: it records each command line and answers with `answer`.
struct Fake {
    calls: Mutex<Vec<Vec<String>>>,
    answer: String,
}

impl Dsa for Fake {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        self.calls.lock().unwrap().push(args.to_vec());
        Ok(self.answer.clone())
    }
}

fn package() -> serde_json::Value {
    json!({
        "apex": "com.google.android.widevine.apex",
        "version": "BE2A.250530.026.F3 (API 36, r7)",
        "android_major": 16,
        "abi": "x86_64",
        "source": "https://dl.google.com/android/repository/sys-img/google_apis/x86_64-36_r07.zip",
        "size": 4096,
        "sha256": "00",
        "license": "android-sdk-license"
    })
}

fn status(package: serde_json::Value, active: bool, restart: bool) -> serde_json::Value {
    json!({
        "installed": !package.is_null(),
        "on_disk": !package.is_null(),
        "package": package,
        "running": true,
        "active": active,
        "restart_needed": restart,
        "security_level": "L3",
        "terms_url": "https://developer.android.com/studio/terms"
    })
}

fn line(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[test]
fn fr37_install_status_and_remove_use_the_same_command_lines() {
    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: status(package(), false, true).to_string(),
    });
    let client = Client::new(fake.clone(), fake.clone(), true);
    client.widevine().unwrap();
    let installed = client.install_widevine().unwrap();
    client.remove_widevine().unwrap();
    assert_eq!(
        fake.calls.lock().unwrap().clone(),
        vec![
            line(&["drm", "widevine", "status", "--json"]),
            // The user agreed in the app's own confirmation.
            line(&["drm", "widevine", "install", "--yes", "--json"]),
            line(&["drm", "widevine", "remove", "--json"]),
        ]
    );
    assert_eq!(installed.state(), WidevineState::RestartPending);
}

#[test]
fn fr37_the_item_shows_before_install_after_install_and_while_a_restart_is_pending() {
    let none = Widevine::read(&status(json!(null), false, false));
    assert_eq!(none.state(), WidevineState::NotInstalled);
    assert_eq!(view::widevine_status(&none), "Not installed.");
    assert!(none.can_install(false));
    assert!(!none.can_remove(false));
    assert!(!none.can_install(true), "not while a request is under way");

    let pending = Widevine::read(&status(package(), false, true));
    assert_eq!(pending.state(), WidevineState::RestartPending);
    assert_eq!(
        view::widevine_status(&pending),
        "Installed: Google's image BE2A.250530.026.F3 (API 36, r7) for Android 16 (x86_64). \
         Restart Android to start using it."
    );
    assert!(!pending.can_install(false));
    assert!(pending.can_remove(false));

    let installed = Widevine::read(&status(package(), true, false));
    assert_eq!(installed.state(), WidevineState::Installed);
    assert_eq!(installed.security_level, "L3");
    assert_eq!(
        view::widevine_status(&installed),
        "Installed: Google's image BE2A.250530.026.F3 (API 36, r7) for Android 16 (x86_64)."
    );

    let removed = Widevine::read(&status(json!(null), true, true));
    assert_eq!(removed.state(), WidevineState::RestartPending);
    assert_eq!(
        view::widevine_status(&removed),
        "Removed. Widevine stays in Android until it restarts."
    );
}

#[test]
fn fr37_the_item_names_googles_terms_and_says_l3_is_standard_definition() {
    let notice = view::widevine_notice();
    assert!(
        notice.contains("https://dl.google.com/android/repository/"),
        "{notice}"
    );
    assert!(
        notice.contains("https://developer.android.com/studio/terms"),
        "{notice}"
    );
    assert!(notice.contains("unchanged"), "{notice}");
    let quality = view::widevine_quality();
    assert!(quality.contains("L3"), "{quality}");
    assert!(quality.contains("standard definition"), "{quality}");
    assert!(quality.contains("browser or app"), "{quality}");
    assert!(!view::widevine_busy_text().is_empty());
}

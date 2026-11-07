//! Google's ARM translator in the ARM apps section of DSA Settings for Linux, without a
//! display: the command line it runs, what it reads from the answer, and what the confirmation
//! says before anything is downloaded.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure};
use dsa_settings_linux::model::{ArmTranslation, Client};
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

fn status(available: bool, package: serde_json::Value) -> serde_json::Value {
    json!({
        "google_fetch_available": available,
        "package": package,
        "enabled": true,
        "supported": !package.is_null(),
        "android": { "installed": !package.is_null(), "active": false, "abilist": "x86_64" },
        "restart_needed": !package.is_null()
    })
}

fn google() -> serde_json::Value {
    json!({
        "name": "Google ARM translator",
        "version": "BE2A.250530.026.F3 (API 36, r7)",
        "library": "libndk_translation.so",
        "abis": ["arm64-v8a"],
        "license": "Android SDK License"
    })
}

#[test]
fn fr25_the_google_button_runs_fetch_google_with_the_users_agreement() {
    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: status(true, google()).to_string(),
    });
    let client = Client::new(fake.clone(), fake.clone(), true);
    let fetched = client.fetch_google_arm_translator().unwrap();
    let line: Vec<String> = ["arm-translation", "fetch", "google", "--yes", "--json"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    assert_eq!(fake.calls.lock().unwrap().clone(), vec![line]);
    assert!(fetched.google_fetch_available);
    assert!(fetched.restart_needed, "it works from the next start");
    assert_eq!(
        view::arm_status(&fetched),
        "Supported by Google ARM translator BE2A.250530.026.F3 (API 36, r7) (Android SDK License)."
    );
}

#[test]
fn fr25_the_google_rows_show_only_where_dsa_can_fetch_it() {
    let offered = ArmTranslation::read(&status(true, json!(null)));
    assert!(offered.google_fetch_available);
    // An ARM64 computer runs ARM apps itself.
    let arm64 = ArmTranslation::read(&status(false, json!(null)));
    assert!(!arm64.google_fetch_available);
    // A dsa that cannot fetch it does not say it can.
    let old = ArmTranslation::read(&json!({ "package": null }));
    assert!(!old.google_fetch_available);
}

#[test]
fn fr25_the_confirmation_names_the_source_and_googles_terms() {
    let notice = view::arm_google_notice();
    assert!(
        notice.contains("https://dl.google.com/android/repository/"),
        "{notice}"
    );
    assert!(
        notice.contains("https://developer.android.com/studio/terms"),
        "{notice}"
    );
    assert!(notice.contains("Android SDK License"), "{notice}");
    assert!(!view::arm_google_busy_text().is_empty());
}

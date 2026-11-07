//! The default ARM translator in the ARM apps section of DSA Settings for Linux, without a
//! display: what it reads from dsa's answer, what the section says for each translator, and
//! the command line that goes back to the default one.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure};
use dsa_settings_linux::model::{ArmSource, ArmTranslation, Client};
use dsa_settings_linux::view;
use serde_json::{json, Value};

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

fn digitalis() -> Value {
    json!({
        "name": "Digitalis",
        "version": "17-abefbe2b1600",
        "library": "libberberis_arm64.so",
        "abis": ["arm64-v8a"],
        "license": "Apache-2.0"
    })
}

fn mine() -> Value {
    json!({
        "name": "Translator",
        "version": "1.0",
        "library": "libtranslator.so",
        "abis": ["arm64-v8a"],
        "license": "Apache-2.0"
    })
}

/// What `dsa arm-translation --json` says: `translator` names where the one in use comes from.
fn status(package: Value, default: Value, source: &str, enabled: bool) -> Value {
    let used = match source {
        "default" => &default,
        _ => &package,
    };
    let translator = if source.is_empty() {
        Value::Null
    } else {
        json!({
            "source": source,
            "name": used["name"],
            "version": used["version"],
            "license": used["license"],
        })
    };
    json!({
        "google_fetch_available": true,
        "package": package,
        "default": default,
        "translator": translator,
        "enabled": enabled,
        "supported": !source.is_empty(),
        "android": { "installed": !package.is_null(), "active": true, "abilist": "x86_64,arm64-v8a" },
        "restart_needed": false
    })
}

#[test]
fn fr25_the_section_shows_the_default_translator_from_the_first_start() {
    let arm = ArmTranslation::read(&status(Value::Null, digitalis(), "default", true));
    let translator = arm.translator.clone().unwrap();
    assert_eq!(translator.source, ArmSource::Default);
    assert_eq!(translator.name, "Digitalis");
    assert_eq!(
        view::arm_status(&arm),
        "Supported by Digitalis 17-abefbe2b1600 (Apache-2.0), the translator that comes with this Android."
    );
    let text = view::arm_default_text(&arm).expect("the image has a default translator");
    assert!(text.contains("Digitalis 17-abefbe2b1600"), "{text}");
    // Already in use, so there is nothing to go back to.
    assert!(!view::arm_can_use_default(&arm));
}

#[test]
fn fr25_a_package_the_user_installed_replaces_the_default_until_they_go_back() {
    let user = ArmTranslation::read(&status(mine(), digitalis(), "user", true));
    assert_eq!(user.translator.as_ref().unwrap().source, ArmSource::User);
    assert_eq!(
        view::arm_status(&user),
        "Supported by Translator 1.0 (Apache-2.0)."
    );
    assert!(view::arm_can_use_default(&user));
    let google = ArmTranslation::read(&status(
        json!({"name": "Google ARM translator", "version": "16", "library": "libndk_translation.so",
               "abis": ["arm64-v8a"], "license": "Android SDK License"}),
        digitalis(),
        "google",
        true,
    ));
    assert_eq!(
        google.translator.as_ref().unwrap().source,
        ArmSource::Google
    );
    assert!(view::arm_can_use_default(&google));

    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: status(Value::Null, digitalis(), "default", true).to_string(),
    });
    let client = Client::new(fake.clone(), fake.clone(), true);
    let back = client.use_default_arm_translator().unwrap();
    let line: Vec<String> = ["arm-translation", "use-default", "--json"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    assert_eq!(fake.calls.lock().unwrap().clone(), vec![line]);
    assert_eq!(back.translator.unwrap().source, ArmSource::Default);
    assert!(back.package.is_none());
}

#[test]
fn fr25_the_section_says_when_translation_is_off_and_offers_no_default_where_there_is_none() {
    let off = ArmTranslation::read(&status(Value::Null, digitalis(), "", false));
    assert!(off.translator.is_none());
    assert_eq!(
        view::arm_status(&off),
        "Not supported: Digitalis 17-abefbe2b1600 is turned off."
    );
    // An arm64 image, or an x86_64 one without a default translator: no row for it.
    let none = ArmTranslation::read(&status(Value::Null, Value::Null, "", true));
    assert_eq!(view::arm_default_text(&none), None);
    assert!(!view::arm_can_use_default(&none));
    assert_eq!(
        view::arm_status(&none),
        "Not supported: no ARM translation package is installed."
    );
    // A dsa from before the default translator says nothing of it.
    let old = ArmTranslation::read(&json!({
        "package": mine(), "enabled": true, "supported": true, "restart_needed": false
    }));
    assert_eq!(old.default, None);
    assert_eq!(old.translator.unwrap().source, ArmSource::User);
}

//! The automatic adb connection in DSA Settings for Linux, without a display: what it reads
//! from the service, the request its switch makes, and what the page says of each state.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure};
use dsa_settings_linux::model::{AdbAutoConnectState, Client, Settings};
use dsa_settings_linux::view;
use serde_json::json;

/// A stand-in for the service: it records each command line and answers with the settings.
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

#[test]
fn fr20_auto_connect_is_read_as_the_service_reports_it() {
    let on = Settings::read(&json!({
        "developer_mode": true, "adb_port": 58526,
        "adb_auto_connect": true, "adb_auto_connect_state": "attached"
    }));
    assert!(on.adb_auto_connect);
    assert_eq!(on.adb_auto_connect_state, AdbAutoConnectState::Attached);

    let off =
        Settings::read(&json!({ "adb_auto_connect": false, "adb_auto_connect_state": "off" }));
    assert!(!off.adb_auto_connect);
    assert_eq!(off.adb_auto_connect_state, AdbAutoConnectState::Off);

    // A service from before auto-connect: on, and not watching.
    let old = Settings::read(&json!({ "render_gpu": "display" }));
    assert!(old.adb_auto_connect);
    assert_eq!(old.adb_auto_connect_state, AdbAutoConnectState::Off);
}

#[test]
fn fr20_every_state_name_is_read() {
    for (name, state) in [
        ("off", AdbAutoConnectState::Off),
        ("waiting-for-server", AdbAutoConnectState::WaitingForServer),
        (
            "waiting-for-android",
            AdbAutoConnectState::WaitingForAndroid,
        ),
        ("attached", AdbAutoConnectState::Attached),
        ("detached", AdbAutoConnectState::Detached),
        ("", AdbAutoConnectState::Off),
        ("something-new", AdbAutoConnectState::Off),
    ] {
        assert_eq!(AdbAutoConnectState::read(name), state, "{name}");
    }
}

#[test]
fn fr20_the_switch_sets_adb_auto_connect_through_the_local_api() {
    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: json!({ "adb_auto_connect": false }).to_string(),
    });
    let elevated = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: String::new(),
    });
    let c = Client::new(fake.clone(), elevated.clone(), true);
    let now = c.switch("adb-auto-connect", false).unwrap();
    assert!(!now.adb_auto_connect);
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![["settings", "set", "adb-auto-connect", "off", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()]
    );
    assert!(elevated.calls.lock().unwrap().is_empty(), "the user's own");
}

#[test]
fn fr20_each_state_has_its_own_words() {
    let states = [
        AdbAutoConnectState::Off,
        AdbAutoConnectState::WaitingForServer,
        AdbAutoConnectState::WaitingForAndroid,
        AdbAutoConnectState::Attached,
        AdbAutoConnectState::Detached,
    ];
    let texts: Vec<String> = states
        .iter()
        .map(|state| view::adb_auto_connect_text(*state))
        .collect();
    for (i, text) in texts.iter().enumerate() {
        assert!(!text.is_empty());
        assert!(
            texts.iter().skip(i + 1).all(|other| other != text),
            "{text:?} says two states"
        );
    }
    assert!(view::adb_auto_connect_text(AdbAutoConnectState::Detached).contains("restarts"));
}

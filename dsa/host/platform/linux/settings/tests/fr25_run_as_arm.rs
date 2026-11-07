//! The "Run as ARM" switch of the Apps page of DSA Settings for Linux, without a display: what
//! it reads from `dsa apps list`, the requests it makes, and what it shows when it cannot be
//! changed.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure, Problem};
use dsa_settings_linux::model::{App, Client};
use dsa_settings_linux::view::{self, RunAsArmNext};
use serde_json::json;

/// A stand-in for the service: it records each command line and gives the answer the test
/// set for it.
struct Fake {
    calls: Mutex<Vec<String>>,
    answers: Vec<(String, Result<String, String>)>,
}

impl Fake {
    fn new(answers: &[(&str, Result<serde_json::Value, &str>)]) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answers: answers
                .iter()
                .map(|(line, answer)| {
                    let answer = match answer {
                        Ok(json) => Ok(json.to_string()),
                        Err(error) => Err(error.to_string()),
                    };
                    (line.to_string(), answer)
                })
                .collect(),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Dsa for Fake {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        let line = args.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        match self.answers.iter().find(|(l, _)| *l == line) {
            Some((_, Ok(out))) => Ok(out.clone()),
            Some((_, Err(error))) => Err(Failure::from_client(error)),
            None => Err(Failure::new(
                Problem::Refused,
                format!("no answer for {line}"),
            )),
        }
    }
}

fn client(fake: Arc<Fake>) -> Client {
    Client::new(fake.clone(), fake, false)
}

fn app(run_as_arm: serde_json::Value) -> App {
    App::read(&json!({
        "package": "com.example.game",
        "label": "Game",
        "system_app": false,
        "primary_abi": "x86_64",
        "native_abis": ["arm64-v8a", "x86_64"],
        "run_as_arm": run_as_arm,
    }))
}

#[test]
fn fr25_the_switch_is_off_with_its_reason_when_it_cannot_be_changed() {
    for (reason, says) in [
        ("translation-off", "ARM translation is not on"),
        ("no-arm-libraries", "holds its ARM code"),
        ("system-app", "part of Android"),
        ("arm-only", "only ARM code"),
    ] {
        let game = app(json!({
            "on": reason == "arm-only",
            "available": false,
            "reason": reason,
        }));
        let shown = view::run_as_arm_view(&game);
        assert!(!shown.sensitive, "{reason}");
        assert!(
            shown.subtitle.contains(says),
            "{reason}: {}",
            shown.subtitle
        );
    }
    let available = app(json!({"on": false, "available": true, "reason": ""}));
    let on = view::run_as_arm_view(&available);
    assert!(on.sensitive && !on.on);
    // A dsa that says nothing of the switch leaves it off.
    let older = App::read(&json!({"package": "com.example.game"}));
    let unknown = view::run_as_arm_view(&older);
    assert!(!unknown.sensitive);
    assert!(
        unknown.subtitle.contains("cannot say"),
        "{}",
        unknown.subtitle
    );
}

#[test]
fn fr25_switching_keeps_the_data_or_asks_before_it_goes() {
    let fake = Fake::new(&[
        (
            "apps run-as-arm com.example.game on --json",
            Err("dsa: not changed (data-would-be-erased): com.example.game: Android cannot change how the app runs and keep its data"),
        ),
        (
            "apps run-as-arm com.example.game on --erase-data --json",
            Ok(json!({"package": "com.example.game", "status": "done", "on": true, "message": ""})),
        ),
    ]);
    let client = client(fake.clone());
    let first = client.run_as_arm("com.example.game", true, false).unwrap();
    assert_eq!(
        view::run_as_arm_next("Game", true, &first),
        RunAsArmNext::ConfirmErase
    );
    let erased = client.run_as_arm("com.example.game", true, true).unwrap();
    assert_eq!(
        view::run_as_arm_next("Game", true, &erased),
        RunAsArmNext::Done("Game runs as ARM now.".into())
    );
    assert_eq!(
        fake.calls(),
        vec![
            "apps run-as-arm com.example.game on --json".to_string(),
            "apps run-as-arm com.example.game on --erase-data --json".to_string(),
        ]
    );
}

#[test]
fn fr25_a_refusal_says_why() {
    let fake = Fake::new(&[(
        "apps run-as-arm com.example.game off --json",
        Err("dsa: not changed (no-libraries): com.example.game: The app's APKs on the device hold no code for that ABI"),
    )]);
    let outcome = client(fake)
        .run_as_arm("com.example.game", false, false)
        .unwrap();
    match view::run_as_arm_next("Game", false, &outcome) {
        RunAsArmNext::Refused(why) => {
            assert!(why.starts_with("Game was not installed again"), "{why}");
            assert!(why.contains("hold no code"), "{why}");
        }
        other => panic!("expected a refusal, not {other:?}"),
    }
}

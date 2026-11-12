//! Root access in DSA Settings for Linux, without a display: the computer's switch on the
//! Advanced page, which only an administrator changes, and each app's level on the Apps page.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure, Problem};
use dsa_settings_linux::model::{App, Client, Settings};
use dsa_settings_linux::view;
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

fn app(root: &str) -> App {
    App::read(&json!({ "package": "com.example.backup", "label": "Backup", "root": root }))
}

#[test]
fn fr35_root_elevation_is_off_by_default_and_changed_as_an_administrator() {
    // A service from before the setting says nothing of it: off.
    assert!(!Settings::read(&json!({ "version": "0.1.9" })).root_elevation);
    assert!(Settings::read(&json!({ "root_elevation": true })).root_elevation);

    let user = Fake::new(&[]);
    let elevated = Fake::new(&[(
        "settings set root-elevation on --json",
        Ok(json!({ "root_elevation": true })),
    )]);
    let c = Client::new(user.clone(), elevated.clone(), true);
    assert!(
        c.set_machine("root-elevation", true)
            .unwrap()
            .root_elevation
    );
    // The user's own dsa is never asked: pkexec asks for an administrator's permission.
    assert!(user.calls().is_empty());
    assert_eq!(
        elevated.calls(),
        vec!["settings set root-elevation on --json"]
    );
}

#[test]
fn fr35_the_page_says_a_change_applies_from_the_next_start_and_where_requests_are_recorded() {
    assert!(!view::root_restart_needed(None, true));
    assert!(!view::root_restart_needed(Some(false), false));
    assert!(view::root_restart_needed(Some(false), true));
    assert!(view::root_restart_needed(Some(true), false));

    let at = view::root_log_text(Some("/var/lib/dsa/logs"));
    assert!(at.contains("service log"), "{at}");
    assert!(at.ends_with("/var/lib/dsa/logs"), "{at}");
    let unknown = view::root_log_text(None);
    assert!(unknown.contains("service log"), "{unknown}");
    assert_eq!(view::root_log_text(Some("")), unknown);
}

#[test]
fn fr35_each_app_shows_its_root_level_off_ask_or_remember() {
    assert_eq!(view::ROOT_LEVELS, ["off", "ask", "remember"]);
    assert_eq!(view::root_choices().len(), view::ROOT_LEVELS.len());
    for (index, level) in view::ROOT_LEVELS.iter().enumerate() {
        let shown = view::root_view(&app(level));
        assert_eq!(shown.index, Some(index), "{level}");
        assert!(shown.sensitive, "{level}");
        assert!(shown.subtitle.contains("15 minutes"), "{}", shown.subtitle);
    }
    assert!(!view::root_unavailable(&[app("ask"), app("off")]));
}

#[test]
fn fr35_no_app_level_can_be_chosen_while_root_elevation_is_off_for_the_computer() {
    let off = view::root_view(&app("unavailable"));
    assert_eq!(off.index, None);
    assert!(!off.sensitive);
    assert!(off.subtitle.contains("Advanced"), "{}", off.subtitle);
    assert!(view::root_unavailable(&[app("unavailable")]));

    // A dsa from before root levels says nothing; the level cannot be chosen either.
    let older = view::root_view(&App::read(&json!({ "package": "com.example.notes" })));
    assert_eq!(older.index, None);
    assert!(!older.sensitive);
    assert!(!view::root_unavailable(&[App::read(
        &json!({ "package": "com.example.notes" })
    )]));
}

#[test]
fn fr35_apps_sharing_a_user_id_are_approved_as_one_group() {
    let shared = |package: &str, label: &str| {
        App::read(&json!({
            "package": package,
            "label": label,
            "root": "ask",
            "shared_user_id": "com.example.shared",
        }))
    };
    let apps = vec![
        shared("com.example.zeta", "Zeta"),
        App::read(&json!({ "package": "com.example.middle", "label": "Middle", "root": "off" })),
        shared("com.example.alpha", "Alpha"),
    ];
    // The group is listed together, in the place of its first app by name.
    let order: Vec<&str> = view::filter_apps(&apps, "")
        .iter()
        .map(|app| app.label.as_str())
        .collect();
    assert_eq!(order, ["Alpha", "Zeta", "Middle"]);
    // A search that finds one app of the group shows the whole group.
    let found: Vec<&str> = view::filter_apps(&apps, "zeta")
        .iter()
        .map(|app| app.label.as_str())
        .collect();
    assert_eq!(found, ["Alpha", "Zeta"]);
    // Each app of the group names the others and says the level is one for all of them.
    let note = view::root_group_note(&apps[0], &apps).unwrap();
    assert!(note.contains("Alpha"), "{note}");
    assert!(note.contains("user ID"), "{note}");
    assert!(view::root_group_note(&apps[1], &apps).is_none());
}

#[test]
fn fr35_choosing_a_level_goes_through_dsa_apps_root_as_the_user() {
    let user = Fake::new(&[(
        "apps root com.example.backup ask --json",
        Ok(json!({ "package": "com.example.backup", "level": "ask" })),
    )]);
    let elevated = Fake::new(&[]);
    let c = Client::new(user.clone(), elevated.clone(), true);
    let outcome = c.set_app_root("com.example.backup", "ask").unwrap();
    assert!(outcome.done);
    assert_eq!(outcome.status, "ask");
    // The level is the user's own, so it is never set through pkexec, which would set root's.
    assert!(elevated.calls().is_empty());
    let toast = view::root_next("Backup", &outcome).unwrap();
    assert!(
        toast.starts_with("Backup now asks an administrator"),
        "{toast}"
    );
}

#[test]
fn fr35_remember_without_an_administrators_approval_is_refused_with_the_services_words() {
    let user = Fake::new(&[(
        "apps root com.example.backup remember --json",
        Err("dsa: not changed: com.example.backup: letting an app keep root without asking needs an administrator"),
    )]);
    let c = Client::new(user.clone(), user, true);
    let outcome = c.set_app_root("com.example.backup", "remember").unwrap();
    assert!(!outcome.done);
    let why = view::root_next("Backup", &outcome).unwrap_err();
    assert_eq!(
        why,
        "The root level of Backup was not changed: letting an app keep root without asking needs an administrator"
    );

    // Any other failure stays a failure, which the page shows as one.
    let unreachable = Fake::new(&[(
        "apps root com.example.backup ask --json",
        Err("dsa: cannot reach the DSA service"),
    )]);
    let c = Client::new(unreachable.clone(), unreachable, true);
    let failure = c.set_app_root("com.example.backup", "ask").unwrap_err();
    assert_eq!(failure.problem, Problem::ServiceUnreachable);
}

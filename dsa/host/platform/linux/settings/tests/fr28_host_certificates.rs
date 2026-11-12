//! The trusted certificates section of the network page in DSA Settings for Linux, without a
//! display: what it reads from the service, the request its switch makes, and what each row
//! says.

use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure};
use dsa_settings_linux::model::{Client, ImportedCertificate, Settings};
use dsa_settings_linux::view;
use serde_json::json;

/// A stand-in for the service: it records each command line and answers with the settings.
struct Fake {
    calls: Mutex<Vec<Vec<String>>>,
    answer: String,
}

impl Fake {
    fn new(answer: String) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answer,
        })
    }
}

impl Dsa for Fake {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        self.calls.lock().unwrap().push(args.to_vec());
        Ok(self.answer.clone())
    }
}

fn line(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

#[test]
fn fr28_host_certificates_switch_sends_the_setting() {
    for (on, word) in [(false, "off"), (true, "on")] {
        let fake = Fake::new(json!({ "host_certificates": on }).to_string());
        let elevated = Fake::new(String::new());
        let c = Client::new(fake.clone(), elevated.clone(), true);
        let now = c.set_host_certificates(on).unwrap();
        assert_eq!(now.host_certificates, on);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            vec![line(&[
                "settings",
                "set",
                "host-certificates",
                word,
                "--json"
            ])]
        );
        assert!(
            elevated.calls.lock().unwrap().is_empty(),
            "the user's own setting needs no administrator"
        );
    }
}

#[test]
fn fr28_imported_certificates_are_listed_with_their_source() {
    let fake = Fake::new(
        json!({
            "host_certificates": false,
            "imported_certificates": [
                {
                    "subject": "CN=Example Root, O=Example",
                    "issuer": "CN=Example Root, O=Example",
                    "source": "system",
                    "location": "/usr/local/share/ca-certificates/example.crt",
                    "sha256": "ab".repeat(32),
                    "not_after": 1893456000
                },
                {
                    "subject": "",
                    "issuer": "CN=Someone",
                    "source": "something_new",
                    "location": "",
                    "sha256": "cd".repeat(32),
                    "not_after": 0
                },
                "not a certificate"
            ]
        })
        .to_string(),
    );
    let c = Client::new(fake.clone(), Fake::new(String::new()), true);
    let settings = c.settings().unwrap();
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![line(&["settings", "--json"])]
    );
    assert!(!settings.host_certificates);
    assert_eq!(
        settings.imported_certificates,
        vec![
            ImportedCertificate {
                subject: "CN=Example Root, O=Example".into(),
                issuer: "CN=Example Root, O=Example".into(),
                source: "system".into(),
                location: "/usr/local/share/ca-certificates/example.crt".into(),
                sha256: "ab".repeat(32),
                not_after: 1893456000,
            },
            ImportedCertificate {
                subject: String::new(),
                issuer: "CN=Someone".into(),
                source: "something_new".into(),
                location: String::new(),
                sha256: "cd".repeat(32),
                not_after: 0,
            },
        ]
    );

    let first = &settings.imported_certificates[0];
    assert_eq!(view::certificate_title(first), "CN=Example Root, O=Example");
    let detail = view::certificate_detail(first);
    let lines: Vec<&str> = detail.lines().collect();
    assert_eq!(
        lines,
        vec![
            "Issued by CN=Example Root, O=Example",
            "Added to the system's certificate store",
            "/usr/local/share/ca-certificates/example.crt",
        ]
    );

    // No name: the fingerprint stands in. An unknown source is shown as the service names it.
    let second = &settings.imported_certificates[1];
    assert!(view::certificate_title(second).contains(&"cd".repeat(32)));
    assert_eq!(
        view::certificate_detail(second),
        "Issued by CN=Someone\nsomething_new"
    );

    // Every source the service names has words of its own.
    let known = [
        "machine",
        "machine_group_policy",
        "machine_enterprise",
        "user",
        "user_group_policy",
        "admin",
        "system",
    ];
    let labels: Vec<String> = known
        .iter()
        .map(|s| view::certificate_source_label(s))
        .collect();
    for (i, (source, label)) in known.iter().zip(&labels).enumerate() {
        assert_ne!(label, source, "{source} has no words of its own");
        assert!(
            labels.iter().skip(i + 1).all(|other| other != label),
            "{label:?} says two sources"
        );
    }
}

#[test]
fn fr28_missing_certificate_keys_read_as_on_and_empty() {
    // A service from before trusted certificates.
    let old = Settings::read(&json!({ "render_gpu": "display", "advanced_networking": true }));
    assert!(old.host_certificates);
    assert!(old.imported_certificates.is_empty());

    let wrong = Settings::read(&json!({
        "host_certificates": "yes",
        "imported_certificates": { "subject": "CN=Not a list" }
    }));
    assert!(wrong.host_certificates);
    assert!(wrong.imported_certificates.is_empty());

    let off = Settings::read(&json!({ "host_certificates": false, "imported_certificates": [] }));
    assert!(!off.host_certificates);
    assert!(off.imported_certificates.is_empty());

    assert_eq!(
        view::certificates_empty_text(),
        "No added root certificates were found on this computer."
    );
}

//! The alternative stores item of DSA Settings for Linux, without a display: the requests it
//! makes through `dsa stores`, what it reads from the answers, what each row says, and that its
//! texts are in every installed language.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure, Problem};
use dsa_settings_linux::model::{AltStore, Client};
use dsa_settings_linux::view;
use serde_json::json;

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

fn answer(aurora_installed: bool) -> serde_json::Value {
    json!({"stores": [
        {"id": "onestore", "name": "ONE store", "package": "com.skt.skaf.A000Z00040",
         "homepage": "https://m.onestore.co.kr/", "installed": false, "version": ""},
        {"id": "aurora", "name": "Aurora Store", "package": "com.aurora.store",
         "homepage": "https://auroraoss.com/", "installed": aurora_installed,
         "version": if aurora_installed { "4.8.4" } else { "" }},
        {"id": "fdroid", "name": "F-Droid", "package": "org.fdroid.fdroid",
         "homepage": "https://f-droid.org/", "installed": false, "version": ""},
    ]})
}

fn client(answer: serde_json::Value) -> (Arc<Fake>, Client) {
    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: answer.to_string(),
    });
    (fake.clone(), Client::new(fake.clone(), fake, false))
}

#[test]
fn fr43_the_item_reads_each_store_and_installs_through_dsa_stores() {
    let (fake, client) = client(answer(true));
    let stores = client.stores().unwrap();
    assert_eq!(
        stores.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["ONE store", "Aurora Store", "F-Droid"]
    );
    assert!(stores[1].installed);
    assert_eq!(stores[1].version, "4.8.4");
    assert!(!stores[1].can_install(false));
    assert!(stores[1].can_open(false));
    assert!(stores[0].can_install(false));
    assert!(!stores[0].can_install(true));
    client.install_store("fdroid").unwrap();
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls[0], ["stores", "status", "--json"]);
    assert_eq!(calls[1], ["stores", "install", "fdroid", "--json"]);
}

#[test]
fn fr43_each_row_says_where_its_store_stands() {
    let stores = AltStore::read_all(&answer(true));
    assert_eq!(
        view::alt_store_status(&stores[0], false, None),
        "Not installed"
    );
    assert_eq!(
        view::alt_store_status(&stores[1], false, None),
        "Installed, version 4.8.4"
    );
    assert_eq!(
        view::alt_store_status(&stores[0], true, None),
        "Downloading and installing\u{2026}"
    );
    let refused = Failure::new(Problem::Refused, "signed by another certificate");
    let text = view::alt_store_status(&stores[2], false, Some(&refused));
    assert!(text.starts_with("Not installed: "), "{text}");
    assert!(text.contains("signed by another certificate"), "{text}");
    // A service from before `dsa stores` still leaves all three offered.
    assert_eq!(AltStore::read_all(&json!({})), AltStore::defaults());
    assert_eq!(AltStore::defaults().len(), 3);
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn catalogue(path: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut entries = BTreeMap::new();
    let mut id: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("msgid ") {
            id = Some(serde_json::from_str(rest.trim()).unwrap());
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            entries.insert(
                id.take().unwrap(),
                serde_json::from_str(rest.trim()).unwrap(),
            );
        }
    }
    entries
}

#[test]
fn fr43_alternative_store_strings_exist_in_every_language() {
    let texts = [
        "Alternative stores",
        "DSA downloads a store from its official site, checks who signed it, and installs it like any other app. DSA does not ship these stores, and each name is a trademark of its owner. Installed stores appear in the app list like any other app, and you uninstall them there.",
        "Installed, version {0}",
        "Downloading and installing\u{2026}",
        "Not installed: {0}",
        "Not installed",
        "Installed",
        "Install",
        "Open",
    ];
    let linguas = std::fs::read_to_string(crate_dir().join("po/LINGUAS")).unwrap();
    for language in linguas.split_whitespace().filter(|l| !l.starts_with('#')) {
        let catalogue = catalogue(&crate_dir().join(format!("po/{language}.po")));
        for text in texts {
            let translated = catalogue
                .get(text)
                .unwrap_or_else(|| panic!("po/{language}.po has no translation of {text:?}"));
            assert!(!translated.is_empty(), "{language}: {text:?}");
            if text.contains("{0}") {
                assert!(translated.contains("{0}"), "{language}: {translated}");
            }
        }
    }
}

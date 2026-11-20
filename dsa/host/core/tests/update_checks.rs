//! The service's update store and its check loop, one turn at a time.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::host_config::UpdateSettings;
use dsa_host_core::settings::{FileSettings, SettingsStore};
use dsa_host_core::update::manifest::{FileEntry, ImageEntry, Manifest, FORMAT};
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::service::{
    open_updates, open_updates_or_disable, Download, DownloadAnnouncer, UpdateCheck, UpdateChecker,
    UpdateNotices, Updates, UpdatesConfig, FIRST_CHECK_DELAY, FIRST_IMAGE_RETRY, SETTINGS_RECHECK,
};
use dsa_host_core::update::store::Fetch;
use dsa_host_core::update::versions::{AndroidVersions, Channel};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "update-checks-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn image(version: &str, major: u32, build: u64) -> InstalledImage {
    InstalledImage {
        version: version.into(),
        android_major: major,
        build,
    }
}

/// A complete older-style image in `images/<version>` of the store at `root`.
fn put_image(root: &std::path::Path, version: &str) {
    let dir = root.join("images").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("system.vhdx"), b"vhdxfile and more").unwrap();
}

fn config<'a>(
    root: &'a std::path::Path,
    first: &'a InstalledImage,
    bundled: Option<&'a InstalledImage>,
) -> UpdatesConfig<'a> {
    UpdatesConfig {
        images_dir: Some(root),
        first_image: Some(first),
        bundled_image: bundled,
        boot_timeout_secs: None,
    }
}

#[test]
fn fr14_no_store_in_the_configuration_is_no_updates() {
    let none = UpdatesConfig {
        images_dir: None,
        first_image: None,
        bundled_image: None,
        boot_timeout_secs: None,
    };
    assert!(open_updates(none, "t").unwrap().is_none());
}

#[test]
fn fr14_the_boot_timeout_defaults_to_five_minutes() {
    let root = scratch("timeout");
    let first = image("16.0.1", 16, 1);
    put_image(&root, "16.0.1");
    let updates = open_updates(config(&root, &first, None), "t")
        .unwrap()
        .unwrap();
    assert_eq!(updates.boot_timeout, Duration::from_secs(300));
    let mut with = config(&root, &first, None);
    with.boot_timeout_secs = Some(30);
    let updates = open_updates(with, "t").unwrap().unwrap();
    assert_eq!(updates.boot_timeout, Duration::from_secs(30));
}

#[test]
fn fr14_bundled_newer_image_becomes_current_on_trial() {
    let root = scratch("bundled");
    let old = image("16.0.1", 16, 1);
    let new = image("16.0.2", 16, 2);
    put_image(&root, "16.0.1");
    // The store has been used with the old image.
    open_updates(config(&root, &old, None), "t").unwrap();
    put_image(&root, "16.0.2");
    let updates = open_updates(config(&root, &old, Some(&new)), "t")
        .unwrap()
        .unwrap();
    let state = updates.store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert!(state.on_trial);
    assert_eq!(state.previous.unwrap().version, "16.0.1");
    // The next start keeps it.
    let updates = open_updates(config(&root, &old, Some(&new)), "t")
        .unwrap()
        .unwrap();
    assert_eq!(updates.store.state().unwrap().current.version, "16.0.2");
}

#[test]
fn fr14_broken_current_image_moves_to_a_usable_one() {
    let root = scratch("broken");
    let first = image("16.0.1", 16, 1);
    let state = r#"{"current": {"version": "16.0.9", "android_major": 16, "build": 9}}"#;
    std::fs::write(root.join("state.json"), state).unwrap();
    // 16.0.9 has no files; 16.0.1, the installer's image, has.
    put_image(&root, "16.0.1");
    let updates = open_updates(config(&root, &first, None), "t")
        .unwrap()
        .unwrap();
    assert_eq!(updates.store.state().unwrap().current.version, "16.0.1");
}

#[test]
fn fr14_no_bundled_image_is_not_reported_broken() {
    // An installation that bundled no image names its first image only; it opens, and the
    // first check downloads one.
    let root = scratch("none");
    let none = image("none", 0, 0);
    let updates = open_updates(config(&root, &none, None), "t")
        .unwrap()
        .unwrap();
    assert!(!updates.store.has_image("none"));
}

#[test]
fn fr14_a_store_that_cannot_be_opened_turns_updates_off_and_says_why() {
    let root = scratch("unopenable");
    let first = image("16.0.1", 16, 1);
    // The state names an image that is not a plain name, which the store refuses.
    std::fs::write(root.join("state.json"), "not json").unwrap();
    let disabled = match open_updates_or_disable(config(&root, &first, None), "t") {
        Err(disabled) => disabled,
        Ok(_) => panic!("a store with a broken state opened"),
    };
    assert!(!disabled.reason.is_empty());
    let why = disabled.report().unwrap_err();
    assert!(
        why.starts_with("image updates are off: ") && why.contains(&disabled.reason),
        "{why}"
    );
    assert!(disabled.choose(Channel::Latest, false).is_err());
    assert!(!disabled.needs_administrator());
}

struct Counting {
    calls: AtomicUsize,
    files: HashMap<String, Vec<u8>>,
}

impl Counting {
    fn nothing() -> Counting {
        Counting {
            calls: AtomicUsize::new(0),
            files: HashMap::new(),
        }
    }
}

impl Fetch for Counting {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let bytes = self
            .files
            .get(url)
            .ok_or_else(|| format!("{url}: HTTP 404"))?;
        out.write_all(bytes).map_err(|e| e.to_string())?;
        Ok(bytes.len() as u64)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[3; 32])
}

fn settings() -> UpdateSettings {
    UpdateSettings {
        list_url: "mem://list.json".into(),
        trusted_keys: vec![hex(signer().verifying_key().as_bytes())],
        check_interval_hours: 24,
        boot_timeout_secs: 300,
        program_list_url: None,
        prerelease_program_list_url: None,
        channel: None,
        program: true,
        program_dir: None,
    }
}

/// A server whose signed list offers 16.0.2.
fn server_of_a_newer_image() -> Counting {
    let contents = b"image 16.0.2".to_vec();
    let url = "mem://images/16.0.2/system.vhdx".to_string();
    let entry = ImageEntry {
        version: "16.0.2".into(),
        android_major: 16,
        build: 2,
        files: vec![FileEntry {
            name: "system.vhdx".into(),
            url: url.clone(),
            size: contents.len() as u64,
            sha256: hex(&Sha256::digest(&contents)),
        }],
    };
    let list = serde_json::to_vec(&Manifest {
        format: FORMAT,
        supported_majors: vec![16],
        images: vec![entry],
    })
    .unwrap();
    let mut files = HashMap::new();
    files.insert(url, contents);
    files.insert(
        "mem://list.json.sig".into(),
        hex(&signer().sign(&list).to_bytes()).into_bytes(),
    );
    files.insert("mem://list.json".into(), list);
    Counting {
        calls: AtomicUsize::new(0),
        files,
    }
}

struct Rig {
    checker: UpdateChecker,
    updates: Updates,
    settings: Arc<FileSettings>,
    base: Instant,
    made: Arc<AtomicUsize>,
}

/// A checker over a store that has its first image (`with_image`) or has none yet.
fn rig(with_image: bool) -> Rig {
    let root = scratch("rig");
    let first = if with_image {
        put_image(&root, "16.0.1");
        image("16.0.1", 16, 1)
    } else {
        image("none", 0, 0)
    };
    let updates = open_updates(config(&root, &first, None), "t")
        .unwrap()
        .unwrap();
    let settings = Arc::new(FileSettings::new(scratch("rig-settings")));
    let made = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&made);
    let base = Instant::now();
    let checker = UpdateChecker::new(
        UpdateCheck {
            settings: settings_for_test(),
            updates: updates.clone(),
            store: Arc::clone(&settings) as Arc<dyn SettingsStore>,
            build: "t".into(),
            on_new_image: Some(Box::new(move || {
                counted.fetch_add(1, Ordering::Relaxed);
            })),
        },
        base,
    )
    .unwrap();
    Rig {
        checker,
        updates,
        settings,
        base,
        made,
    }
}

fn settings_for_test() -> UpdateSettings {
    settings()
}

#[test]
fn fr14_first_image_is_checked_at_once() {
    let mut rig = rig(false);
    assert_eq!(rig.checker.next_check(), rig.base);
    let fetch = Counting::nothing();
    assert!(rig.checker.tick(rig.base, &fetch));
    assert_eq!(fetch.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn fr14_an_image_in_place_is_checked_after_a_minute() {
    let mut rig = rig(true);
    assert_eq!(rig.checker.next_check(), rig.base + FIRST_CHECK_DELAY);
    let fetch = Counting::nothing();
    assert!(!rig.checker.tick(rig.base + Duration::from_secs(1), &fetch));
    assert_eq!(fetch.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn fr14_turned_off_updates_are_rechecked_after_a_minute() {
    let mut rig = rig(true);
    // The image is of the one major the release list supports, so no switch of Android version
    // is waiting; only then is the list left unread (the built-in majors are newer than the
    // test image).
    rig.updates.store.follow_supported(&[16]).unwrap();
    let mut machine = rig.settings.machine();
    machine.image_updates = false;
    rig.settings.set_machine(machine).unwrap();
    let fetch = Counting::nothing();
    let due = rig.base + FIRST_CHECK_DELAY;
    assert!(rig.checker.tick(due, &fetch));
    assert_eq!(
        fetch.calls.load(Ordering::Relaxed),
        0,
        "the list is not read"
    );
    assert_eq!(rig.checker.next_check(), due + SETTINGS_RECHECK);
    assert!(!rig.checker.tick(due + Duration::from_secs(1), &fetch));
    // Turned on again, the next look reads the list.
    machine.image_updates = true;
    rig.settings.set_machine(machine).unwrap();
    assert!(rig.checker.tick(due + SETTINGS_RECHECK, &fetch));
    assert!(fetch.calls.load(Ordering::Relaxed) > 0);
}

#[test]
fn fr14_download_progress_is_announced_once_per_percent() {
    let notices = UpdateNotices::default();
    let entry = ImageEntry {
        version: "16.0.2".into(),
        android_major: 16,
        build: 2,
        files: vec![],
    };
    let mut announcer = DownloadAnnouncer::default();
    let at = |done| Download {
        image: &entry,
        done,
        total: 1000,
    };
    announcer.announce(&at(0), "t", &notices);
    assert!(notices.get().unwrap().contains("0%"));
    notices.set("changed by someone else");
    // The same whole percent does not rewrite the notice.
    announcer.announce(&at(5), "t", &notices);
    assert_eq!(notices.get().as_deref(), Some("changed by someone else"));
    announcer.announce(&at(10), "t", &notices);
    assert!(notices.get().unwrap().contains("1%"));
}

#[test]
fn fr14_first_image_failure_tells_when_it_retries() {
    let mut rig = rig(false);
    let fetch = Counting::nothing();
    assert!(rig.checker.tick(rig.base, &fetch));
    let notice = rig.updates.notices.get().unwrap();
    assert!(
        notice.contains("could not be downloaded yet") && notice.contains("in 5 minutes"),
        "{notice}"
    );
    assert!(rig.checker.next_check() >= rig.base + FIRST_IMAGE_RETRY);
}

#[test]
fn fr14_a_new_image_is_announced_and_the_host_told() {
    let mut rig = rig(true);
    let fetch = server_of_a_newer_image();
    assert!(rig.checker.tick(rig.base + FIRST_CHECK_DELAY, &fetch));
    assert_eq!(rig.made.load(Ordering::Relaxed), 1);
    assert!(rig
        .updates
        .notices
        .get()
        .unwrap()
        .contains("16.0.2 is downloaded"));
    assert_eq!(rig.updates.store.state().unwrap().current.version, "16.0.2");
}

#[test]
fn fr14_a_user_choosing_a_version_is_checked_for_at_once() {
    let mut rig = rig(true);
    rig.updates.check_now.request();
    let fetch = Counting::nothing();
    assert!(rig.checker.tick(rig.base + Duration::from_secs(1), &fetch));
    assert!(fetch.calls.load(Ordering::Relaxed) > 0);
}

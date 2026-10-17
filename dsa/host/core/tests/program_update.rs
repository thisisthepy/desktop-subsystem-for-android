//! Updating DSA itself: the signed program list, the setup download, when setup runs, and
//! what the users are told before and after.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::{UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids};
use dsa_host_core::session::SessionState;
use dsa_host_core::update::manifest;
use dsa_host_core::update::program::{
    self, ProgramDecision, ProgramError, ProgramManifest, SetupFile, Version, FORMAT,
};
use dsa_host_core::update::program_service::{
    CheckOutcome, ProgramInstaller, ProgramSettings, ProgramStatus, ProgramUpdates, RunningSetup,
    Timing, WithProgramStatus,
};
use dsa_host_core::update::store::Fetch;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const LIST: &str = "mem://program.json";
const SETUP_URL: &str = "mem://v0.1.1/setup-0.1.1.exe";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn project_key() -> SigningKey {
    SigningKey::from_bytes(&[5; 32])
}

fn trusted() -> Vec<ed25519_dalek::VerifyingKey> {
    vec![manifest::parse_key(&hex(project_key().verifying_key().as_bytes())).unwrap()]
}

fn list_of(version: &str, setup: &[u8]) -> ProgramManifest {
    ProgramManifest {
        format: FORMAT,
        version: version.into(),
        setup: SetupFile {
            url: SETUP_URL.into(),
            size: setup.len() as u64,
            sha256: hex(&Sha256::digest(setup)),
        },
    }
}

/// Serves files from memory and counts what was fetched.
#[derive(Default, Clone)]
struct MemoryFetch {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    fetched: Arc<Mutex<Vec<String>>>,
}

impl MemoryFetch {
    fn put(&self, url: &str, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(url.into(), bytes);
    }
    /// A signed list offering `list`, with `served` as the setup file.
    fn publish(&self, list: &ProgramManifest, served: &[u8], signer: &SigningKey) {
        let bytes = serde_json::to_vec(list).unwrap();
        self.put(
            &format!("{LIST}.sig"),
            hex(&signer.sign(&bytes).to_bytes()).into(),
        );
        self.put(LIST, bytes);
        self.put(SETUP_URL, served.to_vec());
    }
    fn count(&self, url: &str) -> usize {
        self.fetched
            .lock()
            .unwrap()
            .iter()
            .filter(|u| *u == url)
            .count()
    }
}

impl Fetch for MemoryFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        self.fetched.lock().unwrap().push(url.into());
        let bytes = self
            .files
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| format!("{url}: HTTP 404"))?;
        out.write_all(&bytes).map_err(|e| e.to_string())?;
        Ok(bytes.len() as u64)
    }
}

/// Fails the test if anything is fetched.
struct NeverFetch;

impl Fetch for NeverFetch {
    fn fetch(&self, url: &str, _: &mut dyn Write) -> Result<u64, String> {
        panic!("fetched {url} although program updates are turned off");
    }
}

/// Records each setup it is asked to run. Each setup runs until the test ends it with an
/// exit code; a setup that replaced the service is never ended.
/// A setup that ran: its path, its log and its contents.
type Run = (PathBuf, PathBuf, Vec<u8>);

#[derive(Default, Clone)]
struct FakeInstaller {
    runs: Arc<Mutex<Vec<Run>>>,
    ends: Arc<Mutex<Vec<Sender<u32>>>>,
    refuse: Option<String>,
}

struct FakeSetup(Receiver<u32>);

impl RunningSetup for FakeSetup {
    fn wait(self: Box<Self>) -> Result<u32, String> {
        self.0.recv().map_err(|_| "setup vanished".to_string())
    }
}

impl ProgramInstaller for FakeInstaller {
    fn install(&self, setup: &Path, log: &Path) -> Result<Box<dyn RunningSetup>, String> {
        if let Some(why) = &self.refuse {
            return Err(why.clone());
        }
        let contents = std::fs::read(setup).map_err(|e| e.to_string())?;
        self.runs
            .lock()
            .unwrap()
            .push((setup.to_path_buf(), log.to_path_buf(), contents));
        let (end, ended) = channel();
        self.ends.lock().unwrap().push(end);
        Ok(Box::new(FakeSetup(ended)))
    }
}

impl FakeInstaller {
    fn runs(&self) -> usize {
        self.runs.lock().unwrap().len()
    }
}

fn fresh_dir() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "program-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn settings(root: &Path, running: &str) -> ProgramSettings {
    ProgramSettings {
        list_url: Some(LIST.into()),
        enabled: true,
        keys: trusted(),
        dir: root.join("updates"),
        log_dir: root.join("logs"),
        running: running.into(),
        stop_timeout: Duration::from_secs(5),
        settle: Duration::ZERO,
    }
}

struct Harness {
    root: PathBuf,
    fetch: MemoryFetch,
    installer: FakeInstaller,
    androids: Arc<FakeAndroids>,
}

impl Harness {
    fn new() -> Harness {
        Harness {
            root: fresh_dir(),
            fetch: MemoryFetch::default(),
            installer: FakeInstaller::default(),
            androids: Arc::new(FakeAndroids::default()),
        }
    }

    /// The service's program updates, as a service of version `running` sets them up.
    fn service(&self, running: &str) -> (Arc<ProgramUpdates>, Arc<ProgramStatus>) {
        let status = Arc::new(ProgramStatus::default());
        let updates = ProgramUpdates::new(
            settings(&self.root, running),
            Box::new(self.fetch.clone()),
            Box::new(self.installer.clone()),
            self.androids.clone(),
            Arc::clone(&status),
            "test",
        );
        (updates, status)
    }

    fn android_of(&self, user: &str) -> Arc<dyn UserAndroid> {
        self.androids.android(user).unwrap()
    }

    fn downloads(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.root.join("updates"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name != "state.json")
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

#[test]
fn fr15_versions_compare_as_numbers_and_anything_else_is_refused() {
    let v = |text| Version::parse(text).unwrap();
    assert!(v("0.1.10") > v("0.1.9"));
    assert!(v("1.0") > v("0.99.99"));
    assert_eq!(v("0.1"), v("0.1.0"));
    for bad in ["", "0.1.", ".1", "0.1.0-rc1", "v0.1", "0..1", "../0.1"] {
        let why = Version::parse(bad).unwrap_err();
        assert!(why.contains("dot-separated numbers"), "{bad:?}: {why}");
    }
}

#[test]
fn fr15_an_unsigned_or_wrongly_signed_program_list_is_refused() {
    let list = list_of("0.1.1", b"setup");
    let bytes = serde_json::to_vec(&list).unwrap();
    let other = SigningKey::from_bytes(&[8; 32]);
    let wrong = hex(&other.sign(&bytes).to_bytes());
    for signature in [String::new(), "00".repeat(64), wrong] {
        let error = program::verify(&bytes, &signature, &trusted()).unwrap_err();
        assert!(matches!(error, ProgramError::BadSignature(_)), "{error}");
        assert!(
            error.to_string().contains("not signed by the project"),
            "{error}"
        );
    }
    let good = hex(&project_key().sign(&bytes).to_bytes());
    let mut tampered = bytes.clone();
    tampered[3] ^= 1;
    assert!(matches!(
        program::verify(&tampered, &good, &trusted()),
        Err(ProgramError::BadSignature(_))
    ));
    assert_eq!(program::verify(&bytes, &good, &trusted()).unwrap(), list);

    // Through the service: nothing is downloaded and the reason is reported.
    let h = Harness::new();
    h.fetch.publish(&list, b"setup", &other);
    let (updates, _) = h.service("0.1.0");
    let error = updates.check().unwrap_err();
    assert!(matches!(error, ProgramError::BadSignature(_)), "{error}");
    assert_eq!(
        h.fetch.count(SETUP_URL),
        0,
        "nothing in an unsigned list is used"
    );
    assert!(updates.install_now().is_err());
    assert_eq!(h.installer.runs(), 0);
}

#[test]
fn fr15_a_list_of_an_unknown_format_or_with_a_bad_version_or_hash_is_refused() {
    let mut newer = list_of("0.1.1", b"x");
    newer.format = 2;
    let mut bad_version = list_of("0.1.1", b"x");
    bad_version.version = "0.1.1;rm".into();
    let mut bad_hash = list_of("0.1.1", b"x");
    bad_hash.setup.sha256 = "abc".into();
    for list in [newer, bad_version, bad_hash] {
        let bytes = serde_json::to_vec(&list).unwrap();
        let signature = hex(&project_key().sign(&bytes).to_bytes());
        let result = program::verify(&bytes, &signature, &trusted());
        assert!(
            matches!(result, Err(ProgramError::BadList(_))),
            "{result:?}"
        );
    }
    let unknown_key = br#"{"format":1,"version":"0.1.1","setup":{"url":"u","size":1,
        "sha256":"00000000000000000000000000000000000000000000000000000000000000000"},"x":1}"#;
    let signature = hex(&project_key().sign(unknown_key).to_bytes());
    assert!(program::verify(unknown_key, &signature, &trusted()).is_err());
}

#[test]
fn fr15_a_setup_file_of_the_wrong_size_or_hash_is_not_installed_and_says_why() {
    for (served, says) in [
        (&b"the fake setup"[..], "SHA-256"),
        (&b"short"[..], "bytes"),
        (
            &b"the real setup plus more"[..],
            "longer than the program list says",
        ),
    ] {
        let h = Harness::new();
        h.fetch
            .publish(&list_of("0.1.1", b"the real setup"), served, &project_key());
        let (updates, status) = h.service("0.1.0");
        let error = updates.check().unwrap_err();
        assert!(matches!(error, ProgramError::Download(_)), "{error}");
        assert!(error.to_string().contains(says), "{says}: {error}");
        assert!(h.downloads().is_empty(), "{:?}", h.downloads());
        assert_eq!(updates.install_when_idle().unwrap(), None);
        assert_eq!(h.installer.runs(), 0);
        assert_eq!(status.notices.get(), None);

        // dsa update reports the same reason and runs nothing.
        let why = updates.install_now().unwrap_err();
        assert!(why.to_string().contains(says), "{why}");
        assert_eq!(h.installer.runs(), 0);
    }
}

#[test]
fn fr15_an_equal_or_lower_version_is_never_installed_and_says_why() {
    let list = list_of("0.1.1", b"setup");
    match program::decide("0.1.1", &list).unwrap() {
        ProgramDecision::NotNewer(why) => assert!(why.contains("the version installed"), "{why}"),
        other => panic!("{other:?}"),
    }
    match program::decide("0.1.10", &list).unwrap() {
        ProgramDecision::NotNewer(why) => {
            assert!(why.contains("older than the installed 0.1.10"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        program::decide("0.1.0", &list).unwrap(),
        ProgramDecision::Install(_)
    ));
    let why = program::decide("dev-build", &list).unwrap_err();
    assert!(why.to_string().contains("cannot be compared"), "{why}");

    for running in ["0.1.1", "0.2.0"] {
        let h = Harness::new();
        h.fetch.publish(&list, b"setup", &project_key());
        let (updates, _) = h.service(running);
        match updates.check().unwrap() {
            CheckOutcome::NotNewer(why) => assert!(why.contains("offers DSA 0.1.1"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(h.fetch.count(SETUP_URL), 0);
        let answer = updates.install_now().unwrap();
        assert_eq!(answer.installing, None);
        assert!(answer.detail.contains("up to date"), "{}", answer.detail);
        assert_eq!(h.installer.runs(), 0);
    }
}

#[test]
fn fr15_turned_off_the_program_list_is_never_fetched() {
    let root = fresh_dir();
    let mut off = settings(&root, "0.1.0");
    off.enabled = false;
    let installer = FakeInstaller::default();
    let updates = ProgramUpdates::new(
        off,
        Box::new(NeverFetch),
        Box::new(installer.clone()),
        Arc::new(FakeAndroids::default()),
        Arc::new(ProgramStatus::default()),
        "test",
    );
    assert!(matches!(updates.check(), Err(ProgramError::Off(_))));
    assert_eq!(updates.install_when_idle().unwrap(), None);
    let why = updates.install_now().unwrap_err();
    assert!(why.to_string().contains("turned off"), "{why}");
    let stop = AtomicBool::new(false);
    let timing = Timing {
        first_check: Duration::ZERO,
        check_interval: Duration::from_millis(10),
        retry: Duration::from_millis(10),
    };
    // Returns at once: there is nothing to do while updates are off.
    updates.run(&stop, timing);
    assert_eq!(installer.runs(), 0);
}

#[test]
fn fr15_an_update_waits_while_an_android_runs_and_installs_when_all_stop() {
    let h = Harness::new();
    h.fetch.publish(
        &list_of("0.1.1", b"setup 0.1.1"),
        b"setup 0.1.1",
        &project_key(),
    );
    let (updates, status) = h.service("0.1.0");
    let alice = h.android_of("alice");
    let bob = h.android_of("bob");
    alice.start();

    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.1.1".into())
    );
    let told = status.notices.get().unwrap();
    assert!(
        told.contains("DSA 0.1.1 is downloaded and installs when Android is not running"),
        "{told}"
    );
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::AlreadyDownloaded("0.1.1".into()),
        "a second check does not download again"
    );
    assert_eq!(h.fetch.count(SETUP_URL), 1);

    let stop = Arc::new(AtomicBool::new(false));
    let runner = {
        let (updates, stop) = (Arc::clone(&updates), Arc::clone(&stop));
        thread::spawn(move || {
            updates.run(
                &stop,
                Timing {
                    first_check: Duration::from_secs(3600),
                    check_interval: Duration::from_secs(3600),
                    retry: Duration::from_millis(20),
                },
            )
        })
    };
    thread::sleep(Duration::from_millis(200));
    assert_eq!(h.installer.runs(), 0, "alice's Android is running");
    assert_eq!(
        alice.state(),
        SessionState::Active,
        "nobody's Android is stopped for it"
    );

    alice.stop();
    bob.stop();
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.installer.runs() == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    runner.join().unwrap();
    assert_eq!(
        h.installer.runs(),
        1,
        "every Android stopped, so setup runs"
    );
    let (setup, log, contents) = h.installer.runs.lock().unwrap()[0].clone();
    assert_eq!(contents, b"setup 0.1.1");
    assert_eq!(setup.file_name().unwrap(), "setup-0.1.1.exe");
    assert_eq!(log, h.root.join("logs").join("setup-0.1.1.log"));
    assert_eq!(status.installing().as_deref(), Some("0.1.1"));
    let told = status.notices.get().unwrap();
    assert!(told.contains("installing DSA 0.1.1"), "{told}");
}

#[test]
fn fr15_no_android_starts_while_setup_runs_and_the_user_is_told_why() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"s"), b"s", &project_key());
    let (updates, status) = h.service("0.1.0");
    let android = WithProgramStatus {
        android: Arc::new(FakeAndroid::new()),
        status: Arc::clone(&status),
    };
    updates.check().unwrap();
    assert_eq!(
        updates.install_when_idle().unwrap().as_deref(),
        Some("0.1.1")
    );
    android.start();
    assert_eq!(android.state(), SessionState::Stopped);
    let why = android.last_reason().unwrap();
    assert!(why.contains("DSA 0.1.1 is being installed"), "{why}");
    let notice = android.update_notice().unwrap();
    assert!(notice.contains("installing DSA 0.1.1"), "{notice}");
}

#[test]
fn fr15_dsa_update_installs_at_once_stopping_every_android() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, status) = h.service("0.1.0");
    let alice = h.android_of("alice");
    let bob = h.android_of("bob");
    alice.start();
    bob.start();
    updates.check().unwrap();
    assert_eq!(
        updates.install_when_idle().unwrap(),
        None,
        "both are running"
    );

    let answer = updates.install_now().unwrap();
    assert_eq!(answer.installing.as_deref(), Some("0.1.1"));
    assert!(
        answer.detail.contains("installing DSA 0.1.1"),
        "{}",
        answer.detail
    );
    assert_eq!(alice.state(), SessionState::Stopped);
    assert_eq!(bob.state(), SessionState::Stopped);
    assert_eq!(h.installer.runs(), 1);
    assert_eq!(status.installing().as_deref(), Some("0.1.1"));

    let again = updates.install_now().unwrap();
    assert!(again.detail.contains("already"), "{}", again.detail);
    assert_eq!(h.installer.runs(), 1, "one setup at a time");
}

#[test]
fn fr15_dsa_update_downloads_first_when_nothing_is_downloaded() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, _) = h.service("0.1.0");
    let answer = updates.install_now().unwrap();
    assert_eq!(answer.installing.as_deref(), Some("0.1.1"));
    assert_eq!(h.fetch.count(SETUP_URL), 1);
    assert_eq!(h.installer.runs(), 1);
}

#[test]
fn fr15_after_setup_the_new_service_reports_the_update() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    // Setup stopped the old service and started the new one.
    drop(old);
    let (_new, status) = h.service("0.1.1");
    assert_eq!(
        status.notices.get().as_deref(),
        Some("DSA was updated to 0.1.1")
    );
    assert!(
        h.downloads().is_empty(),
        "the setup file is removed: {:?}",
        h.downloads()
    );
    // Reported once: the next restart has nothing to say.
    let (_later, status) = h.service("0.1.1");
    assert_eq!(status.notices.get(), None);
}

#[test]
fn fr15_when_setup_fails_the_old_version_reports_why_and_does_not_retry_on_its_own() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    drop(old);
    // Setup failed and rolled back: the old version starts again.
    let (again, status) = h.service("0.1.0");
    let told = status.notices.get().unwrap();
    let log = h.root.join("logs").join("setup-0.1.1.log");
    assert_eq!(
        told,
        format!(
            "updating DSA to 0.1.1 failed (see {}); DSA 0.1.0 is still installed",
            log.display()
        )
    );
    match again.check().unwrap() {
        CheckOutcome::FailedBefore(why) => assert!(why.contains("dsa update"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(again.install_when_idle().unwrap(), None);
    assert_eq!(h.installer.runs(), 1);
    // The user can still ask for it.
    assert_eq!(
        again.install_now().unwrap().installing.as_deref(),
        Some("0.1.1")
    );
    assert_eq!(h.installer.runs(), 2);
    assert_eq!(
        h.fetch.count(SETUP_URL),
        1,
        "the download is kept for a retry"
    );
}

#[test]
fn fr15_a_setup_that_ends_without_replacing_the_service_is_reported_as_failed() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, status) = h.service("0.1.0");
    updates.install_now().unwrap();
    h.installer.ends.lock().unwrap()[0].send(1603).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while status.installing().is_some() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(status.installing(), None, "Android may start again");
    let told = status.notices.get().unwrap();
    assert!(told.starts_with("updating DSA to 0.1.1 failed"), "{told}");
    assert!(told.contains("exit code 1603"), "{told}");
    assert!(told.contains("DSA 0.1.0 is still installed"), "{told}");
}

#[test]
fn fr15_a_setup_that_cannot_start_is_reported_and_android_may_start_again() {
    let mut h = Harness::new();
    h.installer = FakeInstaller {
        refuse: Some("access denied".into()),
        ..FakeInstaller::default()
    };
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, status) = h.service("0.1.0");
    let why = updates.install_now().unwrap_err();
    assert!(why.to_string().contains("access denied"), "{why}");
    assert_eq!(status.installing(), None);
    let told = status.notices.get().unwrap();
    assert!(told.contains("updating DSA to 0.1.1 failed"), "{told}");
}

#[test]
fn fr15_a_setup_file_changed_after_download_is_not_run() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, _) = h.service("0.1.0");
    updates.check().unwrap();
    let file = h.root.join("updates").join("0.1.1").join("setup-0.1.1.exe");
    std::fs::write(&file, b"swapped").unwrap();
    assert!(updates.install_when_idle().is_err());
    assert_eq!(h.installer.runs(), 0);
}

#[test]
fn fr15_a_newer_list_replaces_an_older_download() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"one"), b"one", &project_key());
    let (updates, _) = h.service("0.1.0");
    updates.check().unwrap();
    h.fetch
        .publish(&list_of("0.1.2", b"two"), b"two", &project_key());
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.1.2".into())
    );
    assert_eq!(h.downloads(), vec!["0.1.2".to_string()]);
}

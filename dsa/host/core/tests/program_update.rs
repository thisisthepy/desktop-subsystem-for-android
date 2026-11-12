//! Updating DSA itself: the signed program list, the setup download, when setup runs, and
//! what the users are told before and after.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::{ProgramPhase, ProgramProgress, UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids};
use dsa_host_core::input::{PointerAction, PointerEvent, PointerTool};
use dsa_host_core::session::SessionState;
use dsa_host_core::update::manifest;
use dsa_host_core::update::program::{
    self, ProgramDecision, ProgramError, ProgramManifest, ReleaseChannel, SetupFile, Version,
    FORMAT,
};
use dsa_host_core::update::program_service::{
    after_setup, restart_into_files, AfterSetup, CheckOutcome, ProgramInstaller, ProgramSettings,
    ProgramStatus, ProgramUpdates, RunningSetup, SetupOutcome, Timing, WithProgramStatus,
};
use dsa_host_core::update::store::Fetch;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const LIST: &str = "mem://program.json";
const SETUP_URL: &str = "mem://v0.1.1/setup-0.1.1.exe";
/// The pre-release channel's list, beside [`LIST`], which is the stable channel's.
const PRERELEASE_LIST: &str = "mem://program-prerelease.json";

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
    /// A signed list at `list_url` offering `version`, whose setup file is `served` at a URL
    /// of its own.
    fn publish_at(&self, list_url: &str, version: &str, served: &[u8]) {
        let setup_url = format!("mem://v{version}/setup-{version}.exe");
        let list = ProgramManifest {
            format: FORMAT,
            version: version.into(),
            setup: SetupFile {
                url: setup_url.clone(),
                size: served.len() as u64,
                sha256: hex(&Sha256::digest(served)),
            },
        };
        let bytes = serde_json::to_vec(&list).unwrap();
        self.put(
            &format!("{list_url}.sig"),
            hex(&project_key().sign(&bytes).to_bytes()).into(),
        );
        self.put(list_url, bytes);
        self.put(&setup_url, served.to_vec());
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
    /// How each setup ends: its exit code once the test ends it.
    setups: Arc<Mutex<Vec<Arc<Ending>>>>,
    refuse: Option<String>,
    /// Setups carry an id a later service can find them by, as on Windows.
    named: bool,
    /// The version of the program files, as setup leaves them.
    on_disk: Arc<Mutex<Option<String>>>,
    /// Some other installation is running.
    elsewhere: Arc<AtomicBool>,
    /// How often the service asked to be restarted into the program files.
    restarts: Arc<AtomicU32>,
    /// The handles services wait on setups with, until setup stops those services.
    waiters: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
}

#[derive(Default)]
struct Ending {
    code: Mutex<Option<u32>>,
    ended: Condvar,
}

struct FakeSetup {
    ending: Arc<Ending>,
    id: Option<String>,
    /// Set when the service that waits on this handle has stopped, as setup stops it.
    gone: Arc<AtomicBool>,
}

impl RunningSetup for FakeSetup {
    fn wait(self: Box<Self>) -> Result<u32, String> {
        let mut code = self.ending.code.lock().unwrap();
        loop {
            if self.gone.load(Ordering::SeqCst) {
                return Err("the service waiting for setup stopped".into());
            }
            if let Some(code) = *code {
                return Ok(code);
            }
            code = self.ending.ended.wait(code).unwrap();
        }
    }
    fn id(&self) -> Option<String> {
        self.id.clone()
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
        let ending = Arc::new(Ending::default());
        let mut setups = self.setups.lock().unwrap();
        let id = self.named.then(|| format!("setup-{}", setups.len()));
        setups.push(Arc::clone(&ending));
        Ok(Box::new(FakeSetup {
            ending,
            id,
            gone: self.waiter(),
        }))
    }
    /// As Windows Installer's codes read: 3010 installed, with a restart to finish.
    fn outcome(&self, code: u32) -> SetupOutcome {
        match code {
            0 => SetupOutcome::Installed,
            3010 => SetupOutcome::InstalledRestartPending,
            _ => SetupOutcome::Failed,
        }
    }
    fn reattach(&self, id: &str) -> Option<Box<dyn RunningSetup>> {
        let index: usize = id.strip_prefix("setup-")?.parse().ok()?;
        let ending = Arc::clone(self.setups.lock().unwrap().get(index)?);
        if ending.code.lock().unwrap().is_some() {
            return None;
        }
        Some(Box::new(FakeSetup {
            ending,
            id: Some(id.to_string()),
            gone: self.waiter(),
        }))
    }
    fn installed_version(&self) -> Option<String> {
        self.on_disk.lock().unwrap().clone()
    }
    fn installation_running(&self) -> bool {
        self.elsewhere.load(Ordering::SeqCst)
    }
    fn restart_into_installed(&self) -> Result<(), String> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl FakeInstaller {
    fn runs(&self) -> usize {
        self.runs.lock().unwrap().len()
    }
    /// Ends setup number `index` with `code`.
    fn end(&self, index: usize, code: u32) {
        let ending = Arc::clone(&self.setups.lock().unwrap()[index]);
        *ending.code.lock().unwrap() = Some(code);
        ending.ended.notify_all();
    }
    fn waiter(&self) -> Arc<AtomicBool> {
        let gone = Arc::new(AtomicBool::new(false));
        self.waiters.lock().unwrap().push(Arc::clone(&gone));
        gone
    }
    /// Setup stopped the service: whatever that service was waiting on ends with it.
    fn service_stopped(&self) {
        for gone in self.waiters.lock().unwrap().drain(..) {
            gone.store(true, Ordering::SeqCst);
        }
        for ending in self.setups.lock().unwrap().iter() {
            let _held = ending.code.lock().unwrap();
            ending.ended.notify_all();
        }
    }
    fn files_are(&self, version: &str) {
        *self.on_disk.lock().unwrap() = Some(version.to_string());
    }
    fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::SeqCst)
    }
}

/// Waits up to ten seconds for `done`.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Starts the service's loop on a thread of its own, as the service does.
fn run_in_background(
    updates: &Arc<ProgramUpdates>,
    stop: &Arc<AtomicBool>,
    timing: Timing,
) -> thread::JoinHandle<()> {
    let (updates, stop) = (Arc::clone(updates), Arc::clone(stop));
    thread::spawn(move || updates.run(&stop, timing))
}

/// Joins the service's loop, and fails the test with `what` if it has not ended within
/// ten seconds of being stopped, instead of leaving the whole test run waiting for it.
fn join_within(worker: thread::JoinHandle<()>, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.is_finished() {
        assert!(
            Instant::now() < deadline,
            "the update loop did not end within 10 seconds of being stopped: {what}"
        );
        thread::sleep(Duration::from_millis(10));
    }
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
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
        prerelease_list_url: None,
        channel: ReleaseChannel::Stable,
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
    for bad in [
        "",
        "0.1.",
        ".1",
        "0.1.0-rc1",
        "v0.1",
        "0..1",
        "../0.1",
        "-dev.1",
        "0.1.0-dev.",
        "0.1.0-dev.x",
        "0.1.0-dev.1-dev.2",
    ] {
        let why = Version::parse(bad).unwrap_err();
        assert!(why.contains("dot-separated numbers"), "{bad:?}: {why}");
    }
}

#[test]
fn fr15_a_development_build_is_newer_than_the_last_release_and_older_than_the_next() {
    let v = |text| Version::parse(text).unwrap();
    assert!(v("0.2.4-dev.7").is_development());
    assert!(!v("0.2.4").is_development());
    assert!(v("0.2.4-dev.7") > v("0.2.3"));
    assert!(v("0.2.4-dev.7") > v("0.2.4-dev.6"));
    assert!(v("0.2.4-dev.7") < v("0.2.4"));
    assert!(v("0.2.4-dev.7") < v("0.2.4.1"));
    assert_eq!(v("0.2.4-dev.7"), v("0.2.4.0-dev.7"));
    assert_eq!(v("0.2.4-dev.7").to_string(), "0.2.4-dev.7");
    // A development build never takes the release it leads up to for an older version, and
    // never moves down to the release before it.
    let decide = |running: &str, offered: &str| {
        program::decide(running, &list_of(offered, b"setup")).unwrap()
    };
    assert!(matches!(
        decide("0.2.4-dev.7", "0.2.4"),
        ProgramDecision::Install(_)
    ));
    assert!(matches!(
        decide("0.2.4-dev.7", "0.2.3"),
        ProgramDecision::NotNewer(_)
    ));
    // A pre-release below 1.0, like every other version below 1.0.
    assert_eq!(
        ReleaseChannel::of_version("0.2.4-dev.7"),
        ReleaseChannel::Prerelease
    );
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
    // Turned off, the loop still runs, to notice program files that a setup run by hand
    // replaced under the service; it fetches nothing (`NeverFetch` would panic the loop)
    // and runs no setup, and it ends when the service stops it.
    let stop = Arc::new(AtomicBool::new(false));
    let worker = run_in_background(
        &updates,
        &stop,
        Timing {
            first_check: Duration::ZERO,
            check_interval: Duration::from_millis(10),
            retry: Duration::from_millis(10),
        },
    );
    thread::sleep(Duration::from_millis(100));
    assert!(
        !worker.is_finished(),
        "the loop keeps looking for replaced program files while updates are off"
    );
    stop.store(true, Ordering::Relaxed);
    join_within(worker, "program updates turned off in the configuration");
    assert_eq!(installer.runs(), 0);
    assert_eq!(installer.restarts(), 0);
}

#[test]
fn fr17_turned_off_in_the_settings_the_program_list_is_not_fetched_until_turned_on() {
    let root = fresh_dir();
    let installer = FakeInstaller::default();
    let updates = ProgramUpdates::new(
        settings(&root, "0.1.0"),
        Box::new(NeverFetch),
        Box::new(installer.clone()),
        Arc::new(FakeAndroids::default()),
        Arc::new(ProgramStatus::default()),
        "test",
    );
    let on = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&on);
    updates.follow_switch(Box::new(move || switch.load(Ordering::Relaxed)));
    let why = updates.check().unwrap_err();
    assert!(
        why.to_string().contains("turned off in the settings"),
        "{why}"
    );
    assert_eq!(updates.install_when_idle().unwrap(), None);
    assert!(updates.install_now().is_err());

    // The loop keeps running while the settings have updates off, and fetches nothing.
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (updates, stop) = (Arc::clone(&updates), Arc::clone(&stop));
        thread::spawn(move || {
            updates.run(
                &stop,
                Timing {
                    first_check: Duration::ZERO,
                    check_interval: Duration::from_millis(10),
                    retry: Duration::from_millis(10),
                },
            )
        })
    };
    thread::sleep(Duration::from_millis(100));
    assert!(
        !worker.is_finished(),
        "the loop waits for the settings to turn updates on"
    );
    stop.store(true, Ordering::Relaxed);
    join_within(worker, "program updates turned off in the settings");
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
    join_within(runner, "an update waiting for every Android to stop");
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
fn fr5_pointer_input_reaches_an_android_through_the_updating_service() {
    // The installed service wraps each user's Android to report program updates; input in
    // a task window must pass through the wrapper, not stop at it.
    let h = Harness::new();
    let (_updates, status) = h.service("0.1.0");
    let inner = Arc::new(FakeAndroid::new());
    inner.start();
    let android = WithProgramStatus {
        android: Arc::clone(&inner) as Arc<dyn UserAndroid>,
        status,
    };
    let click = PointerEvent::new(5, PointerAction::Down, PointerTool::Mouse, (330.0, 56.0));
    android.pointer(&click).unwrap();
    assert_eq!(inner.pointers.lock().unwrap().as_slice(), &[click]);
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
fn fr15_setup_that_installed_and_asks_for_a_restart_is_no_failure() {
    // Setup said it installed, and asked for a restart because a file was in use.
    assert_eq!(
        after_setup(SetupOutcome::InstalledRestartPending, 3010, "0.1.10", None),
        AfterSetup::RestartIntoInstalled
    );
    assert_eq!(
        after_setup(SetupOutcome::Installed, 0, "0.1.10", Some("0.1.10.0")),
        AfterSetup::RestartIntoInstalled
    );
    // The files say otherwise: setup did not put the new version in place.
    match after_setup(SetupOutcome::Installed, 0, "0.1.10", Some("0.1.9.0")) {
        AfterSetup::Failed(why) => assert!(why.contains("still DSA 0.1.9.0"), "{why}"),
        other => panic!("{other:?}"),
    }
    match after_setup(SetupOutcome::Failed, 1603, "0.1.10", Some("0.1.10.0")) {
        AfterSetup::Failed(why) => assert!(why.contains("exit code 1603"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr15_a_service_still_running_after_setup_installed_restarts_into_the_new_version() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, status) = h.service("0.1.0");
    old.install_now().unwrap();
    // Setup replaced the files but not the running service, and ended with 3010.
    h.installer.files_are("0.1.1.0");
    h.installer.end(0, 3010);
    assert!(
        eventually(|| h.installer.restarts() == 1),
        "the service restarts"
    );
    let told = status.notices.get().unwrap_or_default();
    assert!(!told.contains("failed"), "no failure is reported: {told}");
    assert_eq!(
        status.installing().as_deref(),
        Some("0.1.1"),
        "no Android starts before the restart"
    );
    // The service that starts from the new files reports the update.
    drop(old);
    let (_new, status) = h.service("0.1.1");
    assert_eq!(
        status.notices.get().as_deref(),
        Some("DSA was updated to 0.1.1")
    );
}

#[test]
fn fr15_a_service_started_while_setup_runs_waits_for_setup_instead_of_reporting_failure() {
    let mut h = Harness::new();
    h.installer.named = true;
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    // Setup stopped the service, and something started the old one again before setup had
    // replaced its files.
    drop(old);
    h.installer.service_stopped();
    let (again, status) = h.service("0.1.0");
    assert_eq!(
        status.notices.get(),
        None,
        "nothing has failed while setup runs"
    );
    assert_eq!(status.installing().as_deref(), Some("0.1.1"));
    let alice = h.android_of("alice");
    WithProgramStatus {
        android: Arc::clone(&alice),
        status: Arc::clone(&status),
    }
    .start();
    assert_eq!(
        alice.state(),
        SessionState::Stopped,
        "no Android starts while setup runs"
    );
    // Setup ends having installed: the old service restarts into the new files.
    h.installer.files_are("0.1.1.0");
    h.installer.end(0, 0);
    assert!(eventually(|| h.installer.restarts() == 1));
    assert_eq!(status.notices.get(), None);
    drop(again);
}

#[test]
fn fr15_a_service_started_while_setup_runs_reports_why_once_setup_fails() {
    let mut h = Harness::new();
    h.installer.named = true;
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    drop(old);
    h.installer.service_stopped();
    let (_again, status) = h.service("0.1.0");
    h.installer.end(0, 1603);
    assert!(eventually(|| status.installing().is_none()));
    let told = status.notices.get().unwrap();
    assert!(told.starts_with("updating DSA to 0.1.1 failed"), "{told}");
    assert!(told.contains("exit code 1603"), "{told}");
    assert_eq!(h.installer.restarts(), 0);
}

#[test]
fn fr15_an_update_taken_for_failed_is_reported_applied_once_the_new_version_runs() {
    // As DSA 0.1.9 did: the old service came back while setup ran, found its own version
    // and reported the update as failed, though setup went on to install it.
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    drop(old);
    h.installer.service_stopped();
    let (stale, status) = h.service("0.1.0");
    assert!(status.notices.get().unwrap().contains("failed"));
    drop(stale);
    // The service from the new files starts.
    let (new, status) = h.service("0.1.1");
    assert_eq!(
        status.notices.get().as_deref(),
        Some("DSA was updated to 0.1.1")
    );
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    assert!(
        matches!(new.check().unwrap(), CheckOutcome::NotNewer(_)),
        "nothing is left to install"
    );
}

#[test]
fn fr15_program_files_newer_than_the_service_restart_it_into_them_once() {
    assert!(restart_into_files("0.1.9", Some("0.1.10.0"), false));
    assert!(!restart_into_files("0.1.10", Some("0.1.10.0"), false));
    assert!(!restart_into_files("0.1.10", Some("0.1.9.0"), false));
    assert!(
        !restart_into_files("0.1.9", Some("0.1.10.0"), true),
        "not while an installation may still be replacing them"
    );
    assert!(!restart_into_files("0.1.9", None, false));

    // Through the service: the files were replaced under it while it ran.
    let h = Harness::new();
    let (updates, _) = h.service("0.1.0");
    h.installer.elsewhere.store(true, Ordering::SeqCst);
    h.installer.files_are("0.1.1.0");
    assert!(!updates.restart_if_replaced(), "setup still runs");
    h.installer.elsewhere.store(false, Ordering::SeqCst);
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
    assert!(eventually(|| h.installer.restarts() == 1));
    thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::Relaxed);
    join_within(runner, "a service whose program files were replaced");
    assert_eq!(h.installer.restarts(), 1, "asked once");
}

#[test]
fn fr15_a_setup_that_ends_without_replacing_the_service_is_reported_as_failed() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, status) = h.service("0.1.0");
    updates.install_now().unwrap();
    h.installer.end(0, 1603);
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

#[test]
fn fr15_a_setup_file_still_in_use_after_the_update_is_removed_later() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    drop(old);
    let (new, status) = h.service("0.1.1");
    assert_eq!(
        status.notices.get().as_deref(),
        Some("DSA was updated to 0.1.1")
    );
    // Setup was still running from its file when the new service started, so the file
    // could not go then; the next round of the service removes it.
    let leftover = h.root.join("updates").join("0.1.1");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("setup-0.1.1.exe"), b"setup").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let runner = {
        let (new, stop) = (Arc::clone(&new), Arc::clone(&stop));
        thread::spawn(move || {
            new.run(
                &stop,
                Timing {
                    first_check: Duration::from_secs(3600),
                    check_interval: Duration::from_secs(3600),
                    retry: Duration::from_millis(20),
                },
            )
        })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while leftover.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    join_within(runner, "removing a leftover setup file");
    assert!(!leftover.exists(), "the leftover setup file is removed");
}

/// Program updates that read both lists, on the channel `channel` holds.
fn channel_service(
    h: &Harness,
    running: &str,
    channel: &Arc<Mutex<ReleaseChannel>>,
) -> (Arc<ProgramUpdates>, Arc<ProgramStatus>) {
    let status = Arc::new(ProgramStatus::default());
    let mut settings = settings(&h.root, running);
    settings.prerelease_list_url = Some(PRERELEASE_LIST.into());
    let updates = ProgramUpdates::new(
        settings,
        Box::new(h.fetch.clone()),
        Box::new(h.installer.clone()),
        h.androids.clone(),
        Arc::clone(&status),
        "test",
    );
    let followed = Arc::clone(channel);
    updates.follow_channel(Box::new(move || *followed.lock().unwrap()));
    (updates, status)
}

#[test]
fn fr15_channels_are_named_stable_and_prerelease_and_below_1_0_every_release_is_a_prerelease() {
    for channel in [ReleaseChannel::Stable, ReleaseChannel::Prerelease] {
        assert_eq!(ReleaseChannel::parse(channel.as_str()).unwrap(), channel);
    }
    assert_eq!(
        ReleaseChannel::parse("Pre-release").unwrap(),
        ReleaseChannel::Prerelease
    );
    assert!(ReleaseChannel::parse("beta").is_err());
    assert_eq!(
        ReleaseChannel::of_version("0.1.9"),
        ReleaseChannel::Prerelease
    );
    assert_eq!(
        ReleaseChannel::of_version("0.99.99"),
        ReleaseChannel::Prerelease
    );
    assert_eq!(ReleaseChannel::of_version("1.0.0"), ReleaseChannel::Stable);
    assert_eq!(ReleaseChannel::of_version("1.2"), ReleaseChannel::Stable);
}

#[test]
fn fr15_the_stable_channel_reads_the_stable_list_alone_and_the_prerelease_channel_the_higher() {
    let stable = list_of("0.3.0", b"s");
    let prerelease = list_of("0.4.0", b"p");
    fn pick(
        channel: ReleaseChannel,
        stable: Option<&ProgramManifest>,
        prerelease: Option<&ProgramManifest>,
    ) -> Option<(ReleaseChannel, String)> {
        program::choose(channel, stable, prerelease)
            .map(|(from, list)| (from, list.version.clone()))
    }
    assert_eq!(
        pick(ReleaseChannel::Stable, Some(&stable), Some(&prerelease)),
        Some((ReleaseChannel::Stable, "0.3.0".to_string()))
    );
    assert_eq!(pick(ReleaseChannel::Stable, None, Some(&prerelease)), None);
    assert_eq!(
        pick(ReleaseChannel::Prerelease, Some(&stable), Some(&prerelease)),
        Some((ReleaseChannel::Prerelease, "0.4.0".to_string()))
    );
    let newer_stable = list_of("0.5.0", b"s");
    assert_eq!(
        pick(
            ReleaseChannel::Prerelease,
            Some(&newer_stable),
            Some(&prerelease)
        ),
        Some((ReleaseChannel::Stable, "0.5.0".to_string()))
    );
    let same = list_of("0.4.0", b"s");
    assert_eq!(
        pick(ReleaseChannel::Prerelease, Some(&same), Some(&prerelease)),
        Some((ReleaseChannel::Stable, "0.4.0".to_string())),
        "the same version from both lists counts as stable"
    );
    assert_eq!(
        pick(ReleaseChannel::Prerelease, None, Some(&prerelease)),
        Some((ReleaseChannel::Prerelease, "0.4.0".to_string()))
    );
}

#[test]
fn fr15_the_stable_channel_does_not_apply_a_version_only_the_prerelease_list_offers() {
    let h = Harness::new();
    h.fetch.publish_at(LIST, "0.1.1", b"stable 0.1.1");
    h.fetch.publish_at(PRERELEASE_LIST, "0.2.0", b"pre 0.2.0");
    let channel = Arc::new(Mutex::new(ReleaseChannel::Stable));
    let (updates, _) = channel_service(&h, "0.1.0", &channel);
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.1.1".into())
    );
    assert_eq!(
        h.fetch.count(PRERELEASE_LIST),
        0,
        "the stable channel never reads it"
    );
    assert_eq!(
        updates.install_when_idle().unwrap().as_deref(),
        Some("0.1.1")
    );
    assert_eq!(h.installer.runs.lock().unwrap()[0].2, b"stable 0.1.1");

    // Only the pre-release list offers anything newer: nothing to do on the stable channel.
    let h = Harness::new();
    h.fetch.publish_at(PRERELEASE_LIST, "0.2.0", b"pre 0.2.0");
    h.fetch.publish_at(LIST, "0.1.0", b"stable 0.1.0");
    let (updates, _) = channel_service(&h, "0.1.0", &channel);
    match updates.check().unwrap() {
        CheckOutcome::NotNewer(why) => assert!(why.contains("stable channel"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(updates.install_now().unwrap().installing.is_none());
    assert_eq!(h.installer.runs(), 0);
}

#[test]
fn fr15_the_prerelease_channel_applies_a_version_only_the_prerelease_list_offers() {
    // Before 1.0 there is no stable list at all; the pre-release list alone is followed.
    let h = Harness::new();
    h.fetch.publish_at(PRERELEASE_LIST, "0.2.0", b"pre 0.2.0");
    let channel = Arc::new(Mutex::new(ReleaseChannel::Prerelease));
    let (updates, _) = channel_service(&h, "0.1.0", &channel);
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.2.0".into())
    );
    assert_eq!(
        updates.install_when_idle().unwrap().as_deref(),
        Some("0.2.0")
    );
    assert_eq!(h.installer.runs.lock().unwrap()[0].2, b"pre 0.2.0");
}

#[test]
fn fr15_with_both_lists_the_prerelease_channel_applies_the_higher_version() {
    for (stable, prerelease, wanted) in [("0.3.0", "0.2.0", "0.3.0"), ("0.3.0", "0.4.0", "0.4.0")] {
        let h = Harness::new();
        h.fetch.publish_at(LIST, stable, stable.as_bytes());
        h.fetch
            .publish_at(PRERELEASE_LIST, prerelease, prerelease.as_bytes());
        let channel = Arc::new(Mutex::new(ReleaseChannel::Prerelease));
        let (updates, _) = channel_service(&h, "0.1.0", &channel);
        assert_eq!(
            updates.check().unwrap(),
            CheckOutcome::Downloaded(wanted.into())
        );
        updates.install_when_idle().unwrap();
        assert_eq!(h.installer.runs.lock().unwrap()[0].2, wanted.as_bytes());
    }
}

#[test]
fn fr15_switching_from_prerelease_to_stable_keeps_the_version_until_stable_passes_it() {
    // 0.4.0 came from the pre-release channel; the stable list is behind it.
    let h = Harness::new();
    h.fetch.publish_at(LIST, "0.3.0", b"stable 0.3.0");
    h.fetch.publish_at(PRERELEASE_LIST, "0.4.0", b"pre 0.4.0");
    let channel = Arc::new(Mutex::new(ReleaseChannel::Stable));
    let (updates, _) = channel_service(&h, "0.4.0", &channel);
    match updates.check().unwrap() {
        CheckOutcome::NotNewer(why) => {
            assert!(why.contains("never moved to a lower version"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    let answer = updates.install_now().unwrap();
    assert!(answer.installing.is_none(), "{}", answer.detail);
    assert_eq!(h.installer.runs(), 0, "nothing lower is installed");

    // The stable list passes it: that version is installed.
    h.fetch.publish_at(LIST, "0.5.0", b"stable 0.5.0");
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.5.0".into())
    );
    assert_eq!(
        updates.install_when_idle().unwrap().as_deref(),
        Some("0.5.0")
    );
}

#[test]
fn fr15_a_prerelease_waiting_to_install_is_dropped_when_the_computer_moves_to_stable() {
    let h = Harness::new();
    h.fetch.publish_at(LIST, "0.1.0", b"stable 0.1.0");
    h.fetch.publish_at(PRERELEASE_LIST, "0.2.0", b"pre 0.2.0");
    let channel = Arc::new(Mutex::new(ReleaseChannel::Prerelease));
    let (updates, _) = channel_service(&h, "0.1.0", &channel);
    let alice = h.android_of("alice");
    alice.start();
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.2.0".into())
    );
    assert_eq!(
        updates.install_when_idle().unwrap(),
        None,
        "alice's Android runs"
    );

    *channel.lock().unwrap() = ReleaseChannel::Stable;
    alice.stop();
    assert_eq!(
        updates.install_when_idle().unwrap(),
        None,
        "the stable channel does not take it"
    );
    assert!(matches!(
        updates.check().unwrap(),
        CheckOutcome::NotNewer(_)
    ));
    assert!(h.downloads().is_empty(), "{:?}", h.downloads());
    assert!(updates.install_now().unwrap().installing.is_none());
    assert_eq!(h.installer.runs(), 0);
}

#[test]
fn fr15_on_the_prerelease_channel_a_list_that_cannot_be_read_does_not_stop_the_other() {
    let h = Harness::new();
    h.fetch.publish_at(PRERELEASE_LIST, "0.2.0", b"pre 0.2.0");
    // The stable list is there but signed by someone else.
    let bytes = serde_json::to_vec(&list_of("0.9.0", b"x")).unwrap();
    let other = SigningKey::from_bytes(&[8; 32]);
    h.fetch.put(
        &format!("{LIST}.sig"),
        hex(&other.sign(&bytes).to_bytes()).into(),
    );
    h.fetch.put(LIST, bytes);
    let channel = Arc::new(Mutex::new(ReleaseChannel::Prerelease));
    let (updates, _) = channel_service(&h, "0.1.0", &channel);
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::Downloaded("0.2.0".into())
    );
    // On the stable channel that list is the only one, so its fault is the answer.
    *channel.lock().unwrap() = ReleaseChannel::Stable;
    assert!(matches!(
        updates.check(),
        Err(ProgramError::BadSignature(_))
    ));
}

/// Connects as `user`, as if the operating system had reported that user.
struct DesktopOf(dsa_transport::local::MemoryLocalConnector, &'static str);

impl dsa_transport::Connector for DesktopOf {
    fn connect(
        &mut self,
        _: Duration,
    ) -> Result<Box<dyn dsa_transport::Connection>, dsa_transport::TransportError> {
        self.0.connect_as(dsa_transport::local::Peer {
            user: self.1.into(),
            administrator: false,
        })
    }
}

/// The local API of a service whose users' Android tell about the program updates of
/// `status`, as the installed service wraps them.
fn desktops(
    status: &Arc<ProgramStatus>,
) -> (
    dsa_host_core::host_api_conformance::RunningService,
    dsa_transport::local::MemoryLocalConnector,
) {
    let (listener, connector) = dsa_transport::local::pair();
    let status = Arc::clone(status);
    let androids = Arc::new(dsa_host_core::host_api::PerUser::new(move |_: &str| {
        Ok(Arc::new(WithProgramStatus {
            android: Arc::new(FakeAndroid::new()),
            status: Arc::clone(&status),
        }) as Arc<dyn UserAndroid>)
    }));
    let service =
        dsa_host_core::host_api_conformance::RunningService::start(Box::new(listener), androids);
    (service, connector)
}

/// What `user`'s desktop is given to show when it watches their notifications now, as a
/// window manager does: each notification posted until the watch has been quiet a while.
fn shown_on_desktop(
    connector: &dsa_transport::local::MemoryLocalConnector,
    user: &'static str,
) -> Vec<dsa_host_core::notifications::Notification> {
    use dsa_host_core::notifications::NotificationEvent;
    let mut desktop = DesktopOf(connector.clone(), user);
    let mut watch = dsa_host_core::host_api::HostApiClient::connect(
        &mut desktop,
        "desktop",
        Duration::from_secs(10),
    )
    .unwrap()
    .watch_notifications()
    .unwrap();
    let mut shown = Vec::new();
    while let Some(event) = watch.next(Duration::from_millis(500)).unwrap() {
        if let NotificationEvent::Posted(notification) = event {
            shown.push(notification);
        }
    }
    shown
}

fn keys(shown: &[dsa_host_core::notifications::Notification]) -> Vec<&str> {
    shown.iter().map(|n| n.key.as_str()).collect()
}

#[test]
fn fr15_each_desktop_is_told_once_that_an_update_waits_to_install() {
    use dsa_host_core::native_abi::in_user_language;
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (updates, status) = h.service("0.1.0");
    let (_service, desktop) = desktops(&status);
    assert!(shown_on_desktop(&desktop, "alice").is_empty());

    updates.check().unwrap();
    let shown = shown_on_desktop(&desktop, "alice");
    assert_eq!(keys(&shown), ["dsa:update:program:ready:0.1.1"]);
    let korean = in_user_language(shown[0].clone(), true);
    assert_eq!(korean.title, "DSA 업데이트가 준비되었습니다");
    assert!(korean.text.contains("DSA 0.1.1"), "{}", korean.text);
    let english = in_user_language(shown[0].clone(), false);
    assert_eq!(english.title, "DSA update ready");
    assert!(
        english
            .text
            .contains("installs when Android is not running"),
        "{}",
        english.text
    );

    // The same version again, from another check or another watch, is not shown again.
    assert_eq!(
        updates.check().unwrap(),
        CheckOutcome::AlreadyDownloaded("0.1.1".into())
    );
    assert!(shown_on_desktop(&desktop, "alice").is_empty());
    // Each user is told on their own desktop.
    assert_eq!(
        keys(&shown_on_desktop(&desktop, "bob")),
        ["dsa:update:program:ready:0.1.1"]
    );
}

#[test]
fn fr15_after_an_update_the_desktop_is_told_the_new_version_once() {
    use dsa_host_core::native_abi::in_user_language;
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    drop(old);
    // The service setup started is told before any desktop has connected to it.
    let (_new, status) = h.service("0.1.1");
    let (_service, desktop) = desktops(&status);
    let shown = shown_on_desktop(&desktop, "alice");
    assert_eq!(keys(&shown), ["dsa:update:program:applied:0.1.1"]);
    let english = in_user_language(shown[0].clone(), false);
    assert_eq!(english.title, "DSA updated");
    assert_eq!(english.text, "DSA 0.1.1 is installed.");
    assert_eq!(
        in_user_language(shown[0].clone(), true).title,
        "DSA를 업데이트했습니다"
    );
    assert!(shown_on_desktop(&desktop, "alice").is_empty());
}

#[test]
fn fr15_a_failed_update_is_told_on_the_desktop_with_why_and_only_once() {
    use dsa_host_core::native_abi::in_user_language;
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let (old, _) = h.service("0.1.0");
    old.install_now().unwrap();
    drop(old);
    // Setup failed and rolled back: the old version starts again.
    let (again, status) = h.service("0.1.0");
    let (_service, desktop) = desktops(&status);
    let shown = shown_on_desktop(&desktop, "alice");
    assert_eq!(keys(&shown), ["dsa:update:program:failed:0.1.1"]);
    let english = in_user_language(shown[0].clone(), false);
    assert_eq!(english.title, "DSA update failed");
    assert!(
        english.text.contains("previous version is still in use")
            && english.text.contains("setup-0.1.1.log"),
        "{}",
        english.text
    );
    let korean = in_user_language(shown[0].clone(), true);
    assert!(korean.text.contains("이유:"), "{}", korean.text);
    // A check that finds the same failed version says nothing new.
    assert!(matches!(
        again.check().unwrap(),
        CheckOutcome::FailedBefore(_)
    ));
    assert!(shown_on_desktop(&desktop, "alice").is_empty());
}

#[test]
fn fr15_a_click_on_an_update_alert_does_not_reach_android() {
    use dsa_host_core::notifications::NotificationAction;
    let h = Harness::new();
    let (_updates, status) = h.service("0.1.0");
    let (_service, desktop) = desktops(&status);
    let mut client = dsa_host_core::host_api::HostApiClient::connect(
        &mut DesktopOf(desktop.clone(), "alice"),
        "desktop",
        Duration::from_secs(10),
    )
    .unwrap();
    // Android is not even running; the click is the host's to act on, so it is taken.
    client
        .act_on_notification(&NotificationAction::Open {
            key: "dsa:update:program:ready:0.1.1".into(),
        })
        .unwrap();
}

/// Where the update stood each time it was looked at.
type Seen = Arc<Mutex<Vec<Option<ProgramProgress>>>>;

/// The users' Android, noting where the update stood when it was asked to stop them all.
/// With `stuck`, one of them does not stop.
struct StopObserved {
    androids: Arc<FakeAndroids>,
    status: Arc<ProgramStatus>,
    seen: Seen,
    stuck: bool,
}

impl UserAndroids for StopObserved {
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        self.androids.android(user)
    }
    fn running(&self) -> Vec<(String, SessionState)> {
        self.androids.running()
    }
    fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)> {
        self.seen.lock().unwrap().push(self.status.progress());
        if self.stuck {
            return vec![("alice".into(), SessionState::Stopping)];
        }
        self.androids.stop_all(timeout)
    }
}

/// The project's files, noting where the update stood while the setup file downloaded.
struct FetchObserved {
    fetch: MemoryFetch,
    status: Arc<ProgramStatus>,
    seen: Seen,
}

impl Fetch for FetchObserved {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        if url == SETUP_URL {
            self.seen.lock().unwrap().push(self.status.progress());
        }
        self.fetch.fetch(url, out)
    }
}

/// What an observed service saw: its updates, their status, and where the update stood
/// during each download of the setup file and each stop of every Android.
struct Observed {
    updates: Arc<ProgramUpdates>,
    status: Arc<ProgramStatus>,
    downloads: Seen,
    stops: Seen,
}

/// The program updates of a service of version `running` whose downloads and stops are
/// observed.
fn observed_service(h: &Harness, running: &str, stuck: bool) -> Observed {
    let status = Arc::new(ProgramStatus::default());
    let (downloads, stops) = (Seen::default(), Seen::default());
    let updates = ProgramUpdates::new(
        settings(&h.root, running),
        Box::new(FetchObserved {
            fetch: h.fetch.clone(),
            status: Arc::clone(&status),
            seen: Arc::clone(&downloads),
        }),
        Box::new(h.installer.clone()),
        Arc::new(StopObserved {
            androids: Arc::clone(&h.androids),
            status: Arc::clone(&status),
            seen: Arc::clone(&stops),
            stuck,
        }),
        Arc::clone(&status),
        "test",
    );
    Observed {
        updates,
        status,
        downloads,
        stops,
    }
}

/// Where the update stands, as a settings app reads it from the local API.
fn status_on_desktop(status: &Arc<ProgramStatus>) -> Option<ProgramProgress> {
    let (_service, desktop) = desktops(status);
    dsa_host_core::host_api::HostApiClient::connect(
        &mut DesktopOf(desktop, "alice"),
        "settings",
        Duration::from_secs(10),
    )
    .unwrap()
    .status()
    .unwrap()
    .program_update
}

fn progress(phase: ProgramPhase, version: &str) -> Option<ProgramProgress> {
    Some(ProgramProgress::new(phase, version))
}

#[test]
fn fr15_the_status_reports_each_phase_of_an_update_until_the_new_version_runs() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let old = observed_service(&h, "0.1.0", false);
    assert_eq!(status_on_desktop(&old.status), None, "no update has run");
    h.android_of("alice").start();

    old.updates.install_now().unwrap();
    assert_eq!(
        old.downloads.lock().unwrap().as_slice(),
        &[progress(ProgramPhase::Downloading, "0.1.1")],
        "downloading while the setup file comes"
    );
    assert_eq!(
        old.stops.lock().unwrap().as_slice(),
        &[progress(ProgramPhase::StoppingAndroid, "0.1.1")],
        "stopping Android before setup runs"
    );
    assert_eq!(
        status_on_desktop(&old.status),
        progress(ProgramPhase::Installing, "0.1.1"),
        "installing while setup runs"
    );
    assert!(ProgramPhase::Installing.in_progress());

    // Setup replaced the files under this service, which restarts into them.
    h.installer.files_are("0.1.1.0");
    h.installer.end(0, 0);
    assert!(eventually(|| h.installer.restarts() == 1));
    assert_eq!(
        status_on_desktop(&old.status),
        progress(ProgramPhase::Restarting, "0.1.1")
    );

    // The service from the new files says the update is done.
    drop(old);
    let (_new, status) = h.service("0.1.1");
    let applied = status_on_desktop(&status).unwrap();
    assert_eq!(
        applied,
        ProgramProgress::new(ProgramPhase::Applied, "0.1.1")
    );
    assert!(!applied.phase.in_progress());
    assert_eq!(applied.to_string(), "updated to DSA 0.1.1");
}

#[test]
fn fr15_the_status_reports_a_failed_update_and_a_finished_download_reports_no_phase() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let service = observed_service(&h, "0.1.0", false);
    // The service's own check downloads, then nothing is under way: the update waits.
    service.updates.check().unwrap();
    assert_eq!(
        service.downloads.lock().unwrap().as_slice(),
        &[progress(ProgramPhase::Downloading, "0.1.1")]
    );
    assert_eq!(status_on_desktop(&service.status), None);

    service.updates.install_now().unwrap();
    h.installer.end(0, 1603);
    assert!(eventually(|| service.status.installing().is_none()));
    let failed = status_on_desktop(&service.status).unwrap();
    assert_eq!(failed, ProgramProgress::new(ProgramPhase::Failed, "0.1.1"));
    assert!(!failed.phase.in_progress());
}

#[test]
fn fr15_an_update_refused_because_android_did_not_stop_reports_no_phase() {
    let h = Harness::new();
    h.fetch
        .publish(&list_of("0.1.1", b"setup"), b"setup", &project_key());
    let service = observed_service(&h, "0.1.0", true);
    let why = service.updates.install_now().unwrap_err();
    assert!(why.to_string().contains("did not stop in time"), "{why}");
    assert_eq!(
        service.stops.lock().unwrap().as_slice(),
        &[progress(ProgramPhase::StoppingAndroid, "0.1.1")]
    );
    assert_eq!(
        status_on_desktop(&service.status),
        None,
        "nothing is under way any more"
    );
    assert_eq!(h.installer.runs(), 0);
}

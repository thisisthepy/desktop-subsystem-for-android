//! Updates as the service runs them: the check, and the trial boot through a real session.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::UserAndroid;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry, Manifest, FORMAT};
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::service::{
    check, check_reporting, next_image_check, Download, UpdateNotices, UpdatingAndroid,
};
use dsa_host_core::update::store::{Fetch, ImageStore};
use dsa_host_core::update::UpdateError;
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::Role;
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::memory;
use dsa_transport::Connector;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const WAIT: Duration = Duration::from_secs(10);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[3; 32])
}

fn trusted() -> Vec<ed25519_dalek::VerifyingKey> {
    vec![manifest::parse_key(&hex(key().verifying_key().as_bytes())).unwrap()]
}

struct MemoryFetch(HashMap<String, Vec<u8>>);

impl Fetch for MemoryFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let bytes = self.0.get(url).ok_or_else(|| format!("{url}: HTTP 404"))?;
        out.write_all(bytes).map_err(|e| e.to_string())?;
        Ok(bytes.len() as u64)
    }
}

/// A server with a signed list offering `images` (version, major, build).
fn server(supported: &[u32], images: &[(&str, u32, u64)], signer: &SigningKey) -> MemoryFetch {
    let mut files = HashMap::new();
    let entries = images
        .iter()
        .map(|(version, major, build)| {
            let contents = format!("image {version}").into_bytes();
            let url = format!("mem://images/{version}/system.vhdx");
            let entry = ImageEntry {
                version: version.to_string(),
                android_major: *major,
                build: *build,
                files: vec![FileEntry {
                    name: "system.vhdx".into(),
                    url: url.clone(),
                    size: contents.len() as u64,
                    sha256: hex(&Sha256::digest(&contents)),
                }],
            };
            files.insert(url, contents);
            entry
        })
        .collect();
    let list = serde_json::to_vec(&Manifest {
        format: FORMAT,
        supported_majors: supported.to_vec(),
        images: entries,
    })
    .unwrap();
    files.insert(
        "mem://list.json.sig".into(),
        hex(&signer.sign(&list).to_bytes()).into_bytes(),
    );
    files.insert("mem://list.json".into(), list);
    MemoryFetch(files)
}

fn first() -> InstalledImage {
    InstalledImage {
        version: "16.0.1".into(),
        android_major: 16,
        build: 1,
    }
}

fn fresh_store() -> Arc<ImageStore> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "update-service-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("images/16.0.1")).unwrap();
    Arc::new(ImageStore::open(&root, &first()).unwrap())
}

/// A store as an installer without a bundled image leaves it: the first image is only a
/// name, `none`, with no folder, so the first check downloads a real one.
fn store_without_an_image() -> Arc<ImageStore> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "update-service-none-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let none = InstalledImage {
        version: "none".into(),
        android_major: 0,
        build: 0,
    };
    Arc::new(ImageStore::open(&root, &none).unwrap())
}

#[test]
fn fr14_a_check_downloads_the_newer_image_and_tells_the_user() {
    let store = fresh_store();
    let fetch = server(&[16], &[("16.0.2", 16, 2)], &key());
    let notice = check(&store, &fetch, "mem://list.json", &trusted()).unwrap();
    assert_eq!(
        notice.as_deref(),
        Some("Android 16.0.2 is downloaded and will be used the next time Android starts")
    );
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert!(state.on_trial);
    // Nothing more happens until the new image has had its first boot.
    assert_eq!(
        check(&store, &fetch, "mem://list.json", &trusted()).unwrap(),
        None
    );
}

#[test]
fn fr14_a_list_signed_by_someone_else_changes_nothing() {
    let store = fresh_store();
    let fetch = server(
        &[16],
        &[("16.0.2", 16, 2)],
        &SigningKey::from_bytes(&[4; 32]),
    );
    let error = check(&store, &fetch, "mem://list.json", &trusted()).unwrap_err();
    assert!(matches!(error, UpdateError::BadSignature(_)), "{error}");
    assert_eq!(store.state().unwrap().current, first());
}

#[test]
fn fr14_the_user_is_told_when_their_version_lost_support() {
    let store = fresh_store();
    let fetch = server(&[17, 18], &[], &key());
    let notice = check(&store, &fetch, "mem://list.json", &trusted())
        .unwrap()
        .unwrap();
    assert!(notice.contains("no longer supported"), "{notice}");
}

/// Starts whatever image the store calls current; images in `broken` do not start.
struct StoreLauncher {
    store: Arc<ImageStore>,
    broken: Vec<String>,
    /// What its console shows: the agent was refused because of the disk's signing keys.
    other_keys: bool,
    /// The running boot's secret, which the agent answers the host's challenge with.
    secret: Arc<Mutex<Option<BootSecret>>>,
    /// The images it started, in order.
    boots: Arc<Mutex<Vec<String>>>,
}

impl GuestLauncher for StoreLauncher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.secret.lock().unwrap() = Some(secret.clone());
        if let Ok(state) = self.store.state() {
            self.boots.lock().unwrap().push(state.current.version);
        }
        let current = self.store.state().map_err(|e| e.to_string())?.current;
        if !self.store.image_dir(&current.version).is_dir() {
            return Err(format!(
                "booted {}, which has no image files",
                current.version
            ));
        }
        if self.broken.contains(&current.version) {
            return Err(format!("{} does not boot", current.version));
        }
        Ok(())
    }
    fn stop(&mut self, _: Duration) -> Result<(), String> {
        Ok(())
    }
    fn agent_refused_for_keys(&mut self) -> bool {
        self.other_keys
    }
}

/// A session over the store, with an agent that connects whenever the guest is up.
fn updating_android(
    store: &Arc<ImageStore>,
    broken: &[&str],
) -> (UpdatingAndroid, Arc<AtomicBool>) {
    let (listener, connector) = memory::pair();
    let secret = Arc::new(Mutex::new(None));
    let session = Arc::new(Session::new(
        StoreLauncher {
            store: Arc::clone(store),
            broken: broken.iter().map(|b| b.to_string()).collect(),
            other_keys: false,
            secret: Arc::clone(&secret),
            boots: Default::default(),
        },
        Box::new(listener),
        ControlHandshake::new("test host", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            ..SessionConfig::default()
        },
    ));
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let watched = Arc::clone(&session);
    thread::spawn(move || {
        let mut connector = connector;
        while !finished.load(Ordering::Relaxed) {
            let boot_secret = secret.lock().unwrap().clone();
            if let (SessionState::Starting, Some(boot_secret)) = (watched.state(), boot_secret) {
                // A real agent takes a while; long enough for a test to act during the boot.
                thread::sleep(Duration::from_millis(300));
                if let Ok(mut agent) = connector.connect(WAIT) {
                    let _ = guest_connect(
                        &mut *agent,
                        &local_hello(Role::Guest, "agent"),
                        &boot_secret,
                        WAIT,
                    );
                    let _ = agent.send(&dsa_protocol::encode(&dsa_protocol::control::Envelope {
                        body: Some(dsa_protocol::control::envelope::Body::GuestBoot(
                            dsa_protocol::control::GuestBoot {
                                boot_completed: true,
                                agent_connected: true,
                            },
                        )),
                    }));
                    // Held until this boot ends, and no longer: the next boot needs a new one.
                    watched.wait_for(SessionState::Active, Duration::from_secs(1));
                    while watched.state() == SessionState::Active
                        && !finished.load(Ordering::Relaxed)
                    {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    (
        UpdatingAndroid {
            session,
            store: Arc::clone(store),
            notices: Arc::new(UpdateNotices::default()),
            boot_timeout: Duration::from_secs(5),
            trial_running: Arc::new(AtomicBool::new(false)),
            rollback: Default::default(),
            image_restart: Default::default(),
        },
        done,
    )
}

fn wait_for_notice(android: &UpdatingAndroid) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(notice) = android.update_notice() {
            return notice;
        }
        assert!(Instant::now() < deadline, "no notice");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn fr14_a_new_image_that_boots_is_kept_and_the_user_told() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, done) = updating_android(&store, &[]);
    android.start();
    assert_eq!(wait_for_notice(&android), "Android was updated to 16.0.2");
    assert_eq!(android.state(), SessionState::Active);
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert!(!state.on_trial);
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_new_image_that_does_not_boot_is_undone_and_the_session_started_again() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, done) = updating_android(&store, &["16.0.2"]);
    android.start();
    let notice = wait_for_notice(&android);
    assert!(
        notice.starts_with("Android 16.0.2 did not start")
            && notice.contains("Android 16.0.1 is in use again")
            && notice.contains("Android 16.0.2 is not tried again"),
        "{notice}"
    );
    assert!(
        android.session.wait_for(SessionState::Active, WAIT),
        "the session is running again"
    );
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.1");
    assert_eq!(state.failed, ["16.0.2"]);
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_stop_asked_for_during_the_rollback_does_not_cancel_it() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, done) = updating_android(&store, &["16.0.2"]);
    android.start();
    let deadline = Instant::now() + WAIT;
    while !android.rollback.running() {
        assert!(Instant::now() < deadline, "no rollback began");
        thread::sleep(Duration::from_millis(1));
    }
    // What `dsa start --wait` reads meanwhile: Android is still starting, and why.
    assert_eq!(android.state(), SessionState::Starting);
    let problem = android.problem().unwrap_or_default();
    assert!(
        problem.contains("going back to the previous one"),
        "{problem}"
    );
    // `dsa stop`, or a restart from the tray, in the middle of the rollback.
    android.stop();

    let notice = wait_for_notice(&android);
    assert!(
        notice.contains("Android 16.0.1 is in use again") && !notice.contains("failed too"),
        "{notice}"
    );
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.1");
    assert!(!state.on_trial);
    assert_eq!(state.failed, ["16.0.2"]);
    // The stop is made once the previous image has booted.
    assert!(
        android.session.wait_for(SessionState::Stopped, WAIT),
        "the stop asked for is made after the rollback"
    );
    // And the next start boots the previous image plainly.
    android.start();
    assert!(android.session.wait_for(SessionState::Active, WAIT));
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_new_image_whose_agent_never_connects_is_given_up_within_bounded_time() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (listener, mut connector) = memory::pair();
    let secret = Arc::new(Mutex::new(None));
    let session = Arc::new(Session::new(
        StoreLauncher {
            store: Arc::clone(&store),
            broken: Vec::new(),
            other_keys: false,
            secret: Arc::clone(&secret),
            boots: Default::default(),
        },
        Box::new(listener),
        ControlHandshake::new("test host", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            agent_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        },
    ));
    let android = UpdatingAndroid {
        session,
        store: Arc::clone(&store),
        notices: Arc::new(UpdateNotices::default()),
        // Far longer than the test waits: the agent's own deadline is what ends the trial.
        boot_timeout: Duration::from_secs(600),
        trial_running: Arc::new(AtomicBool::new(false)),
        rollback: Default::default(),
        image_restart: Default::default(),
    };
    let began = Instant::now();
    android.start();

    // Each boot's daemon reports Android booted; only the previous image's agent connects.
    let mut last = None;
    for agent_connected in [false, true] {
        let deadline = Instant::now() + WAIT;
        let boot_secret = loop {
            let current = secret.lock().unwrap().clone();
            if let Some(boot_secret) = current {
                if last.as_ref() != Some(&boot_secret) {
                    break boot_secret;
                }
            }
            assert!(Instant::now() < deadline, "the guest was not started");
            thread::sleep(Duration::from_millis(5));
        };
        last = Some(boot_secret.clone());
        let mut daemon = connector.connect(WAIT).unwrap();
        guest_connect(
            &mut *daemon,
            &local_hello(Role::Guest, "daemon"),
            &boot_secret,
            WAIT,
        )
        .unwrap();
        daemon
            .send(&dsa_protocol::encode(&dsa_protocol::control::Envelope {
                body: Some(dsa_protocol::control::envelope::Body::GuestBoot(
                    dsa_protocol::control::GuestBoot {
                        boot_completed: true,
                        agent_connected,
                    },
                )),
            }))
            .unwrap();
        if agent_connected {
            let notice = wait_for_notice(&android);
            assert!(
                notice.starts_with("Android 16.0.2 did not start")
                    && notice.contains("agent has not reached the control channel")
                    && notice.contains("Android 16.0.1 is in use again"),
                "{notice}"
            );
            assert!(android.session.wait_for(SessionState::Active, WAIT));
        }
        // Held until the boot ends, as a connection is.
        std::mem::forget(daemon);
    }
    assert!(
        began.elapsed() < Duration::from_secs(60),
        "the trial took {:?}",
        began.elapsed()
    );
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.1");
    assert_eq!(state.failed, ["16.0.2"]);
}

#[test]
fn fr14_a_new_image_on_a_disk_from_other_keys_is_neither_blamed_nor_kept() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (listener, mut connector) = memory::pair();
    let secret = Arc::new(Mutex::new(None));
    let session = Arc::new(Session::new(
        StoreLauncher {
            store: Arc::clone(&store),
            broken: Vec::new(),
            other_keys: true,
            secret: Arc::clone(&secret),
            boots: Default::default(),
        },
        Box::new(listener),
        ControlHandshake::new("test host", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            agent_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        },
    ));
    let android = UpdatingAndroid {
        session,
        store: Arc::clone(&store),
        notices: Arc::new(UpdateNotices::default()),
        boot_timeout: WAIT,
        trial_running: Arc::new(AtomicBool::new(false)),
        rollback: Default::default(),
        image_restart: Default::default(),
    };
    android.start();

    let deadline = Instant::now() + WAIT;
    let boot_secret = loop {
        if let Some(boot_secret) = secret.lock().unwrap().clone() {
            break boot_secret;
        }
        assert!(Instant::now() < deadline, "the guest was never started");
        thread::sleep(Duration::from_millis(5));
    };
    let mut daemon = connector.connect(WAIT).unwrap();
    guest_connect(
        &mut *daemon,
        &local_hello(Role::Guest, "daemon"),
        &boot_secret,
        WAIT,
    )
    .unwrap();
    daemon
        .send(&dsa_protocol::encode(&dsa_protocol::control::Envelope {
            body: Some(dsa_protocol::control::envelope::Body::GuestBoot(
                dsa_protocol::control::GuestBoot {
                    boot_completed: true,
                    agent_connected: false,
                },
            )),
        }))
        .unwrap();

    let notice = wait_for_notice(&android);
    assert!(notice.contains("16.0.2 is kept on trial"), "{notice}");
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2", "not rolled back");
    assert!(state.on_trial, "not kept either");
    assert!(state.failed.is_empty(), "not marked as failed");
    assert_eq!(
        android.session.state(),
        SessionState::Stalled,
        "the user's Android keeps running, with the problem to read"
    );
}

/// A user's Android over `store` whose guest's daemon the test plays, followed as the
/// service follows it. The daemon's console says the agent was refused for the disk's signing
/// keys, so a trial that does not get ready is undecided rather than undone.
fn undecided_android(
    store: &Arc<ImageStore>,
) -> (
    Arc<UpdatingAndroid>,
    memory::MemoryConnector,
    Arc<Mutex<Option<BootSecret>>>,
) {
    let (listener, connector) = memory::pair();
    let secret = Arc::new(Mutex::new(None));
    let session = Arc::new(Session::new(
        StoreLauncher {
            store: Arc::clone(store),
            broken: Vec::new(),
            other_keys: true,
            secret: Arc::clone(&secret),
            boots: Default::default(),
        },
        Box::new(listener),
        ControlHandshake::new("test host", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            agent_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        },
    ));
    let android = Arc::new(UpdatingAndroid {
        session,
        store: Arc::clone(store),
        notices: Arc::new(UpdateNotices::default()),
        boot_timeout: WAIT,
        trial_running: Arc::new(AtomicBool::new(false)),
        rollback: Default::default(),
        image_restart: Default::default(),
    });
    dsa_host_core::update::service::follow_waiting_image(&android, Duration::from_millis(50));
    (android, connector, secret)
}

/// Plays the daemon of the boot after `last`: it connects with that boot's secret and says
/// Android finished booting, and whether the agent connected. Returns the connection, which
/// is held as long as the boot lasts, and the boot's secret.
fn daemon_of_next_boot(
    connector: &mut memory::MemoryConnector,
    secret: &Mutex<Option<BootSecret>>,
    last: Option<&BootSecret>,
    agent_connected: bool,
) -> (Box<dyn dsa_transport::Connection>, BootSecret) {
    let deadline = Instant::now() + WAIT;
    let boot_secret = loop {
        if let Some(boot_secret) = secret.lock().unwrap().clone() {
            if last != Some(&boot_secret) {
                break boot_secret;
            }
        }
        assert!(Instant::now() < deadline, "the guest was not started");
        thread::sleep(Duration::from_millis(5));
    };
    let mut daemon = connector.connect(WAIT).unwrap();
    guest_connect(
        &mut *daemon,
        &local_hello(Role::Guest, "daemon"),
        &boot_secret,
        WAIT,
    )
    .unwrap();
    say_booted(&mut *daemon, agent_connected);
    (daemon, boot_secret)
}

fn say_booted(daemon: &mut dyn dsa_transport::Connection, agent_connected: bool) {
    daemon
        .send(&dsa_protocol::encode(&dsa_protocol::control::Envelope {
            body: Some(dsa_protocol::control::envelope::Body::GuestBoot(
                dsa_protocol::control::GuestBoot {
                    boot_completed: true,
                    agent_connected,
                },
            )),
        }))
        .unwrap();
}

#[test]
fn fr14_a_refusal_of_the_image_from_before_an_update_reads_as_a_step_of_the_update() {
    let store = fresh_store();
    let (android, mut connector, secret) = undecided_android(&store);
    android.start();

    // The image from before the update boots against the updated host, which refuses its
    // daemon for another protocol definition.
    let deadline = Instant::now() + WAIT;
    let boot_secret = loop {
        if let Some(boot_secret) = secret.lock().unwrap().clone() {
            break boot_secret;
        }
        assert!(Instant::now() < deadline, "the guest was never started");
        thread::sleep(Duration::from_millis(5));
    };
    let mut daemon = connector.connect(WAIT).unwrap();
    let mut hello = local_hello(Role::Guest, "dsa-channeld old");
    hello.schema_hash = vec![0x5a; 32];
    let _ = guest_connect(&mut *daemon, &hello, &boot_secret, WAIT);
    assert!(android.session.wait_for(SessionState::Stopped, WAIT));

    // With no newer image yet, it is a mismatch to act on, said in plain words.
    let reason = android.last_reason().expect("a reason");
    assert!(
        reason.starts_with("Android's image was made for another release of DSA"),
        "{reason}"
    );
    assert!(reason.contains("dsa update"), "{reason}");

    // The updated image arrives: the refusal was a step of the update.
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let reason = android.last_reason().expect("a reason");
    let first = reason.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("during the update, Android 16.0.1"),
        "{reason}"
    );
    assert!(first.contains("is resolved"), "{reason}");
    assert!(first.contains("starts from 16.0.2 next"), "{reason}");
    assert!(!first.contains("control.proto"), "{reason}");
}

fn alert_keys(android: &UpdatingAndroid) -> Vec<String> {
    android
        .untold_update_alerts("alice")
        .iter()
        .map(|alert| alert.key())
        .collect()
}

#[test]
fn fr14_an_undecided_trial_image_is_kept_once_android_finishes_starting_from_it() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, mut connector, secret) = undecided_android(&store);
    android.start();
    let (mut daemon, _) = daemon_of_next_boot(&mut connector, &secret, None, false);
    let notice = wait_for_notice(&android);
    assert!(notice.contains("16.0.2 is kept on trial"), "{notice}");
    assert!(store.state().unwrap().on_trial, "undecided so far");

    // The guest the trial left running gets ready later: its agent connects. No trial runs
    // any more, and the boot still keeps the image.
    say_booted(&mut *daemon, true);
    assert!(android.session.wait_for(SessionState::Active, WAIT));
    wait_until("the image is kept", || !store.state().unwrap().on_trial);
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert_eq!(state.previous.map(|p| p.version).as_deref(), Some("16.0.1"));
    assert!(state.failed.is_empty());
    wait_until("the user is told", || {
        android.update_notice().as_deref() == Some("Android was updated to 16.0.2")
    });
    assert_eq!(alert_keys(&android), ["dsa:update:android:applied:16.0.2"]);
    drop(daemon);
}

#[test]
fn fr14_a_trial_image_booted_without_a_trial_is_kept_once_android_finishes_starting() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, mut connector, secret) = undecided_android(&store);
    android.start();
    let (daemon, first_boot) = daemon_of_next_boot(&mut connector, &secret, None, false);
    let notice = wait_for_notice(&android);
    assert!(notice.contains("16.0.2 is kept on trial"), "{notice}");

    // The guest stops, and the session is started again without the updating Android, as
    // a host tool or a request for Android's files starts it: no trial runs for this boot.
    android.session.stop();
    assert!(android.session.wait_for(SessionState::Stopped, WAIT));
    drop(daemon);
    android.session.start();
    let (daemon, _) = daemon_of_next_boot(&mut connector, &secret, Some(&first_boot), true);
    assert!(android.session.wait_for(SessionState::Active, WAIT));
    wait_until("the image is kept", || !store.state().unwrap().on_trial);
    assert_eq!(store.state().unwrap().current.version, "16.0.2");
    wait_until("the user is told", || {
        android.update_notice().as_deref() == Some("Android was updated to 16.0.2")
    });
    assert_eq!(alert_keys(&android), ["dsa:update:android:applied:16.0.2"]);
    drop(daemon);
}

#[test]
fn fr14_a_finished_boot_of_an_image_not_on_trial_changes_nothing() {
    // Android runs the kept image, and a new one is downloaded meanwhile: a boot that
    // completes is of the old image, and says nothing about the new one.
    let store = fresh_store();
    let (android, boots, done) = dozing_android(&store);
    android.image_auto_restart(false);
    android.start();
    assert!(android.session.wait_for(SessionState::Dozing, WAIT));
    let before = store.state().unwrap();
    assert!(!before.on_trial);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        store.state().unwrap(),
        before,
        "a kept image stays as it is"
    );
    assert_eq!(android.update_notice(), None);
    assert!(alert_keys(&android).is_empty());

    download_new_image(&store, &android);
    thread::sleep(Duration::from_millis(500));
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert!(
        state.on_trial,
        "the new image has not booted, so it is not kept"
    );
    assert!(
        store.has_image("16.0.1"),
        "the previous image is not removed"
    );
    assert_eq!(*boots.lock().unwrap(), ["16.0.1"]);
    assert_eq!(alert_keys(&android), ["dsa:update:android:ready:16.0.2"]);
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_trial_image_that_does_not_boot_is_still_undone_while_boots_are_followed() {
    let store = fresh_store();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, done) = updating_android(&store, &["16.0.2"]);
    let android = Arc::new(android);
    dsa_host_core::update::service::follow_waiting_image(&android, Duration::from_millis(10));
    android.start();
    let notice = wait_for_notice(&android);
    assert!(
        notice.starts_with("Android 16.0.2 did not start")
            && notice.contains("Android 16.0.1 is in use again"),
        "{notice}"
    );
    assert!(android.session.wait_for(SessionState::Active, WAIT));
    thread::sleep(Duration::from_millis(200));
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.1");
    assert!(!state.on_trial);
    assert_eq!(state.failed, ["16.0.2"]);
    assert_eq!(
        android.update_notice(),
        Some(notice),
        "the previous image booting is not taken for the new one"
    );
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_check_says_which_image_it_downloads_before_it_starts() {
    let store = store_without_an_image();
    let fetch = server(&[16], &[("16.0.2", 16, 2)], &key());
    let mut announced = Vec::new();
    check_reporting(
        &store,
        &fetch,
        "mem://list.json",
        &trusted(),
        &mut |download: &Download| {
            if download.done == 0 {
                assert!(
                    !store.image_dir(&download.image.version).exists(),
                    "announced after the download"
                );
                announced.push(download.image.version.clone());
            }
        },
    )
    .unwrap();
    assert_eq!(announced, ["16.0.2"]);
    // Up to date: nothing to announce.
    store.confirm().unwrap();
    check_reporting(
        &store,
        &fetch,
        "mem://list.json",
        &trusted(),
        &mut |download: &Download| {
            panic!(
                "announced {} with nothing to download",
                download.image.version
            )
        },
    )
    .unwrap();
}

#[test]
fn fr14_a_download_reports_its_progress_up_to_the_whole_size() {
    let store = store_without_an_image();
    let fetch = server(&[16], &[("16.0.2", 16, 2)], &key());
    let mut seen = Vec::new();
    check_reporting(
        &store,
        &fetch,
        "mem://list.json",
        &trusted(),
        &mut |download: &Download| seen.push((download.done, download.total)),
    )
    .unwrap();
    let total = "image 16.0.2".len() as u64;
    assert_eq!(
        seen.first(),
        Some(&(0, total)),
        "starts at nothing: {seen:?}"
    );
    assert_eq!(
        seen.last(),
        Some(&(total, total)),
        "ends at the whole size: {seen:?}"
    );
    assert!(
        seen.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "never goes backwards: {seen:?}"
    );
}

#[test]
fn fr14_a_download_notice_says_how_far_it_has_got() {
    let image = ImageEntry {
        version: "17.0.0-1".into(),
        android_major: 17,
        build: 1,
        files: Vec::new(),
    };
    let notice = |done, total| {
        Download {
            image: &image,
            done,
            total,
        }
        .notice()
    };
    assert_eq!(
        notice(0, 1_800_000_000),
        "downloading Android 17.0.0-1: 0% (0 of 1.8 GB)"
    );
    assert_eq!(
        notice(450_000_000, 1_800_000_000),
        "downloading Android 17.0.0-1: 25% (0.45 of 1.8 GB)"
    );
    assert_eq!(
        notice(1_800_000_000, 1_800_000_000),
        "downloading Android 17.0.0-1: 100% (1.8 of 1.8 GB)"
    );
}

#[test]
fn fr14_starting_before_any_image_is_downloaded_says_so_and_boots_nothing() {
    let store = store_without_an_image();
    let (android, done) = updating_android(&store, &[]);
    android.start();
    assert_eq!(android.state(), SessionState::Stopped);
    let reason = android.last_reason().unwrap_or_default();
    assert!(
        reason.contains("no Android image is installed yet"),
        "{reason}"
    );
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_the_first_image_downloaded_after_installation_boots() {
    let store = store_without_an_image();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, done) = updating_android(&store, &[]);
    android.start();
    assert_eq!(wait_for_notice(&android), "Android 16.0.2 is installed");
    assert!(android.session.wait_for(SessionState::Active, WAIT));
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert!(!state.on_trial);
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_first_image_that_does_not_boot_is_given_up_without_booting_the_placeholder() {
    let store = store_without_an_image();
    check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    let (android, done) = updating_android(&store, &["16.0.2"]);
    android.start();
    let notice = wait_for_notice(&android);
    assert!(
        notice.starts_with("Android 16.0.2 did not start") && notice.contains("another image"),
        "{notice}"
    );
    assert_eq!(android.state(), SessionState::Stopped);
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "none");
    assert_eq!(state.failed, ["16.0.2"]);
    // The next check moves on to an image that has not failed.
    let next = check(
        &store,
        &server(&[16], &[("16.0.2", 16, 2), ("16.0.1", 16, 1)], &key()),
        "mem://list.json",
        &trusted(),
    )
    .unwrap();
    assert!(next.unwrap_or_default().contains("16.0.1"));
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_losing_the_only_image_brings_the_next_check_forward() {
    let now = Instant::now();
    let retry = Duration::from_secs(300);
    let tomorrow = now + Duration::from_secs(24 * 3600);
    // The check was scheduled a day out while an image was in place; the image then failed
    // its first boot and none is left, so the check comes now.
    assert_eq!(next_image_check(tomorrow, now, false, retry), now);
    // With an image, the schedule stands.
    assert_eq!(next_image_check(tomorrow, now, true, retry), tomorrow);
    // A check already due within the retry stands too.
    let soon = now + Duration::from_secs(60);
    assert_eq!(next_image_check(soon, now, false, retry), soon);
}

/// Connects as `user`, as if the operating system had reported that user.
struct DesktopOf(dsa_transport::local::MemoryLocalConnector, &'static str);

impl Connector for DesktopOf {
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

/// The local API of a service whose users all have `android`.
fn desktops(
    android: &Arc<UpdatingAndroid>,
) -> (
    dsa_host_core::host_api_conformance::RunningService,
    dsa_transport::local::MemoryLocalConnector,
) {
    let (listener, connector) = dsa_transport::local::pair();
    let android = Arc::clone(android);
    let androids = Arc::new(dsa_host_core::host_api::PerUser::new(move |_: &str| {
        Ok(Arc::clone(&android) as Arc<dyn UserAndroid>)
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
    let mut watch = dsa_host_core::host_api::HostApiClient::connect(
        &mut DesktopOf(connector.clone(), user),
        "desktop",
        WAIT,
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
fn fr14_the_desktop_is_told_once_before_and_once_after_an_image_update() {
    use dsa_host_core::native_abi::in_user_language;
    use dsa_host_core::update::service::check_alerting;
    let store = fresh_store();
    let (android, done) = updating_android(&store, &[]);
    let android = Arc::new(android);
    let (_service, desktop) = desktops(&android);
    let fetch = server(&[16], &[("16.0.2", 16, 2)], &key());
    let check = || {
        check_alerting(
            &store,
            &fetch,
            "mem://list.json",
            &trusted(),
            &android.notices,
            &mut |_: &Download| {},
        )
        .unwrap()
    };
    assert!(check().is_some());
    let shown = shown_on_desktop(&desktop, "alice");
    assert_eq!(keys(&shown), ["dsa:update:android:ready:16.0.2"]);
    let english = in_user_language(shown[0].clone(), false);
    assert_eq!(english.title, "Android update ready");
    assert!(
        english.text.contains("used the next time Android starts"),
        "{}",
        english.text
    );
    assert_eq!(
        in_user_language(shown[0].clone(), true).title,
        "Android 업데이트가 준비되었습니다"
    );
    // Checked again and watched again: the same version is not shown twice.
    assert_eq!(check(), None);
    assert!(shown_on_desktop(&desktop, "alice").is_empty());

    android.start();
    assert_eq!(wait_for_notice(&android), "Android was updated to 16.0.2");
    let shown = shown_on_desktop(&desktop, "alice");
    assert_eq!(keys(&shown), ["dsa:update:android:applied:16.0.2"]);
    assert_eq!(
        in_user_language(shown[0].clone(), false).text,
        "Android 16.0.2 started and is now in use."
    );
    assert!(shown_on_desktop(&desktop, "alice").is_empty());
    done.store(true, Ordering::Relaxed);
}

#[test]
fn fr14_a_desktop_that_was_away_is_told_only_how_the_update_ended() {
    use dsa_host_core::native_abi::in_user_language;
    use dsa_host_core::update::service::check_alerting;
    let store = fresh_store();
    let (android, done) = updating_android(&store, &["16.0.2"]);
    let android = Arc::new(android);
    check_alerting(
        &store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
        &android.notices,
        &mut |_: &Download| {},
    )
    .unwrap();
    android.start();
    let notice = wait_for_notice(&android);
    assert!(
        notice.starts_with("Android 16.0.2 did not start"),
        "{notice}"
    );
    // Nobody watched meanwhile. The news that it was ready is old by now; only the failure
    // is shown, with why.
    let (_service, desktop) = desktops(&android);
    let shown = shown_on_desktop(&desktop, "alice");
    assert_eq!(keys(&shown), ["dsa:update:android:failed:16.0.2"]);
    let english = in_user_language(shown[0].clone(), false);
    assert_eq!(english.title, "Android update failed");
    assert!(
        english.text.starts_with("Android 16.0.2 was not applied.")
            && english.text.contains("Android 16.0.1 is in use again"),
        "{}",
        english.text
    );
    assert_eq!(
        in_user_language(shown[0].clone(), true).title,
        "Android 업데이트에 실패했습니다"
    );
    assert!(shown_on_desktop(&desktop, "alice").is_empty());
    assert!(
        android.session.wait_for(SessionState::Active, WAIT),
        "the previous image runs again"
    );
    done.store(true, Ordering::Relaxed);
}

/// A user's Android over the store whose guest dozes as a real one does: the session turns the
/// screen off once no window has been shown for a moment, and the agent then says the device
/// is idle. Its follower restarts it into each image downloaded while it runs, as the service's
/// does. Also returns the images its guest booted, in order.
fn dozing_android(
    store: &Arc<ImageStore>,
) -> (
    Arc<UpdatingAndroid>,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicBool>,
) {
    use dsa_protocol::control::{envelope::Body, DeviceIdle, Envelope, GuestBoot};
    let (listener, connector) = memory::pair();
    let secret = Arc::new(Mutex::new(None));
    let boots = Arc::new(Mutex::new(Vec::new()));
    let session = Arc::new(Session::new(
        StoreLauncher {
            store: Arc::clone(store),
            broken: Vec::new(),
            other_keys: false,
            secret: Arc::clone(&secret),
            boots: Arc::clone(&boots),
        },
        Box::new(listener),
        ControlHandshake::new("test host", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            doze_delay: Duration::from_millis(200),
            ..SessionConfig::default()
        },
    ));
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let watched = Arc::clone(&session);
    thread::spawn(move || {
        let mut connector = connector;
        let up = |state| {
            matches!(
                state,
                SessionState::Active | SessionState::Dozing | SessionState::Frozen
            )
        };
        while !finished.load(Ordering::Relaxed) {
            let boot_secret = secret.lock().unwrap().clone();
            if let (SessionState::Starting, Some(boot_secret)) = (watched.state(), boot_secret) {
                thread::sleep(Duration::from_millis(100));
                if let Ok(mut agent) = connector.connect(WAIT) {
                    let _ = guest_connect(
                        &mut *agent,
                        &local_hello(Role::Guest, "agent"),
                        &boot_secret,
                        WAIT,
                    );
                    let _ = agent.send(&dsa_protocol::encode(&Envelope {
                        body: Some(Body::GuestBoot(GuestBoot {
                            boot_completed: true,
                            agent_connected: true,
                        })),
                    }));
                    watched.wait_for(SessionState::Active, Duration::from_secs(1));
                    // Idle whenever its screen is off, until this boot ends.
                    while up(watched.state()) && !finished.load(Ordering::Relaxed) {
                        let idle = Envelope {
                            body: Some(Body::DeviceIdle(DeviceIdle { idle: true })),
                        };
                        if agent.send(&dsa_protocol::encode(&idle)).is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    let android = Arc::new(UpdatingAndroid {
        session,
        store: Arc::clone(store),
        notices: Arc::new(UpdateNotices::default()),
        boot_timeout: Duration::from_secs(5),
        trial_running: Arc::new(AtomicBool::new(false)),
        rollback: Default::default(),
        image_restart: Default::default(),
    });
    dsa_host_core::update::service::follow_waiting_image(&android, Duration::from_millis(50));
    (android, boots, done)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The service's check finds 16.0.2 and downloads it.
fn download_new_image(store: &ImageStore, android: &UpdatingAndroid) {
    dsa_host_core::update::service::check_alerting(
        store,
        &server(&[16], &[("16.0.2", 16, 2)], &key()),
        "mem://list.json",
        &trusted(),
        &android.notices,
        &mut |_: &Download| {},
    )
    .unwrap();
}

/// The new image passed its trial, and the guest booted the old image and then the new one.
fn wait_applied(store: &ImageStore, boots: &Mutex<Vec<String>>) {
    wait_until("the new image is applied", || {
        let state = store.state().unwrap();
        state.current.version == "16.0.2" && !state.on_trial && boots.lock().unwrap().len() == 2
    });
    assert_eq!(*boots.lock().unwrap(), ["16.0.1", "16.0.2"]);
}

/// Shows a task window and waits until Android is awake for it.
fn open_a_window(android: &UpdatingAndroid) {
    android.session.windows_shown(true).unwrap();
    wait_until("Android is awake for the window", || {
        android.session.state() == SessionState::Active
    });
}

#[test]
fn fr14_a_downloaded_image_is_applied_without_the_user_restarting() {
    use dsa_host_core::native_abi::in_user_language;
    use dsa_host_core::update::alerts::{buttons_of, LATER, RESTART_NOW};

    // No app window is open, so Android dozes: it restarts into the new image at once, and
    // the desktop is told once that is over, not asked.
    {
        let store = fresh_store();
        let (android, boots, done) = dozing_android(&store);
        let (_service, desktop) = desktops(&android);
        android.start();
        assert!(android.session.wait_for(SessionState::Dozing, WAIT));
        download_new_image(&store, &android);
        wait_applied(&store, &boots);
        let shown = shown_on_desktop(&desktop, "alice");
        assert_eq!(keys(&shown), ["dsa:update:android:applied:16.0.2"]);
        assert_eq!(android.restart_pending(), None);
        done.store(true, Ordering::Relaxed);
    }

    // App windows are open: the desktop asks, with [Restart now] and [Later], and nothing
    // restarts. [Restart now] restarts it into the new image.
    {
        let store = fresh_store();
        let (android, boots, done) = dozing_android(&store);
        let (_service, desktop) = desktops(&android);
        android.start();
        assert!(android.session.wait_for(SessionState::Active, WAIT));
        open_a_window(&android);
        download_new_image(&store, &android);
        wait_until("the user is asked", || {
            android.image_restart.asked().is_some()
        });
        let shown = shown_on_desktop(&desktop, "alice");
        assert_eq!(
            keys(&shown),
            ["dsa:update:android:restart:16.0.2"],
            "asked, without the news that it waits for the next start"
        );
        let asked = in_user_language(shown[0].clone(), false);
        assert_eq!(asked.title, "Restart Android to update");
        let actions: Vec<&str> = buttons_of(&asked.key, false)
            .iter()
            .map(|button| button.action)
            .collect();
        assert_eq!(actions, [RESTART_NOW, LATER]);
        thread::sleep(Duration::from_secs(1));
        assert_eq!(boots.lock().unwrap().len(), 1, "nothing restarts meanwhile");
        assert!(store.state().unwrap().on_trial, "not applied yet");
        assert_eq!(android.session.state(), SessionState::Active);
        assert_eq!(android.restart_pending().as_deref(), Some("16.0.2"));
        assert!(
            android
                .update_notice()
                .is_some_and(|notice| notice.contains("restart Android")),
            "dsa status says how to apply it"
        );

        let mut client = dsa_host_core::host_api::HostApiClient::connect(
            &mut DesktopOf(desktop.clone(), "alice"),
            "desktop",
            WAIT,
        )
        .unwrap();
        let (version, detail) = client.restart_for_update().unwrap();
        assert_eq!(version.as_deref(), Some("16.0.2"), "{detail}");
        wait_applied(&store, &boots);
        let shown = shown_on_desktop(&desktop, "alice");
        assert_eq!(keys(&shown), ["dsa:update:android:applied:16.0.2"]);
        let (version, _) = client.restart_for_update().unwrap();
        assert_eq!(version, None, "nothing waits any more");
        done.store(true, Ordering::Relaxed);
    }

    // [Later], or no answer: the image is applied once every window is closed and Android
    // dozes.
    {
        let store = fresh_store();
        let (android, boots, done) = dozing_android(&store);
        let (_service, desktop) = desktops(&android);
        android.start();
        assert!(android.session.wait_for(SessionState::Active, WAIT));
        open_a_window(&android);
        download_new_image(&store, &android);
        wait_until("the user is asked", || {
            android.image_restart.asked().is_some()
        });
        assert_eq!(
            keys(&shown_on_desktop(&desktop, "alice")),
            ["dsa:update:android:restart:16.0.2"]
        );
        thread::sleep(Duration::from_millis(500));
        assert_eq!(boots.lock().unwrap().len(), 1);
        android.session.windows_shown(false).unwrap();
        wait_applied(&store, &boots);
        assert_eq!(
            keys(&shown_on_desktop(&desktop, "alice")),
            ["dsa:update:android:applied:16.0.2"]
        );
        done.store(true, Ordering::Relaxed);
    }

    // The setting off: the image waits for the next start, even while Android dozes, and the
    // desktop says so.
    {
        let store = fresh_store();
        let (android, boots, done) = dozing_android(&store);
        let (_service, desktop) = desktops(&android);
        android.image_auto_restart(false);
        android.start();
        assert!(android.session.wait_for(SessionState::Dozing, WAIT));
        download_new_image(&store, &android);
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(boots.lock().unwrap().len(), 1, "not restarted");
        assert!(store.state().unwrap().on_trial, "not applied yet");
        let shown = shown_on_desktop(&desktop, "alice");
        assert_eq!(keys(&shown), ["dsa:update:android:ready:16.0.2"]);
        assert!(
            in_user_language(shown[0].clone(), false)
                .text
                .contains("used the next time Android starts"),
            "{:?}",
            shown[0]
        );
        // The settings app still offers [Restart now].
        assert_eq!(android.restart_pending().as_deref(), Some("16.0.2"));
        android.stop();
        assert!(android.session.wait_for(SessionState::Stopped, WAIT));
        android.start();
        wait_applied(&store, &boots);
        done.store(true, Ordering::Relaxed);
    }
}

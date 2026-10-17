//! Updates as the service runs them: the check, and the trial boot through a real session.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::UserAndroid;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry, Manifest, FORMAT};
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::service::{check, UpdateNotices, UpdatingAndroid};
use dsa_host_core::update::store::{Fetch, ImageStore};
use dsa_host_core::update::UpdateError;
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
}

impl GuestLauncher for StoreLauncher {
    fn start(&mut self) -> Result<(), String> {
        let current = self.store.state().map_err(|e| e.to_string())?.current;
        if self.broken.contains(&current.version) {
            return Err(format!("{} does not boot", current.version));
        }
        Ok(())
    }
    fn stop(&mut self, _: Duration) -> Result<(), String> {
        Ok(())
    }
}

/// A session over the store, with an agent that connects whenever the guest is up.
fn updating_android(
    store: &Arc<ImageStore>,
    broken: &[&str],
) -> (UpdatingAndroid, Arc<AtomicBool>) {
    let (listener, connector) = memory::pair();
    let session = Arc::new(Session::new(
        StoreLauncher {
            store: Arc::clone(store),
            broken: broken.iter().map(|b| b.to_string()).collect(),
        },
        Box::new(listener),
        ControlHandshake::new("test host", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
        },
    ));
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let watched = Arc::clone(&session);
    thread::spawn(move || {
        let mut connector = connector;
        while !finished.load(Ordering::Relaxed) {
            if watched.state() == SessionState::Starting {
                if let Ok(mut agent) = connector.connect(WAIT) {
                    let _ = guest_connect(&mut *agent, &local_hello(Role::Guest, "agent"), WAIT);
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
            && notice.ends_with("Android 16.0.1 is in use again"),
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

//! Choosing the Android version: the channel per major, moving up only, an older major only
//! with the data erased, and the version as the local API reports it.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::host_api::{ClientError, HostApiClient, HostApiService};
use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry, Manifest};
use dsa_host_core::update::policy::{decide_for, Decision, InstalledImage};
use dsa_host_core::update::service::{check, CheckNow, StoreVersions, UpdateNotices};
use dsa_host_core::update::store::{Fetch, ImageStore};
use dsa_host_core::update::versions::{
    self, major_of_version, target_major, AndroidVersions, Channel, ChooseError,
};
use dsa_protocol::host_api::types::ErrorCode;
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::{Connection, Connector, TransportError};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const WAIT: Duration = Duration::from_secs(10);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[9; 32])
}

fn image(version: &str, major: u32, build: u64) -> ImageEntry {
    ImageEntry {
        version: version.into(),
        android_major: major,
        build,
        files: vec![FileEntry {
            name: "system.vhdx".into(),
            url: format!("mem://{version}/system.vhdx"),
            size: version.len() as u64,
            sha256: hex(&Sha256::digest(version.as_bytes())),
        }],
    }
}

fn list(supported: &[u32], images: Vec<ImageEntry>) -> Manifest {
    Manifest {
        format: manifest::FORMAT,
        supported_majors: supported.to_vec(),
        images,
    }
}

fn installed(version: &str, major: u32, build: u64) -> InstalledImage {
    InstalledImage {
        version: version.into(),
        android_major: major,
        build,
    }
}

/// Serves a signed release list at `mem://list` and each image's file (its version's bytes).
struct Release(HashMap<String, Vec<u8>>);

impl Release {
    fn of(manifest: &Manifest) -> Release {
        let bytes = serde_json::to_vec(manifest).unwrap();
        let mut served = HashMap::new();
        served.insert(
            "mem://list.sig".into(),
            hex(&key().sign(&bytes).to_bytes()).into_bytes(),
        );
        served.insert("mem://list".into(), bytes);
        for image in &manifest.images {
            served.insert(
                format!("mem://{}/system.vhdx", image.version),
                image.version.clone().into_bytes(),
            );
        }
        Release(served)
    }
}

impl Fetch for Release {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let bytes = self.0.get(url).ok_or_else(|| format!("{url} not found"))?;
        out.write_all(bytes).map_err(|e| e.to_string())?;
        Ok(bytes.len() as u64)
    }
}

fn trusted() -> Vec<ed25519_dalek::VerifyingKey> {
    vec![manifest::parse_key(&hex(key().verifying_key().as_bytes())).unwrap()]
}

fn store_on(first: &InstalledImage) -> (Arc<ImageStore>, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "versions-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("images").join(&first.version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("image.json"),
        r#"{"version": "x", "source": {"build_id": "16373615", "security_patch": "2026-06-05"}}"#,
    )
    .unwrap();
    (Arc::new(ImageStore::open(&root, first).unwrap()), root)
}

#[test]
fn fr14_the_supported_versions_come_from_one_list() {
    assert_eq!(versions::supported_majors(), vec![16, 17]);
    assert_eq!(versions::newest_major(), 17);
    assert_eq!(major_of_version("17.0.0-16373615-8"), Some(17));
    assert_eq!(major_of_version("16.0.0-15581820-8-arm64"), Some(16));
    assert_eq!(major_of_version("none"), None);
}

#[test]
fn fr14_a_chosen_major_takes_only_its_own_builds() {
    let now = installed("16.0.0-1", 16, 1);
    let manifest = list(
        &[16, 17],
        vec![image("16.0.0-2", 16, 2), image("17.0.0-1", 17, 1)],
    );
    match decide_for(&now, &manifest, &[], Channel::Major(16), false) {
        Decision::Apply(image) => assert_eq!(image.version, "16.0.0-2"),
        other => panic!("expected the newer 16 build, got {other:?}"),
    }
    match decide_for(&now, &manifest, &[], Channel::Latest, false) {
        Decision::Apply(image) => assert_eq!(image.version, "17.0.0-1"),
        other => panic!("expected the newest major, got {other:?}"),
    }
    let on_newest = installed("16.0.0-2", 16, 2);
    assert_eq!(
        decide_for(&on_newest, &manifest, &[], Channel::Major(16), false),
        Decision::UpToDate
    );
}

#[test]
fn fr14_a_chosen_major_that_lost_support_moves_up_to_the_next_supported_one() {
    assert_eq!(target_major(Channel::Major(16), &[17, 18]), Some(17));
    assert_eq!(target_major(Channel::Major(19), &[17, 18]), Some(18));
    assert_eq!(target_major(Channel::Latest, &[17, 18]), Some(18));

    let (store, _root) = store_on(&installed("16.0.0-2", 16, 2));
    store.choose(Channel::Major(16), false).unwrap();
    let manifest = list(
        &[17, 18],
        vec![image("17.0.0-3", 17, 3), image("18.0.0-1", 18, 1)],
    );
    let notice = check(&store, &Release::of(&manifest), "mem://list", &trusted())
        .unwrap()
        .unwrap();
    assert!(notice.contains("17.0.0-3"), "{notice}");
    let state = store.state().unwrap();
    assert_eq!(state.channel, Channel::Major(17));
    assert_eq!(state.current.version, "17.0.0-3");
    assert!(state.on_trial);
}

#[test]
fn fr14_choosing_a_newer_major_downloads_it_on_the_next_check() {
    let (store, _root) = store_on(&installed("16.0.0-2", 16, 2));
    store.choose(Channel::Major(16), false).unwrap();
    store.choose(Channel::Major(17), false).unwrap();
    assert!(store.switch_pending());
    let manifest = list(&[16, 17], vec![image("17.0.0-1", 17, 1)]);
    check(&store, &Release::of(&manifest), "mem://list", &trusted()).unwrap();
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "17.0.0-1");
    assert_eq!(state.previous.unwrap().version, "16.0.0-2");
    assert!(state.on_trial);
}

#[test]
fn fr14_an_older_major_needs_the_data_erased_and_is_then_applied() {
    let (store, _root) = store_on(&installed("17.0.0-1", 17, 1));
    match store.choose(Channel::Major(16), false) {
        Err(ChooseError::DataWouldBeLost(why)) => {
            assert!(why.contains("erases the Android data"), "{why}");
            assert!(why.contains("--erase-data"), "{why}");
        }
        other => panic!("expected the data loss to be named, got {other:?}"),
    }
    assert_eq!(
        store.state().unwrap().channel,
        Channel::Latest,
        "nothing changed"
    );
    match store.choose(Channel::Major(15), true) {
        Err(ChooseError::Unsupported(why)) => assert!(why.contains("16, 17"), "{why}"),
        other => panic!("expected an unsupported version, got {other:?}"),
    }

    store.choose(Channel::Major(16), true).unwrap();
    let manifest = list(
        &[16, 17],
        vec![image("16.0.0-9", 16, 9), image("17.0.0-1", 17, 1)],
    );
    check(&store, &Release::of(&manifest), "mem://list", &trusted()).unwrap();
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.0-9");
    assert!(state.on_trial);
    assert!(!state.older_allowed, "the consent is used once");
}

#[test]
fn fr14_a_disk_with_data_of_a_newer_major_is_replaced_before_an_older_one_boots() {
    let (_, root) = store_on(&installed("16.0.0-9", 16, 9));
    let disk = root.join("alice.vhdx");
    std::fs::write(&disk, b"Android 17 data").unwrap();
    std::fs::write(disk.with_extension("image-version"), "17.0.0-1").unwrap();
    let version = disk.with_extension("image-version");
    use dsa_host_core::data_reset::erase_if_newer;
    // Data of the same or an older major is moved up, never erased.
    assert!(!erase_if_newer(&disk, &version, 17).unwrap());
    assert!(disk.exists());
    assert!(erase_if_newer(&disk, &version, 16).unwrap());
    assert!(!disk.exists() && !version.exists());
    assert!(!erase_if_newer(&disk, &version, 16).unwrap());
}

/// Connects as `alice`, acting as an administrator or not.
struct AsAdmin(MemoryLocalConnector, bool);

impl Connector for AsAdmin {
    fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.connect_as(Peer {
            user: "alice".into(),
            administrator: self.1,
        })
    }
}

#[test]
fn fr14_the_version_is_reported_and_only_an_administrator_chooses_it() {
    let (store, _root) = store_on(&installed("17.0.0-16373615-8", 17, 1637361508));
    let check_now = Arc::new(CheckNow::default());
    let versions = Arc::new(StoreVersions {
        store: Arc::clone(&store),
        notices: Arc::new(UpdateNotices::default()),
        check_now: Arc::clone(&check_now),
        shared_by_users: true,
    });
    let (listener, connector) = local::pair();
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_android_versions(versions as Arc<dyn AndroidVersions>);
    let _service = RunningService::start_service(Box::new(listener), service);

    let mut user =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), false), "u", WAIT).unwrap();
    let report = user.android_versions().unwrap();
    assert_eq!(report.android_major, 17);
    assert_eq!(report.version, "17.0.0-16373615-8");
    assert_eq!(report.build_id.as_deref(), Some("16373615"));
    assert_eq!(report.security_patch.as_deref(), Some("2026-06-05"));
    assert_eq!(report.supported_majors, vec![16, 17]);
    assert_eq!(report.channel, Channel::Latest);
    match user.use_android_version(Channel::Major(17), false) {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::NotAllowed),
        other => panic!("expected a refusal, got {other:?}"),
    }

    let mut admin = HostApiClient::connect(&mut AsAdmin(connector, true), "a", WAIT).unwrap();
    match admin.use_android_version(Channel::Major(16), false) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::DataWouldBeLost);
            assert!(detail.contains("accounts"), "{detail}");
        }
        other => panic!("expected the data loss to be named, got {other:?}"),
    }
    match admin.use_android_version(Channel::Major(12), false) {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::InvalidSetting),
        other => panic!("expected an unsupported version, got {other:?}"),
    }
    let chosen = admin.use_android_version(Channel::Major(16), true).unwrap();
    assert_eq!(chosen.channel, Channel::Major(16));
    assert_eq!(chosen.target_major, 16);
    assert!(chosen.notice.unwrap().contains("Android 16"));
    assert!(check_now.take(), "the service checks for the image at once");
}

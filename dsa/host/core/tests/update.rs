//! Image updates: the signed release list, the rules, staging and the trial boot.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry, Manifest};
use dsa_host_core::update::policy::{check, decide, Decision, InstalledImage};
use dsa_host_core::update::store::{Fetch, ImageStore};
use dsa_host_core::update::trial::{self, Boot, Outcome};
use dsa_host_core::update::UpdateError;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn project_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn sign(key: &SigningKey, bytes: &[u8]) -> String {
    hex(&key.sign(bytes).to_bytes())
}

fn trusted() -> Vec<ed25519_dalek::VerifyingKey> {
    vec![manifest::parse_key(&hex(project_key().verifying_key().as_bytes())).unwrap()]
}

fn image(version: &str, major: u32, build: u64, contents: &[u8]) -> ImageEntry {
    ImageEntry {
        version: version.into(),
        android_major: major,
        build,
        files: vec![FileEntry {
            name: "system.vhdx".into(),
            url: format!("mem://{version}/system.vhdx"),
            size: contents.len() as u64,
            sha256: hex(&Sha256::digest(contents)),
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

/// Serves fixed bytes per URL.
struct MemoryFetch(HashMap<String, Vec<u8>>);

impl Fetch for MemoryFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let bytes = self.0.get(url).ok_or_else(|| format!("{url} not found"))?;
        out.write_all(bytes).map_err(|e| e.to_string())?;
        Ok(bytes.len() as u64)
    }
}

fn serving(pairs: &[(&str, &[u8])]) -> MemoryFetch {
    MemoryFetch(
        pairs
            .iter()
            .map(|(version, bytes)| (format!("mem://{version}/system.vhdx"), bytes.to_vec()))
            .collect(),
    )
}

fn fresh_store(first: &InstalledImage) -> (ImageStore, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "update-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("images").join(&first.version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("system.vhdx"), b"first image").unwrap();
    (ImageStore::open(&root, first).unwrap(), root)
}

#[test]
fn fr14_an_unsigned_or_wrongly_signed_list_is_refused() {
    let bytes = serde_json::to_vec(&list(&[16], vec![])).unwrap();
    let other = SigningKey::from_bytes(&[9; 32]);
    for signature in [String::new(), "00".repeat(64), sign(&other, &bytes)] {
        let error = manifest::verify(&bytes, &signature, &trusted()).unwrap_err();
        assert!(matches!(error, UpdateError::BadSignature(_)), "{error}");
    }
    let mut tampered = bytes.clone();
    let signature = sign(&project_key(), &bytes);
    tampered[2] ^= 1;
    assert!(matches!(
        manifest::verify(&tampered, &signature, &trusted()),
        Err(UpdateError::BadSignature(_))
    ));
    assert!(manifest::verify(&bytes, &signature, &trusted()).is_ok());
    assert!(matches!(
        manifest::verify(&bytes, &signature, &[]),
        Err(UpdateError::BadSignature(_))
    ));
}

#[test]
fn fr14_a_list_of_an_unknown_format_or_with_unsafe_names_is_refused() {
    let mut newer = list(&[16], vec![]);
    newer.format = 2;
    let mut escaping = list(&[16], vec![image("16.0.1", 16, 2, b"x")]);
    escaping.images[0].files[0].name = "../../evil".into();
    let mut bad_version = list(&[16], vec![image("16.0.1", 16, 2, b"x")]);
    bad_version.images[0].version = "..".into();
    for manifest in [newer, escaping, bad_version] {
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let result = manifest::verify(&bytes, &sign(&project_key(), &bytes), &trusted());
        assert!(
            matches!(result, Err(UpdateError::BadManifest(_))),
            "{result:?}"
        );
    }
}

#[test]
fn fr14_an_image_of_an_unsupported_version_is_not_applied_and_says_why() {
    let offered = image("14.0.9", 14, 9, b"x");
    let manifest = list(&[15, 16], vec![offered.clone()]);
    let why = check(&installed("14.0.1", 14, 1), &offered, &manifest, &[]).unwrap_err();
    assert!(why.contains("not a supported version"), "{why}");
}

#[test]
fn fr14_a_lower_major_version_is_never_applied() {
    let offered = image("15.0.9", 15, 99, b"x");
    let manifest = list(&[15, 16], vec![offered.clone()]);
    let current = installed("16.0.1", 16, 1);
    let why = check(&current, &offered, &manifest, &[]).unwrap_err();
    assert!(why.contains("older major version"), "{why}");
    assert_eq!(decide(&current, &manifest, &[]), Decision::UpToDate);
}

#[test]
fn fr14_the_newest_supported_image_is_chosen_including_the_next_major() {
    let manifest = list(
        &[15, 16],
        vec![
            image("15.0.2", 15, 2, b"a"),
            image("16.0.1", 16, 1, b"b"),
            image("16.0.3", 16, 3, b"c"),
        ],
    );
    match decide(&installed("15.0.1", 15, 1), &manifest, &[]) {
        Decision::Apply(image) => assert_eq!(image.version, "16.0.3"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        decide(&installed("16.0.3", 16, 3), &manifest, &[]),
        Decision::UpToDate
    );
}

#[test]
fn fr14_a_user_on_a_version_that_lost_support_moves_to_a_supported_one() {
    let manifest = list(&[15, 16], vec![image("15.0.4", 15, 4, b"a")]);
    match decide(&installed("14.0.7", 14, 7), &manifest, &[]) {
        Decision::Apply(image) => assert_eq!(image.version, "15.0.4"),
        other => panic!("{other:?}"),
    }
    match decide(&installed("14.0.7", 14, 7), &list(&[15, 16], vec![]), &[]) {
        Decision::Unsupported(why) => assert!(why.contains("no longer supported"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn fr14_a_download_that_does_not_match_its_hash_is_not_applied() {
    let first = installed("16.0.1", 16, 1);
    let (store, root) = fresh_store(&first);
    let offered = image("16.0.2", 16, 2, b"the real image");
    let manifest = list(&[16], vec![offered.clone()]);
    let error = store
        .stage(
            &offered,
            &manifest,
            &serving(&[("16.0.2", b"a tampered one")]),
        )
        .unwrap_err();
    assert!(matches!(error, UpdateError::Download(_)), "{error}");
    assert_eq!(store.state().unwrap().current, first);
    assert!(!root.join("images/16.0.2").exists());
}

#[test]
fn fr14_a_staged_image_is_current_on_trial_with_the_previous_kept() {
    let first = installed("16.0.1", 16, 1);
    let (store, root) = fresh_store(&first);
    let offered = image("16.0.2", 16, 2, b"second image");
    let manifest = list(&[16], vec![offered.clone()]);
    store
        .stage(
            &offered,
            &manifest,
            &serving(&[("16.0.2", b"second image")]),
        )
        .unwrap();
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.2");
    assert_eq!(state.previous, Some(first));
    assert!(state.on_trial);
    assert_eq!(
        std::fs::read(root.join("images/16.0.2/system.vhdx")).unwrap(),
        b"second image"
    );
    assert!(root.join("images/16.0.1/system.vhdx").exists());
    // The state is on disk, not only in memory.
    let reopened = ImageStore::open(&root, &installed("ignored", 1, 1)).unwrap();
    assert_eq!(reopened.state().unwrap(), state);
}

/// A guest that boots every image except the ones named.
struct FakeGuest {
    broken: Vec<String>,
    booted: Vec<(String, PathBuf)>,
    halts: u32,
}

impl Boot for FakeGuest {
    fn boot(&mut self, image: &InstalledImage, dir: &Path, _: Duration) -> Result<(), String> {
        if self.broken.contains(&image.version) {
            return Err("the agent did not connect within 120 s".into());
        }
        self.booted.push((image.version.clone(), dir.to_path_buf()));
        Ok(())
    }
    fn halt(&mut self) {
        self.halts += 1;
    }
}

fn staged_store() -> (ImageStore, PathBuf, Manifest) {
    let (store, root) = fresh_store(&installed("16.0.1", 16, 1));
    let offered = image("16.0.2", 16, 2, b"second image");
    let manifest = list(&[16], vec![offered.clone()]);
    store
        .stage(
            &offered,
            &manifest,
            &serving(&[("16.0.2", b"second image")]),
        )
        .unwrap();
    (store, root, manifest)
}

#[test]
fn fr14_an_image_that_does_not_boot_in_time_is_rolled_back_and_the_previous_started() {
    let (store, root, manifest) = staged_store();
    let mut guest = FakeGuest {
        broken: vec!["16.0.2".into()],
        booted: vec![],
        halts: 0,
    };
    let outcome = trial::start(&store, &mut guest, Duration::from_secs(120)).unwrap();
    match outcome {
        Outcome::RolledBack {
            failed,
            reason,
            image,
        } => {
            assert_eq!(failed.version, "16.0.2");
            assert!(reason.contains("did not connect"), "{reason}");
            assert_eq!(image.version, "16.0.1");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(guest.halts, 1, "the failed guest is stopped first");
    assert_eq!(
        guest.booted,
        vec![("16.0.1".into(), root.join("images").join("16.0.1"))]
    );
    let state = store.state().unwrap();
    assert_eq!(state.current.version, "16.0.1");
    assert!(!state.on_trial);
    assert!(!root.join("images/16.0.2").exists());

    // The failed image is not offered again.
    let current = state.current.clone();
    assert_eq!(
        decide(&current, &manifest, &state.failed),
        Decision::UpToDate
    );
}

#[test]
fn fr14_an_image_that_boots_is_kept_and_older_images_removed() {
    let (store, root, manifest) = staged_store();
    let mut guest = FakeGuest {
        broken: vec![],
        booted: vec![],
        halts: 0,
    };
    let outcome = trial::start(&store, &mut guest, Duration::from_secs(120)).unwrap();
    assert!(
        matches!(
            outcome,
            Outcome::Booted {
                confirmed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(!store.state().unwrap().on_trial);

    // A third image: once it boots, the first is removed and the second kept as previous.
    let third = image("16.0.3", 16, 3, b"third image");
    let mut manifest = manifest;
    manifest.images.push(third.clone());
    store
        .stage(&third, &manifest, &serving(&[("16.0.3", b"third image")]))
        .unwrap();
    trial::start(&store, &mut guest, Duration::from_secs(120)).unwrap();
    assert!(!root.join("images/16.0.1").exists());
    assert!(root.join("images/16.0.2").exists());
    assert!(root.join("images/16.0.3").exists());
}

#[test]
fn fr14_nothing_new_is_staged_while_an_image_is_on_trial() {
    let (store, _, mut manifest) = staged_store();
    let third = image("16.0.3", 16, 3, b"third");
    manifest.images.push(third.clone());
    let error = store
        .stage(&third, &manifest, &serving(&[("16.0.3", b"third")]))
        .unwrap_err();
    assert!(matches!(error, UpdateError::Refused(_)), "{error}");
}

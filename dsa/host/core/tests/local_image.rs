//! Installing an Android image from a folder: `dsa android install-image`, which a
//! development build's install script runs. The folder's image goes through the update path's
//! own checks, so nothing an update would refuse gets in this way. Its setup file is checked
//! against the signed program list beside it, as the service checks a downloaded update.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use dsa_host_core::update::local::{
    check_setup_in_folder, install_from_folder, LIST, PROGRAM_LIST,
};
use dsa_host_core::update::manifest::{self, FileEntry, ImageEntry, Manifest, FORMAT};
use dsa_host_core::update::policy::{decide, Decision, InstalledImage};
use dsa_host_core::update::program::{self, ProgramError, ProgramManifest, SetupFile};
use dsa_host_core::update::store::ImageStore;
use dsa_host_core::update::versions::{development_build_of, image_order};
use dsa_host_core::update::UpdateError;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn project_key() -> SigningKey {
    SigningKey::from_bytes(&[5; 32])
}

fn trusted() -> Vec<ed25519_dalek::VerifyingKey> {
    vec![manifest::parse_key(&hex(project_key().verifying_key().as_bytes())).unwrap()]
}

fn fresh_dir(what: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "local-image-{what}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn installed(version: &str, build: u64) -> InstalledImage {
    InstalledImage {
        version: version.into(),
        android_major: 17,
        build,
    }
}

/// A store whose current image is `first`, with its files in place.
fn store_with(first: &InstalledImage) -> (ImageStore, PathBuf) {
    let root = fresh_dir("store");
    let dir = root.join("images").join(&first.version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("system.vhdx"), b"the installed image").unwrap();
    (ImageStore::open(&root, first).unwrap(), root)
}

/// A folder as a development build's artifacts leave it: the image's files and the release
/// list naming them, signed by `signer`. The URLs are where a release would have served
/// them; the folder is read by file name.
fn image_folder(version: &str, build: u64, signer: &SigningKey) -> PathBuf {
    let dir = fresh_dir("folder");
    let contents = format!("image {version}").into_bytes();
    std::fs::write(dir.join("system.vhdx"), &contents).unwrap();
    let list = Manifest {
        format: FORMAT,
        supported_majors: vec![16, 17],
        images: vec![ImageEntry {
            version: version.into(),
            android_major: 17,
            build,
            files: vec![FileEntry {
                name: "system.vhdx".into(),
                url: "https://github.com/example/actions/runs/1/system.vhdx".into(),
                size: contents.len() as u64,
                sha256: hex(&Sha256::digest(&contents)),
            }],
        }],
    };
    let bytes = serde_json::to_vec_pretty(&list).unwrap();
    std::fs::write(dir.join(LIST), &bytes).unwrap();
    std::fs::write(
        dir.join(format!("{LIST}.sig")),
        hex(&signer.sign(&bytes).to_bytes()),
    )
    .unwrap();
    dir
}

fn install(store: &ImageStore, dir: &Path) -> Result<InstalledImage, UpdateError> {
    install_from_folder(store, dir, &trusted(), &mut |_| {})
}

#[test]
fn fr14_an_image_from_a_folder_is_installed_on_trial_through_the_update_checks() {
    let before = installed("17.0.0-16373615-14", 1637361514);
    let (store, root) = store_with(&before);
    let version = "17.0.0-16373615-15-dev.3";
    let dir = image_folder(version, 1637361515, &project_key());
    let now = install(&store, &dir).unwrap();
    assert_eq!(now.version, version);
    let state = store.state().unwrap();
    assert_eq!(state.current.version, version);
    assert!(state.on_trial, "a new image boots on trial");
    assert_eq!(state.previous, Some(before), "the image before it is kept");
    assert_eq!(
        std::fs::read(root.join("images").join(version).join("system.vhdx")).unwrap(),
        format!("image {version}").into_bytes()
    );
    // Until it has booted nothing else goes on top of it.
    let next = image_folder("17.0.0-16373615-15-dev.4", 1637361515, &project_key());
    let why = install(&store, &next).unwrap_err();
    assert!(why.to_string().contains("first boot"), "{why}");
}

#[test]
fn fr14_a_folder_whose_list_is_not_signed_by_a_trusted_key_installs_nothing() {
    let before = installed("17.0.0-16373615-14", 1637361514);
    let (store, _) = store_with(&before);
    let stranger = SigningKey::from_bytes(&[6; 32]);
    let dir = image_folder("17.0.0-16373615-15-dev.3", 1637361515, &stranger);
    let why = install(&store, &dir).unwrap_err();
    assert!(matches!(why, UpdateError::BadSignature(_)), "{why}");
    assert_eq!(store.state().unwrap().current, before);
    // A list without its signature is refused the same way.
    let dir = image_folder("17.0.0-16373615-15-dev.3", 1637361515, &project_key());
    std::fs::remove_file(dir.join(format!("{LIST}.sig"))).unwrap();
    assert!(matches!(
        install(&store, &dir),
        Err(UpdateError::BadSignature(_))
    ));
    assert_eq!(store.state().unwrap().current, before);
}

#[test]
fn fr14_a_folder_file_that_differs_from_the_signed_list_installs_nothing() {
    let before = installed("17.0.0-16373615-14", 1637361514);
    let (store, root) = store_with(&before);
    let version = "17.0.0-16373615-15-dev.3";
    let dir = image_folder(version, 1637361515, &project_key());
    std::fs::write(dir.join("system.vhdx"), b"image 17.0.0-16373615-15-dev.X").unwrap();
    let why = install(&store, &dir).unwrap_err();
    assert!(matches!(why, UpdateError::Download(_)), "{why}");
    assert_eq!(store.state().unwrap().current, before);
    assert!(!root.join("images").join(version).exists());
}

#[test]
fn fr14_a_folder_image_that_is_not_newer_is_refused_and_says_why() {
    let before = installed("17.0.0-16373615-14", 1637361514);
    let (store, _) = store_with(&before);
    let dir = image_folder("17.0.0-16373615-13", 1637361513, &project_key());
    let why = install(&store, &dir).unwrap_err();
    assert!(matches!(why, UpdateError::Refused(_)), "{why}");
    assert!(why.to_string().contains("not newer"), "{why}");
    assert_eq!(store.state().unwrap().current, before);
}

#[test]
fn fr14_a_development_image_orders_after_the_release_before_it_and_before_the_release_of_its_revision(
) {
    assert_eq!(development_build_of("17.0.0-16373615-15-dev.7"), Some(7));
    assert_eq!(development_build_of("17.0.0-16373615-15"), None);
    assert_eq!(development_build_of("17.0.0-16373615-15-dev."), None);
    let order = |version: &str, build| image_order(17, build, version);
    let released_14 = order("17.0.0-16373615-14", 1637361514);
    let dev_6 = order("17.0.0-16373615-15-dev.6", 1637361515);
    let dev_7 = order("17.0.0-16373615-15-dev.7", 1637361515);
    let released_15 = order("17.0.0-16373615-15", 1637361515);
    assert!(released_14 < dev_6 && dev_6 < dev_7 && dev_7 < released_15);

    // The update service, too, takes the release of the revision a development build used.
    let image = |version: &str| ImageEntry {
        version: version.into(),
        android_major: 17,
        build: 1637361515,
        files: Vec::new(),
    };
    let list = Manifest {
        format: FORMAT,
        supported_majors: vec![16, 17],
        images: vec![image("17.0.0-16373615-15")],
    };
    let on_dev = installed("17.0.0-16373615-15-dev.7", 1637361515);
    assert!(matches!(decide(&on_dev, &list, &[]), Decision::Apply(_)));
    let on_release = installed("17.0.0-16373615-15", 1637361515);
    let dev_list = Manifest {
        images: vec![image("17.0.0-16373615-15-dev.8")],
        ..list
    };
    assert_eq!(decide(&on_release, &dev_list, &[]), Decision::UpToDate);
}

/// A folder as a development build's installer artifact leaves it: the setup file and the
/// program list naming it, signed by `signer`.
fn setup_folder(setup: &[u8], signer: &SigningKey) -> PathBuf {
    let dir = fresh_dir("setup");
    let name = "DSA-v0.2.4-dev.3-win-x64-setup.exe";
    std::fs::write(dir.join(name), setup).unwrap();
    let list = ProgramManifest {
        format: program::FORMAT,
        version: "0.2.4-dev.3".into(),
        setup: SetupFile {
            url: format!("https://github.com/example/actions/runs/1/{name}"),
            size: setup.len() as u64,
            sha256: hex(&Sha256::digest(setup)),
        },
    };
    let bytes = serde_json::to_vec_pretty(&list).unwrap();
    std::fs::write(dir.join(PROGRAM_LIST), &bytes).unwrap();
    std::fs::write(
        dir.join(format!("{PROGRAM_LIST}.sig")),
        hex(&signer.sign(&bytes).to_bytes()),
    )
    .unwrap();
    dir
}

#[test]
fn fr15_a_setup_file_in_a_folder_is_checked_against_its_signed_program_list() {
    let dir = setup_folder(b"setup", &project_key());
    let (list, path) = check_setup_in_folder(&dir, &trusted()).unwrap();
    assert_eq!(list.version, "0.2.4-dev.3");
    assert_eq!(path, dir.join("DSA-v0.2.4-dev.3-win-x64-setup.exe"));

    let stranger = setup_folder(b"setup", &SigningKey::from_bytes(&[6; 32]));
    assert!(matches!(
        check_setup_in_folder(&stranger, &trusted()),
        Err(ProgramError::BadSignature(_))
    ));
    // The same length, other bytes.
    std::fs::write(path, b"Setup").unwrap();
    let why = check_setup_in_folder(&dir, &trusted()).unwrap_err();
    assert!(matches!(why, ProgramError::Download(_)), "{why}");
    assert!(why.to_string().contains("SHA-256"), "{why}");
}

#[test]
fn fr14_a_newer_development_build_of_the_same_revision_replaces_the_older() {
    let before = installed("17.0.0-16373615-15-dev.6", 1637361515);
    let (store, _) = store_with(&before);
    let dir = image_folder("17.0.0-16373615-15-dev.7", 1637361515, &project_key());
    assert_eq!(
        install(&store, &dir).unwrap().version,
        "17.0.0-16373615-15-dev.7"
    );
}

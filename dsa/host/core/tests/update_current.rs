//! Which image the next boot starts from: the store's current image, or the folder the
//! configuration names.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use dsa_host_core::update::current::{current_image, prune_image_cache};
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::store::ImageStore;

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "current-image-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn fr14_store_picks_the_current_version() {
    let root = scratch("store");
    let first = InstalledImage {
        version: "16.0.1".into(),
        android_major: 16,
        build: 7,
    };
    let store = ImageStore::open(&root, &first).unwrap();
    // The folder named in the configuration is ignored once a store decides.
    let image = current_image(Some(&store), Some(Path::new("/elsewhere"))).unwrap();
    assert_eq!(image.dir, root.join("images").join("16.0.1"));
    assert_eq!(image.version.as_deref(), Some("16.0.1"));
    assert_eq!(image.android_major, Some(16));
    assert_eq!(image.cache_key(), "16.0.1");
}

#[test]
fn fr14_without_a_store_the_manifest_names_the_version() {
    let dir = scratch("manifest");
    std::fs::write(dir.join("image.json"), r#"{"version": "15.0.3"}"#).unwrap();
    let image = current_image(None, Some(&dir)).unwrap();
    assert_eq!(image.dir, dir);
    assert_eq!(image.version.as_deref(), Some("15.0.3"));
    assert_eq!(image.android_major, None);
}

#[test]
fn fr14_a_manifest_without_version_has_none() {
    let dir = scratch("noversion");
    let image = current_image(None, Some(&dir)).unwrap();
    assert_eq!(image.version, None);
    std::fs::write(dir.join("image.json"), r#"{"kernel_cmdline": ""}"#).unwrap();
    assert_eq!(current_image(None, Some(&dir)).unwrap().version, None);
    std::fs::write(dir.join("image.json"), "not json").unwrap();
    assert_eq!(current_image(None, Some(&dir)).unwrap().version, None);
}

#[test]
fn fr14_cache_key_is_local_without_version() {
    let dir = scratch("local");
    assert_eq!(
        current_image(None, Some(&dir)).unwrap().cache_key(),
        "local"
    );
}

#[test]
fn fr14_nothing_configured_is_an_error() {
    let why = current_image(None, None).unwrap_err();
    assert_eq!(why, "the configuration names no image to boot");
}

#[test]
fn fr14_prune_keeps_only_the_current_cache() {
    let cache = scratch("prune");
    for version in ["15.0.3", "16.0.1", "local"] {
        std::fs::create_dir_all(cache.join(version)).unwrap();
        std::fs::write(cache.join(version).join("disk.img"), b"x").unwrap();
    }
    prune_image_cache(&cache, "16.0.1");
    assert!(cache.join("16.0.1").join("disk.img").exists());
    assert!(!cache.join("15.0.3").exists());
    assert!(!cache.join("local").exists());
    // A cache folder that does not exist is nothing to prune.
    prune_image_cache(&cache.join("missing"), "16.0.1");
}

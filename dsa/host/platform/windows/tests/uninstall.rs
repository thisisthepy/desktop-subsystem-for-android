//! Removing the downloaded images when DSA is uninstalled.

use std::path::PathBuf;

use dsa_host_windows::uninstall::remove_downloaded_images;

fn fresh(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("uninstall-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("images")
}

#[test]
fn fr1_uninstalling_removes_the_downloaded_images() {
    let store = fresh("store");
    std::fs::create_dir_all(store.join("images").join("16.0.2")).unwrap();
    std::fs::write(
        store.join("images").join("16.0.2").join("system.vhdx"),
        b"x",
    )
    .unwrap();
    std::fs::write(store.join("state.json"), b"{}").unwrap();
    let users = store.parent().unwrap().join("users");
    std::fs::create_dir_all(&users).unwrap();
    remove_downloaded_images(&store).unwrap();
    assert!(!store.exists());
    assert!(users.exists(), "each user's data stays");
    remove_downloaded_images(&store).unwrap();
}

#[test]
fn fr1_a_folder_that_is_not_the_image_store_is_left_alone() {
    let store = fresh("other");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("thesis.docx"), b"x").unwrap();
    assert!(remove_downloaded_images(&store).is_err());
    assert!(store.join("thesis.docx").exists());
    let parent = store.parent().unwrap().to_path_buf();
    assert!(
        remove_downloaded_images(&parent).is_err(),
        "only a folder named images"
    );
}

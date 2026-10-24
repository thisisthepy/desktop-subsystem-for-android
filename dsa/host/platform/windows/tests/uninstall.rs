//! Removing the downloaded images when DSA is uninstalled.

use std::path::PathBuf;

use dsa_host_windows::uninstall::{installer_folder, remove_downloaded_images};

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

#[test]
fn fr1_the_folder_from_the_uninstaller_is_read_whole() {
    // Windows Installer's [IMAGESFOLDER] ends in a backslash; quoted, it arrives with a quote.
    assert_eq!(
        installer_folder(r#"C:\ProgramData\DSA\images""#),
        PathBuf::from(r"C:\ProgramData\DSA\images")
    );
    assert_eq!(
        installer_folder(r"C:\ProgramData\DSA\images\"),
        PathBuf::from(r"C:\ProgramData\DSA\images")
    );
    let store = fresh("quoted");
    std::fs::create_dir_all(store.join("images")).unwrap();
    let argument = format!("{}\\\"", store.display());
    remove_downloaded_images(&installer_folder(&argument)).unwrap();
    assert!(!store.exists());
}

#[test]
fn fr15_uninstalling_removes_the_downloaded_setup_files() {
    use dsa_host_windows::uninstall::remove_downloaded_updates;
    let base = fresh("updates").parent().unwrap().to_path_buf();
    let updates = base.join("updates");
    std::fs::create_dir_all(updates.join("0.1.1")).unwrap();
    std::fs::write(updates.join("0.1.1").join("setup-0.1.1.exe"), b"x").unwrap();
    std::fs::write(updates.join(".download-0.1.2"), b"x").unwrap();
    std::fs::write(updates.join("state.json"), b"{}").unwrap();
    remove_downloaded_updates(&updates).unwrap();
    assert!(!updates.exists());
    // Nothing downloaded yet: nothing to do.
    remove_downloaded_updates(&updates).unwrap();
}

#[test]
fn fr15_a_folder_that_is_not_the_update_folder_is_left_alone() {
    use dsa_host_windows::uninstall::remove_downloaded_updates;
    let base = fresh("not-updates").parent().unwrap().to_path_buf();
    let updates = base.join("updates");
    std::fs::create_dir_all(updates.join("photos")).unwrap();
    assert!(remove_downloaded_updates(&updates).is_err());
    assert!(updates.join("photos").exists());
    let elsewhere = base.join("documents");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(
        remove_downloaded_updates(&elsewhere).is_err(),
        "only a folder named updates"
    );
}

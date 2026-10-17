//! dsa-release, the project's signing tool, end to end.

use std::path::PathBuf;
use std::process::Command;

use dsa_host_core::update::manifest::{self, ImageEntry, Manifest, FORMAT};

fn tool(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_dsa-release"))
        .args(args)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

#[test]
fn fr14_a_list_signed_by_the_release_tool_is_accepted_and_a_changed_one_refused() {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("secret.key");
    let key = key.to_str().unwrap();

    let (ok, public, err) = tool(&["keygen", key]);
    assert!(ok, "{err}");
    assert_eq!(public.len(), 64);
    let (ok, _, err) = tool(&["keygen", key]);
    assert!(!ok && err.contains("never overwritten"), "{err}");
    assert_eq!(tool(&["public-key", key]).1, public);

    let image_file = dir.join("system.vhdx");
    std::fs::write(&image_file, b"an image").unwrap();
    let (ok, entry, err) = tool(&[
        "entry",
        "16.0.2",
        "16",
        "2026101500",
        "https://example.invalid/16.0.2/",
        image_file.to_str().unwrap(),
    ]);
    assert!(ok, "{err}");
    let entry: ImageEntry = serde_json::from_str(&entry).unwrap();
    assert_eq!(
        entry.files[0].url,
        "https://example.invalid/16.0.2/system.vhdx"
    );
    assert_eq!(entry.files[0].size, 8);

    let list = Manifest {
        format: FORMAT,
        supported_majors: vec![15, 16],
        images: vec![entry],
    };
    let list_path = dir.join("images.json");
    std::fs::write(&list_path, serde_json::to_vec_pretty(&list).unwrap()).unwrap();
    let list_path = list_path.to_str().unwrap();
    let (ok, _, err) = tool(&["sign", key, list_path]);
    assert!(ok, "{err}");
    let (ok, out, err) = tool(&["verify", &public, list_path]);
    assert!(ok && out.starts_with("signed and valid"), "{out} {err}");

    let bytes = std::fs::read(list_path).unwrap();
    let signature = std::fs::read_to_string(format!("{list_path}.sig")).unwrap();
    let trusted = [manifest::parse_key(&public).unwrap()];
    assert_eq!(
        manifest::verify(&bytes, &signature, &trusted).unwrap(),
        list
    );

    let mut changed = bytes.clone();
    changed.extend_from_slice(b" ");
    std::fs::write(list_path, changed).unwrap();
    let (ok, _, err) = tool(&["verify", &public, list_path]);
    assert!(!ok && err.contains("not signed by the project"), "{err}");
}

#[test]
fn fr14_an_image_folder_becomes_a_list_entry_from_its_image_json() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("release-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("disk.vhdx"), b"a disk").unwrap();
    std::fs::write(dir.join("initrd.img"), b"a ramdisk").unwrap();
    std::fs::write(dir.join("signed-target_files.zip"), b"not shipped").unwrap();
    std::fs::write(
        dir.join("image.json"),
        r#"{"version": "17.0.0-16373615", "android_major": 17, "build": 16373615,
            "kernel_cmdline": "console=ttyS0",
            "files": {"disk.vhdx": {"size": 6, "sha256": "x"},
                      "initrd.img": {"size": 9, "sha256": "y"}}}"#,
    )
    .unwrap();

    let (ok, entry, err) = tool(&[
        "image-entry",
        "https://example.invalid/android-17.0.0-16373615",
        dir.to_str().unwrap(),
    ]);
    assert!(ok, "{err}");
    let entry: ImageEntry = serde_json::from_str(&entry).unwrap();
    assert_eq!(entry.version, "17.0.0-16373615");
    assert_eq!(entry.android_major, 17);
    assert_eq!(entry.build, 16373615);
    let names: Vec<_> = entry.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["disk.vhdx", "image.json", "initrd.img"]);
    assert_eq!(
        entry.files[0].url,
        "https://example.invalid/android-17.0.0-16373615/disk.vhdx"
    );
    assert_eq!(entry.files[2].size, 9);

    std::fs::remove_file(dir.join("initrd.img")).unwrap();
    let (ok, _, err) = tool(&[
        "image-entry",
        "https://example.invalid",
        dir.to_str().unwrap(),
    ]);
    assert!(!ok && err.contains("initrd.img"), "{err}");
}

#[test]
fn fr15_a_program_list_written_and_signed_by_the_release_tool_is_accepted() {
    use dsa_host_core::update::program;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("release-program-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("secret.key");
    let key = key.to_str().unwrap();
    let (ok, public, err) = tool(&["keygen", key]);
    assert!(ok, "{err}");

    let setup = dir.join("setup-0.1.1.exe");
    std::fs::write(&setup, b"a setup file").unwrap();
    let url = "https://example.invalid/releases/download/v0.1.1/setup-0.1.1.exe";
    let (ok, list, err) = tool(&["program", "0.1.1", url, setup.to_str().unwrap()]);
    assert!(ok, "{err}");
    let list_path = dir.join("program.json");
    std::fs::write(&list_path, format!("{list}\n")).unwrap();
    let list_path = list_path.to_str().unwrap();
    let (ok, _, err) = tool(&["sign", key, list_path]);
    assert!(ok, "{err}");
    let (ok, out, err) = tool(&["verify-program", &public, list_path]);
    assert!(ok && out.contains("DSA 0.1.1"), "{out} {err}");

    let bytes = std::fs::read(list_path).unwrap();
    let signature = std::fs::read_to_string(format!("{list_path}.sig")).unwrap();
    let trusted = [manifest::parse_key(&public).unwrap()];
    let list = program::verify(&bytes, &signature, &trusted).unwrap();
    assert_eq!(list.version, "0.1.1");
    assert_eq!(list.setup.url, url);
    assert_eq!(list.setup.size, 12);

    // The image list's check does not take a program list, and the other way round.
    let (ok, _, err) = tool(&["verify", &public, list_path]);
    assert!(!ok && err.contains("not usable"), "{err}");

    let (ok, _, err) = tool(&["program", "0.1.1-beta", url, setup.to_str().unwrap()]);
    assert!(!ok && err.contains("dot-separated numbers"), "{err}");
}

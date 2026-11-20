//! Reading an image folder: the same rules for every host, with each host's data.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use dsa_host_core::image_files::{read_image, GuestArch, ImageRules};

/// The rules the Windows host passes.
fn windows<'a>(fallback: &'a Path, extra: &'a str) -> ImageRules<'a> {
    ImageRules {
        kernel_names: &["bzImage", "Image"],
        disk_extensions: &["vhdx"],
        arch: Some(GuestArch::HOST),
        fallback_kernel: Some(fallback),
        extra_cmdline: extra,
    }
}

/// The rules the macOS host passes for a guest of `arch`.
fn macos(arch: GuestArch) -> ImageRules<'static> {
    let kernel_names: &'static [&'static str] = match arch {
        GuestArch::Arm64 => &["Image"],
        GuestArch::X86_64 => &["bzImage"],
    };
    ImageRules {
        kernel_names,
        disk_extensions: &["img", "vhdx"],
        arch: Some(arch),
        fallback_kernel: None,
        extra_cmdline: "",
    }
}

/// The rules the Linux host passes.
fn linux<'a>(fallback: Option<&'a Path>, extra: &'a str) -> ImageRules<'a> {
    ImageRules {
        kernel_names: &["bzImage", "Image"],
        disk_extensions: &["img", "raw", "qcow2", "vhdx"],
        arch: Some(GuestArch::HOST),
        fallback_kernel: fallback,
        extra_cmdline: extra,
    }
}

fn folder(name: &str, manifest: Option<&str>, files: &[&str]) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "image-files-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(manifest) = manifest {
        std::fs::write(dir.join("image.json"), manifest).unwrap();
    }
    for file in files {
        std::fs::write(dir.join(file), b"").unwrap();
    }
    dir
}

#[test]
fn d9_two_disk_image_uses_data_template() {
    let dir = folder(
        "two",
        Some(
            r#"{"version": "16.0.0-1", "arch": "arm64", "kernel_cmdline": "init=/init",
                "files": {"system.vhdx": {}, "data.vhdx": {}}}"#,
        ),
        &["Image"],
    );
    let files = read_image(&dir, &macos(GuestArch::Arm64)).unwrap();
    assert_eq!(files.kernel, dir.join("Image"));
    assert_eq!(files.initrd, Some(dir.join("initrd.img")));
    assert_eq!(files.system_disk, Some(dir.join("system.vhdx")));
    assert_eq!(files.disk_template, Some(dir.join("data.vhdx")));
    assert_eq!(files.kernel_cmdline, "init=/init");
}

#[test]
fn d9_one_disk_image_uses_disk_template() {
    // Windows: the manifest lists no files, and the image's disk is named all the same.
    let dir = folder(
        "one",
        Some(
            r#"{"version": "17.0.0-16373615", "kernel_cmdline": "androidboot.boot_part_uuid=6b1ad5ad androidboot.fstab_suffix=dsa"}"#,
        ),
        &[],
    );
    let files = read_image(&dir, &windows(Path::new(r"C:\vm\bzImage"), "")).unwrap();
    assert_eq!(files.system_disk, None);
    assert_eq!(files.disk_template, Some(dir.join("disk.vhdx")));
    assert_eq!(files.initrd, Some(dir.join("initrd.img")));
    assert_eq!(
        files.kernel_cmdline,
        "androidboot.boot_part_uuid=6b1ad5ad androidboot.fstab_suffix=dsa"
    );
    // Linux: a one disk image with a fallback kernel.
    let dir = folder("one-linux", None, &["disk.vhdx"]);
    let files = read_image(&dir, &linux(Some(Path::new("/opt/dsa/bzImage")), "")).unwrap();
    assert_eq!(files.kernel, PathBuf::from("/opt/dsa/bzImage"));
    assert_eq!(files.system_disk, None);
    assert_eq!(files.disk_template, Some(dir.join("disk.vhdx")));
}

#[test]
fn d9_windows_two_disk_image_lists_system_and_data() {
    let dir = folder(
        "win-two",
        Some(
            r#"{"version": "17.0.0-1", "kernel_cmdline": "androidboot.boot_devices=vmbus androidboot.fstab_suffix=dsa", "files": {"system.vhdx": {}, "data.vhdx": {}}}"#,
        ),
        &[],
    );
    let files = read_image(&dir, &windows(Path::new(r"C:\vm\bzImage"), "")).unwrap();
    assert_eq!(files.system_disk, Some(dir.join("system.vhdx")));
    assert_eq!(files.disk_template, Some(dir.join("data.vhdx")));
    assert_eq!(files.initrd, Some(dir.join("initrd.img")));
}

#[test]
fn d9_raw_disks_are_preferred_when_rules_say_so() {
    let dir = folder(
        "raw",
        Some(r#"{"version": "16.0.0-1", "arch": "x86_64", "files": {"disk.vhdx": {}}}"#),
        &["bzImage", "disk.img"],
    );
    let files = read_image(&dir, &macos(GuestArch::X86_64)).unwrap();
    assert_eq!(files.kernel, dir.join("bzImage"));
    assert_eq!(files.system_disk, None);
    assert_eq!(files.disk_template, Some(dir.join("disk.img")));
    // A host that attaches VHDX only takes the published disk.
    let dir = folder(
        "raw-vhdx",
        Some(r#"{"version": "16.0.0-1", "files": {"disk.vhdx": {}}}"#),
        &["bzImage", "disk.img", "disk.vhdx"],
    );
    let files = read_image(&dir, &windows(Path::new("/k"), "")).unwrap();
    assert_eq!(files.disk_template, Some(dir.join("disk.vhdx")));
    // Linux: a raw copy made beside the VHDX wins over it.
    let dir = folder(
        "raw-linux",
        None,
        &["Image", "system.vhdx", "system.img", "data.vhdx"],
    );
    let files = read_image(&dir, &linux(None, "")).unwrap();
    assert_eq!(files.kernel, dir.join("Image"));
    assert_eq!(files.system_disk, Some(dir.join("system.img")));
    assert_eq!(files.disk_template, Some(dir.join("data.vhdx")));
}

#[test]
fn d9_published_image_boots_its_own_kernel_and_adds_the_hosts_words() {
    let dir = folder(
        "words",
        Some(r#"{"kernel_cmdline": "console=ttyS0 androidboot.boot_devices=vmbus"}"#),
        &["bzImage", "initrd.img", "system.vhdx", "data.vhdx"],
    );
    let files = read_image(&dir, &linux(None, "androidboot.dsa.x=1")).unwrap();
    assert_eq!(files.kernel, dir.join("bzImage"));
    assert_eq!(files.initrd, Some(dir.join("initrd.img")));
    assert_eq!(
        files.kernel_cmdline,
        "console=ttyS0 androidboot.boot_devices=vmbus androidboot.dsa.x=1"
    );
    // The image's own kernel wins over the host's.
    let files = read_image(&dir, &windows(Path::new("/other/bzImage"), "")).unwrap();
    assert_eq!(files.kernel, dir.join("bzImage"));
}

#[test]
fn d9_wrong_arch_image_is_refused() {
    let dir = folder(
        "arch",
        Some(r#"{"arch": "arm64", "files": {"system.vhdx": {}, "data.vhdx": {}}}"#),
        &["Image"],
    );
    let why = read_image(&dir, &macos(GuestArch::X86_64)).unwrap_err();
    assert!(
        why.contains("is for arm64") && why.contains("runs x86_64 guests only"),
        "{why}"
    );
    // An image naming no architecture is taken as it is, and `aarch64` names arm64.
    let dir = folder(
        "no-arch",
        Some(r#"{"files": {"disk.vhdx": {}}}"#),
        &["Image"],
    );
    assert!(read_image(&dir, &macos(GuestArch::Arm64)).is_ok());
    let dir = folder("aarch64", Some(r#"{"arch": "aarch64"}"#), &["Image"]);
    assert!(read_image(&dir, &macos(GuestArch::Arm64)).is_ok());
    let dir = folder("riscv", Some(r#"{"arch": "riscv64"}"#), &["Image"]);
    assert!(read_image(&dir, &macos(GuestArch::Arm64)).is_err());
}

#[test]
fn d9_the_arch_is_not_checked_when_the_rules_name_none() {
    let dir = folder("unchecked", Some(r#"{"arch": "riscv64"}"#), &["bzImage"]);
    let mut rules = linux(None, "");
    rules.arch = None;
    assert!(read_image(&dir, &rules).is_ok());
}

#[test]
fn d9_image_without_manifest_falls_back_to_system_vhdx() {
    let dir = folder("legacy", None, &[]);
    let files = read_image(&dir, &windows(Path::new(r"C:\vm\bzImage"), "console=hvc0")).unwrap();
    assert_eq!(files.system_disk, Some(dir.join("system.vhdx")));
    assert_eq!(files.disk_template, None);
    assert_eq!(files.kernel, PathBuf::from(r"C:\vm\bzImage"));
    assert_eq!(files.initrd, None);
    assert_eq!(files.kernel_cmdline, "console=hvc0");
    std::fs::write(dir.join("bzImage"), b"kernel").unwrap();
    let files = read_image(&dir, &windows(Path::new(r"C:\vm\bzImage"), "")).unwrap();
    assert_eq!(files.kernel, dir.join("bzImage"), "the image's own kernel");
}

#[test]
fn d9_an_arm64_image_boots_its_own_kernel_named_image() {
    let dir = folder("arm64-kernel", None, &["Image"]);
    let files = read_image(&dir, &windows(Path::new(r"C:\vm\bzImage"), "")).unwrap();
    assert_eq!(files.kernel, dir.join("Image"));
}

#[test]
fn d9_an_image_with_no_kernel_and_no_fallback_is_refused() {
    let dir = folder("no-kernel", Some(r#"{}"#), &[]);
    let why = read_image(&dir, &macos(GuestArch::Arm64)).unwrap_err();
    assert!(
        why.contains("has no kernel") && why.contains("Image"),
        "{why}"
    );
}

#[test]
fn d9_a_manifest_that_is_not_json_is_refused() {
    let dir = folder("broken", Some("{ not json"), &["bzImage"]);
    let why = read_image(&dir, &windows(Path::new("/k"), "")).unwrap_err();
    assert!(
        why.contains("image.json") && why.contains("not valid"),
        "{why}"
    );
}

//! The Android VM launcher on Windows, where no guest image exists to boot.
//!
//! These check that failures are reported, not that a VM boots: booting needs the guest
//! kernel and system image, which are checked by hand on a real machine.
#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use dsa_host_core::session::GuestLauncher;
use dsa_host_windows::android_vm::{prepare_boot, AndroidVmLauncher};
use dsa_host_windows::vm::{AndroidVmConfig, GuestArch};
use dsa_protocol::auth::BootSecret;

fn secret() -> BootSecret {
    BootSecret::from_bytes([0x5e; 32])
}

/// A configuration whose kernel and disks do not exist. The initrd does, since the boot's
/// secret is added to a copy of it before anything else happens.
fn missing_files() -> AndroidVmConfig {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("no-such-vm");
    std::fs::create_dir_all(&dir).unwrap();
    let initrd = dir.join("initrd.img");
    // Written once: the tests run in parallel, and rewriting it would let another test read
    // it while it is empty.
    static WRITTEN: std::sync::Once = std::sync::Once::new();
    WRITTEN.call_once(|| std::fs::write(&initrd, b"ramdisk").unwrap());
    AndroidVmConfig {
        owner: "DSA test".into(),
        kernel: dir.join("kernel"),
        initrd: Some(initrd),
        kernel_cmdline: String::new(),
        memory_mb: 1024,
        processors: 1,
        system_image: Some(dir.join("system.vhdx")),
        data_disk: dir.join("data.vhdx"),
        pstore_disk: None,
        addon_disk: None,
        socket_sids: Vec::new(),
        page_reporting_order: 9,
        console_pipe: None,
        image_version: None,
        hosting_process_name_suffix: None,
        fine_grained_memory: false,
        network_adapter: None,
        direct_mapped: None,
    }
}

#[test]
fn fr2_a_vm_that_cannot_start_is_reported_and_nothing_is_left_running() {
    let mut launcher = AndroidVmLauncher::new("dsa-test-missing-files", missing_files());
    let error = launcher
        .start(&secret())
        .expect_err("there is nothing to boot");
    assert!(!error.is_empty());
    assert!(launcher.runtime_id().is_err(), "no VM is left behind");
    launcher
        .stop(Duration::from_secs(5))
        .expect("stopping nothing succeeds");
}

#[test]
fn fr17_the_boot_initrd_carries_a_changed_device_identity_before_the_bootconfig() {
    use dsa_host_windows::android_vm::write_boot_initrd;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("boot-identity");
    std::fs::create_dir_all(&dir).unwrap();
    let initrd = dir.join("initrd.img");
    std::fs::write(&initrd, b"ramdisk").unwrap();

    let with = dir.join("with.initrd");
    let file = write_boot_initrd(
        &initrd,
        Some("ro.product.model=My tablet\n"),
        &secret(),
        "",
        &with,
    )
    .unwrap();
    let data = std::fs::read(&with).unwrap();
    assert!(
        data.starts_with(b"ramdisk\x00070701"),
        "padded to four bytes"
    );
    let at = |needle: &[u8]| data.windows(needle.len()).position(|w| w == needle);
    let props = at(b"ro.product.model=My tablet\n").expect("the identity is in the initrd");
    let config = at(b"#BOOTCONFIG\n").expect("the bootconfig still ends it");
    assert!(at(b"system/etc/ramdisk/build.prop").is_some());
    assert!(props < config);
    drop(file);
    assert!(!with.exists(), "the copy goes once the VM has read it");

    let without = dir.join("without.initrd");
    let _file = write_boot_initrd(&initrd, None, &secret(), "", &without).unwrap();
    let data = std::fs::read(&without).unwrap();
    assert!(data.starts_with(b"ramdisk"));
    assert!(!data.windows(6).any(|w| w == b"070701"), "nothing added");
}

#[test]
fn fr2_an_invalid_configuration_is_refused_before_hcs_is_called() {
    let mut config = missing_files();
    config.processors = 0;
    let mut launcher = AndroidVmLauncher::new("dsa-test-invalid", config);
    let error = launcher.start(&secret()).unwrap_err();
    assert!(error.contains("at least one processor"), "{error}");
}

#[test]
fn fr2_a_failed_start_keeps_a_console_log_and_frees_the_pipe_for_the_next_start() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("android-vm-console");
    let _ = std::fs::remove_dir_all(&dir);
    let log = dir.join("android-test.log");
    let mut config = missing_files();
    config.console_pipe = Some(format!(
        r"\\.\pipe\dsa-console-test-launcher-{}",
        std::process::id()
    ));
    config.image_version = Some("17.0.0-1".into());
    let mut launcher =
        AndroidVmLauncher::new("dsa-test-console", config).with_console_log(log.clone());

    for attempt in 1..=2 {
        launcher
            .start(&secret())
            .expect_err("there is nothing to boot");
        launcher
            .stop(Duration::from_secs(5))
            .expect("stopping what was left succeeds");
        let report = launcher.diagnostics().expect("the console is reported");
        assert!(
            !report.contains("not kept"),
            "attempt {attempt}: the console pipe was free: {report}"
        );
        assert!(report.contains(&log.display().to_string()), "{report}");
        assert_eq!(launcher.exited(), None, "no VM is left to exit");
    }
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        text.matches("==== DSA ").count(),
        2,
        "a header per boot: {text}"
    );
    assert!(text.contains("image 17.0.0-1"), "{text}");
    assert!(text.contains(GuestArch::HOST.console_cmdline()), "{text}");
}

#[test]
fn fr27_an_arm64_boot_gets_a_folder_of_its_own_with_the_kernel_and_the_initrd() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("android-vm-arm64-boot");
    let _ = std::fs::remove_dir_all(&root);
    let image = root.join("image");
    let boot_dir = root.join("tmp");
    std::fs::create_dir_all(&image).unwrap();
    std::fs::create_dir_all(&boot_dir).unwrap();
    // Another boot's file in the same tmp folder, which the VM must not be able to read.
    let other = boot_dir.join("other.initrd");
    std::fs::write(&other, b"another boot's secret").unwrap();
    let mut config = missing_files();
    config.kernel = image.join("Image");
    std::fs::write(&config.kernel, b"MZ kernel").unwrap();

    let boot = prepare_boot(
        GuestArch::Arm64,
        &mut config,
        None,
        &secret(),
        "",
        &boot_dir,
        "dsa-7-0",
    )
    .expect("the boot folder is written");
    let folder = boot_dir.join("dsa-7-0");
    assert_eq!(config.kernel, folder.join("Image"));
    assert_eq!(config.initrd, Some(folder.join("initrd.img")));
    assert_eq!(boot.initrd(), folder.join("initrd.img"));
    assert_eq!(std::fs::read(&config.kernel).unwrap(), b"MZ kernel");
    let initrd = std::fs::read(boot.initrd()).unwrap();
    assert!(initrd.starts_with(b"ramdisk"));
    let mut names: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["Image", "initrd.img"], "only this boot's files");
    assert_eq!(
        boot.vm_access(),
        [
            folder.clone(),
            folder.join("Image"),
            folder.join("initrd.img")
        ]
    );
    // The document the launcher makes from it boots from that folder.
    let doc = config.to_hcs_document_for(GuestArch::Arm64).unwrap();
    assert_eq!(
        doc["VirtualMachine"]["Chipset"]["Uefi"]["BootThis"]["VmbFsRootPath"],
        folder.display().to_string()
    );

    drop(boot);
    assert!(!folder.exists(), "removed with the boot");
    assert!(other.exists());
}

#[test]
fn fr27_an_x86_64_boot_keeps_its_initrd_beside_the_others() {
    let boot_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("android-vm-x86-boot");
    let _ = std::fs::remove_dir_all(&boot_dir);
    std::fs::create_dir_all(&boot_dir).unwrap();
    let mut config = missing_files();
    let kernel = config.kernel.clone();
    let boot = prepare_boot(
        GuestArch::X86_64,
        &mut config,
        None,
        &secret(),
        "",
        &boot_dir,
        "dsa-7-1",
    )
    .expect("the initrd is written");
    assert_eq!(config.kernel, kernel, "booted from where it is");
    assert_eq!(boot.initrd(), boot_dir.join("dsa-7-1.initrd"));
    assert_eq!(boot.vm_access(), [boot_dir.join("dsa-7-1.initrd")]);
    drop(boot);
    assert_eq!(std::fs::read_dir(&boot_dir).unwrap().count(), 0);
}

#[test]
fn pr6_the_initrd_with_the_boots_secret_is_gone_once_the_start_is_over() {
    let boot_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("android-vm-boot");
    let _ = std::fs::remove_dir_all(&boot_dir);
    std::fs::create_dir_all(&boot_dir).unwrap();
    let mut launcher = AndroidVmLauncher::new("dsa-test-boot-initrd", missing_files())
        .with_boot_dir(boot_dir.clone());
    launcher
        .start(&secret())
        .expect_err("there is nothing to boot");
    let left: Vec<_> = std::fs::read_dir(&boot_dir).unwrap().collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn pr6_a_vm_without_an_initrd_is_refused_because_the_secret_cannot_reach_it() {
    let mut config = missing_files();
    config.initrd = None;
    let mut launcher = AndroidVmLauncher::new("dsa-test-no-initrd", config);
    let error = launcher.start(&secret()).unwrap_err();
    assert!(error.contains("initrd"), "{error}");
}

#[test]
fn nfr1_a_boot_falls_back_to_the_image_disk_while_the_direct_mapped_disk_is_made() {
    use dsa_host_core::direct_mapped_disk::{DirectMappedImages, Store, FORMAT};
    use dsa_host_windows::android_vm::direct_mapped_disks;
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("direct-mapped-fallback");
    let _ = std::fs::remove_dir_all(&root);
    let image = root.join("images").join("17.0.0-1-1");
    std::fs::create_dir_all(&image).unwrap();
    let manifest = serde_json::json!({
        "version": "17.0.0-1-1",
        "direct_mapped": {
            "fstab_suffix": "dsa_dax",
            "verified_partitions": ["odm"],
            "system_disk": {"format": FORMAT, "bytes": 2u64 << 30, "sha256": "ab".repeat(32),
                            "super_bytes": 1u64 << 30},
        },
    });
    std::fs::write(image.join("image.json"), manifest.to_string()).unwrap();
    let mut config = missing_files();
    config.system_image = Some(image.join("system.vhdx"));
    // The maker never gets to the image here: it waits until the test is over.
    let (hold, held) = std::sync::mpsc::channel::<()>();
    let images = DirectMappedImages::start(
        Store::new(root.join("direct-mapped")),
        Box::new(|| None),
        Box::new(move || {
            let _ = held.recv();
            Ok(())
        }),
        std::sync::Arc::new(|_: &str| {}),
    );
    let why = direct_mapped_disks(&config, &images).expect_err("no copy is made yet");
    if GuestArch::HOST == GuestArch::X86_64 {
        assert!(why.contains("being made in the background"), "{why}");
    }
    // Nothing is mapped: the VM attaches the image's own disk on SCSI, with dm-verity.
    let document = config.to_hcs_document().unwrap();
    assert!(document["VirtualMachine"]["Devices"]["VirtualPMem"].is_null());
    assert!(document.to_string().contains("system.vhdx"));
    drop(hold);
    images.stop();
    let _ = std::fs::remove_dir_all(&root);
}

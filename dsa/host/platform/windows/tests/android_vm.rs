//! The Android VM launcher on Windows, where no guest image exists to boot.
//!
//! These check that failures are reported, not that a VM boots: booting needs the guest
//! kernel and system image, which are checked by hand on a real machine.
#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use dsa_host_core::session::GuestLauncher;
use dsa_host_windows::android_vm::AndroidVmLauncher;
use dsa_host_windows::vm::AndroidVmConfig;
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
    std::fs::write(&initrd, b"ramdisk").unwrap();
    AndroidVmConfig {
        owner: "DSA test".into(),
        kernel: dir.join("kernel"),
        initrd: Some(initrd),
        kernel_cmdline: String::new(),
        memory_mb: 1024,
        processors: 1,
        system_image: Some(dir.join("system.vhdx")),
        data_disk: dir.join("data.vhdx"),
        socket_sids: Vec::new(),
        page_reporting_order: 9,
        console_pipe: None,
        image_version: None,
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
    assert!(text.contains("console=ttyS0,115200"), "{text}");
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

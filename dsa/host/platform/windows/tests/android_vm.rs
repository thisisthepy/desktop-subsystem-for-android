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

fn missing_files() -> AndroidVmConfig {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("no-such-vm");
    AndroidVmConfig {
        owner: "DSA test".into(),
        kernel: dir.join("kernel"),
        initrd: None,
        kernel_cmdline: String::new(),
        memory_mb: 1024,
        processors: 1,
        system_image: Some(dir.join("system.vhdx")),
        data_disk: dir.join("data.vhdx"),
        socket_sids: Vec::new(),
        page_reporting_order: 9,
    }
}

#[test]
fn fr2_a_vm_that_cannot_start_is_reported_and_nothing_is_left_running() {
    let mut launcher = AndroidVmLauncher::new("dsa-test-missing-files", missing_files());
    let error = launcher.start().expect_err("there is nothing to boot");
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
    let error = launcher.start().unwrap_err();
    assert!(error.contains("at least one processor"), "{error}");
}

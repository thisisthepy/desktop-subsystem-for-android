//! The VM's memory growth from the host's policy to crosvm: `host/core`'s growth decides, the
//! launcher turns each size into the virtio-mem requested size on crosvm's control socket.
//!
//! A stand-in crosvm records every command it is run with; its `run` serves no VM, only a
//! control socket file and one console line, which is all the launcher waits for.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dsa_host_core::memory::{GrowthAction, GuestPressure, MemoryGrowth};
use dsa_host_core::session::GuestLauncher;
use dsa_host_linux::crosvm::guest_cid;
use dsa_host_linux::image::ImageFiles;
use dsa_host_linux::launcher::{BootPlan, LauncherSetup, LinuxVmLauncher};
use dsa_host_linux::users::Account;
use dsa_protocol::auth::BootSecret;

const MIB: u64 = 1024 * 1024;

fn scratch() -> PathBuf {
    // Short: the control socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("dsa-grow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A crosvm that logs its arguments to `calls`. `run` makes the control socket's file, prints
/// a console line and waits until `stop` removes the file.
fn fake_crosvm(dir: &Path) -> PathBuf {
    let program = dir.join("crosvm");
    let log = dir.join("calls");
    std::fs::write(
        &program,
        format!(
            r#"#!/bin/sh
echo "$@" >> '{log}'
if [ "$2" = run ]; then
    sock=""
    prev=""
    for a in "$@"; do
        [ "$prev" = --socket ] && sock="$a"
        prev="$a"
    done
    : > "$sock"
    echo "[    0.000000] Linux version stand-in"
    while [ -e "$sock" ]; do sleep 0.05; done
    exit 0
fi
[ "$1" = stop ] && rm -f "$2"
exit 0
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

fn pressure(total_mb: u64, available_mb: u64) -> GuestPressure {
    GuestPressure {
        mem_total_kb: total_mb * 1024,
        mem_available_kb: available_mb * 1024,
        stall_percent: 0.0,
    }
}

#[test]
fn nfr2_memory_growth_sets_the_virtio_mem_request() {
    let dir = scratch();
    let kernel = dir.join("bzImage");
    std::fs::write(&kernel, b"kernel").unwrap();
    let initrd = dir.join("initrd.img");
    std::fs::write(&initrd, b"070701").unwrap();
    let files = ImageFiles {
        kernel,
        initrd: Some(initrd),
        kernel_cmdline: "androidboot.boot_devices=vmbus".into(),
        system_image: None,
        disk_template: None,
    };
    let mut launcher = LinuxVmLauncher::new(
        LauncherSetup {
            account: Account::current().unwrap(),
            name: "dsa-grow-test".into(),
            guest_cid: guest_cid(Account::current().unwrap().uid).wrapping_add(0x2000_0000),
            crosvm: fake_crosvm(&dir),
            sandbox: false,
            processors: 1,
            page_reporting_order: 9,
            data_disk: dir.join("data.img"),
            pstore_disk: None,
            run_dir: dir.join("run"),
            console_log: dir.join("console.log"),
            grow_memory: true,
        },
        Box::new(move || {
            Ok(BootPlan {
                files: files.clone(),
                image_version: Some("test".into()),
                memory_mb: 3072,
                memory_max_mb: Some(4096),
            })
        }),
    );
    let secret = BootSecret::generate().unwrap();
    launcher
        .start(&secret)
        .unwrap_or_else(|why| panic!("{why}\n{:?}", launcher.diagnostics()));

    // The VM boots at its base, with the rest as virtio-mem, and no balloon holding memory.
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    let run = calls.lines().next().unwrap();
    assert!(run.contains("--mem size=3072 --virtio-mem 1024"), "{run}");
    assert!(!run.contains("--init-mem"), "{run}");
    assert!(run.contains("memory_hotplug.memmap_on_memory=1"), "{run}");
    assert_eq!(launcher.memory_range_mb(), Some((3072, 4096)));

    // The session's policy, as the session drives it.
    let (base, max) = launcher.memory_range_mb().unwrap();
    let mut growth = MemoryGrowth::new(base, max);
    let start = Instant::now();
    // Short of memory: grow to the maximum, all of the virtio-mem region.
    let Some(GrowthAction::Resize(mb)) = growth.on_pressure(&pressure(2900, 200), start) else {
        panic!("a guest short of memory did not grow");
    };
    assert_eq!(mb, 4096);
    launcher.set_memory_mb(mb).unwrap();
    // The first calm report after the growth settles gives it back at once, to the base:
    // nothing of the region is requested any more.
    let calm = start + Duration::from_secs(5);
    let Some(GrowthAction::Resize(mb)) = growth.on_pressure(&pressure(4000, 3600), calm) else {
        panic!("a calm guest kept the memory it no longer needs");
    };
    assert_eq!(mb, 3072);
    launcher.set_memory_mb(mb).unwrap();

    let sock = dir.join("run").join("crosvm.sock");
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    let resizes: Vec<&str> = calls
        .lines()
        .filter(|line| line.starts_with("virtio-mem "))
        .collect();
    assert_eq!(
        resizes,
        [
            format!("virtio-mem {} {}", 1024 * MIB, sock.display()),
            format!("virtio-mem 0 {}", sock.display()),
        ]
    );
    assert!(!calls.contains("balloon "), "{calls}");

    launcher.stop(Duration::from_millis(200)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

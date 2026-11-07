//! The launcher against the real crosvm under KVM.
//!
//! Two runs, each when CI gives it what it needs:
//! - `DSA_CROSVM` names the crosvm to run, `DSA_CROSVM_TEST_KERNEL` a kernel to boot and
//!   `DSA_CROSVM_TEST_INITRD` an initrd whose init brings up the virtio balloon and virtio-mem
//!   drivers and idles: the command line it builds is one crosvm takes, and the VM can be
//!   resized through its virtio-mem device, suspended, resumed and stopped through the control
//!   socket. The kernel need not reach Android.
//! - `DSA_CROSVM_TEST_IMAGE` names a published Android image folder as well: the real image
//!   boots under crosvm until the guest's control daemon connects on its vsock port.
//!
//! Both need `/dev/kvm` and `/dev/vhost-vsock`.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dsa_host_core::session::GuestLauncher;
use dsa_host_linux::crosvm::guest_cid;
use dsa_host_linux::image::{image_files, make_disk, raw_disk, ImageFiles};
use dsa_host_linux::launcher::{BootPlan, LauncherSetup, LinuxVmLauncher};
use dsa_host_linux::users::Account;
use dsa_host_linux::vsock::{VsockHub, VMADDR_CID_ANY};
use dsa_protocol::auth::BootSecret;

/// The guest's control daemon connects to the host on this port.
const CONTROL_PORT: u32 = 5000;

fn scratch(name: &str) -> PathBuf {
    // Short: crosvm's control socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("crosvm-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn launcher(
    dir: &Path,
    crosvm: PathBuf,
    cid: u32,
    files: ImageFiles,
    memory: (u32, u32),
) -> LinuxVmLauncher {
    let account = Account::current().unwrap();
    LinuxVmLauncher::new(
        LauncherSetup {
            account,
            name: "dsa-test".into(),
            guest_cid: cid,
            crosvm,
            sandbox: true,
            processors: 2,
            page_reporting_order: 9,
            data_disk: dir.join("data.img"),
            pstore_disk: Some(dir.join("pstore.img")),
            run_dir: dir.join("run"),
            console_log: dir.join("console.log"),
            grow_memory: true,
        },
        Box::new(move || {
            Ok(BootPlan {
                files: files.clone(),
                image_version: Some("test".into()),
                memory_mb: memory.0,
                memory_max_mb: Some(memory.1),
            })
        }),
    )
}

fn crosvm() -> Option<PathBuf> {
    std::env::var_os("DSA_CROSVM").map(PathBuf::from)
}

/// An address clear of the ones the service gives users' VMs, different for each test.
fn test_cid(offset: u32) -> u32 {
    guest_cid(Account::current().unwrap().uid).wrapping_add(0x1000_0000 + offset)
}

#[test]
fn fr2_crosvm_starts_resizes_freezes_and_stops_the_vm() {
    let (Some(crosvm), Some(kernel), Some(test_initrd)) = (
        crosvm(),
        std::env::var_os("DSA_CROSVM_TEST_KERNEL").map(PathBuf::from),
        std::env::var_os("DSA_CROSVM_TEST_INITRD").map(PathBuf::from),
    ) else {
        eprintln!(
            "skipped: DSA_CROSVM, DSA_CROSVM_TEST_KERNEL and DSA_CROSVM_TEST_INITRD name no \
             crosvm, kernel and initrd"
        );
        return;
    };
    let dir = scratch("kernel");
    let initrd = dir.join("initrd.img");
    std::fs::copy(&test_initrd, &initrd).unwrap();
    let account = Account::current().unwrap();
    make_disk(&account, &dir.join("data.img"), None, 1).unwrap();
    let files = ImageFiles {
        kernel,
        initrd: Some(initrd),
        kernel_cmdline: "androidboot.boot_devices=vmbus".into(),
        system_image: None,
        disk_template: None,
    };
    let mut launcher = launcher(&dir, crosvm, test_cid(0), files, (512, 1024));
    assert_eq!(launcher.prerequisite(), None);
    let secret = BootSecret::generate().unwrap();
    launcher
        .start(&secret)
        .unwrap_or_else(|why| panic!("{why}\n{:?}", launcher.diagnostics()));
    assert!(launcher.vm_id().is_some());
    assert_eq!(launcher.memory_range_mb(), Some((512, 1024)));
    // Retried while the guest's drivers come up; crosvm takes a virtio-mem request at once.
    let deadline = Instant::now() + Duration::from_secs(60);
    while let Err(why) = launcher.set_memory_mb(768) {
        if Instant::now() >= deadline {
            panic!(
                "the VM could not be resized within 60 s: {why}\n{:?}",
                launcher.diagnostics()
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    launcher.set_memory_mb(512).unwrap();
    launcher.freeze().unwrap();
    launcher.thaw().unwrap();
    launcher.stop(Duration::from_secs(5)).unwrap();
    assert!(launcher.vm_id().is_none());
    // The boot's initrd, which carried the secret, is gone.
    let left: Vec<_> = std::fs::read_dir(dir.join("run"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .filter(|name| name.to_string_lossy().ends_with(".initrd"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let console = std::fs::read_to_string(dir.join("console.log")).unwrap();
    assert!(console.contains("==== DSA boot at"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr2_the_published_image_boots_under_crosvm_until_its_daemon_connects() {
    let (Some(crosvm), Some(image)) = (
        crosvm(),
        std::env::var_os("DSA_CROSVM_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_CROSVM_TEST_IMAGE name no crosvm and image");
        return;
    };
    let dir = scratch("image");
    let account = Account::current().unwrap();
    let mut files = image_files(&image, None, "").unwrap();
    files.system_image = files
        .system_image
        .as_deref()
        .map(raw_disk)
        .transpose()
        .unwrap();
    make_disk(
        &account,
        &dir.join("data.img"),
        files.disk_template.as_deref(),
        8,
    )
    .unwrap();
    let cid = test_cid(1);
    let hub = VsockHub::bind(VMADDR_CID_ANY, CONTROL_PORT).expect("binding the control port");
    let mut daemon = hub.listener(cid, true);
    let mut launcher = launcher(&dir, crosvm, cid, files, (3072, 4096));
    let secret = BootSecret::generate().unwrap();
    let started = Instant::now();
    launcher
        .start(&secret)
        .unwrap_or_else(|why| panic!("{why}\n{:?}", launcher.diagnostics()));
    // Waited for in short steps, so a VM that exits during boot fails the test at once
    // instead of after the whole 600 s.
    let deadline = Instant::now() + Duration::from_secs(600);
    let connected = loop {
        match daemon.accept_stream(Duration::from_secs(2)) {
            Ok(stream) => break Ok(stream),
            Err(why) => {
                if let Some(exit) = launcher.exited() {
                    break Err(format!("{exit} after {} s", started.elapsed().as_secs()));
                }
                if Instant::now() >= deadline {
                    break Err(format!(
                        "the guest's daemon did not connect within 600 s: {why:?}"
                    ));
                }
            }
        }
    };
    let report = launcher.diagnostics();
    let stopped = launcher.stop(Duration::from_secs(30));
    if let Err(why) = &connected {
        panic!("{why}\n{}", report.unwrap_or_default());
    }
    eprintln!(
        "the guest's daemon connected {} s after the start",
        started.elapsed().as_secs()
    );
    stopped.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

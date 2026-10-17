//! dsa-hostd installed as a real Windows service, reached through its named pipe.
//!
//! It registers a service on the machine, so it runs only when `DSA_SERVICE_TESTS=1` (CI
//! sets it on the Windows runner) and needs administrator rights. The VM's kernel does not
//! exist, so starting the caller's Android fails; the test checks that the service creates
//! the caller's data disk, reports why the VM did not start, keeps serving, and stops
//! cleanly.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::session::SessionState;
use dsa_host_windows::pipe::{current_user_sid, NamedPipeConnector};

fn sc(args: &[&str]) -> String {
    let output = Command::new("sc.exe")
        .args(args)
        .output()
        .expect("running sc.exe");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(200));
    }
}

/// The registered service, stopped and deleted on drop even when the test fails.
struct Installed(String);

impl Installed {
    fn state(&self) -> String {
        sc(&["query", &self.0])
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        sc(&["stop", &self.0]);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.state().contains("STOPPED") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(200));
        }
        sc(&["delete", &self.0]);
    }
}

fn write_config(dir: &Path, pipe: &str) -> PathBuf {
    let missing = dir.join("missing");
    let config = serde_json::json!({
        "kernel": missing.join("bzImage"),
        "system_image": missing.join("system.vhdx"),
        "data_dir": dir.join("users"),
        "data_disk_size_gb": 8,
        "pipe_name": pipe,
        // The test runs as a non-interactive account, which the default would not admit.
        "pipe_sddl": "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)",
        "log_file": dir.join("dsa-hostd.log"),
        "connect_timeout_secs": 5,
        "stop_timeout_secs": 5,
    });
    let path = dir.join("dsa-hostd.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    path
}

#[test]
fn pr5_the_installed_service_serves_each_caller_their_own_android() {
    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to install and run the service");
        return;
    }
    let id = std::process::id();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("service-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pipe = format!("dsa-test-service-{id}");
    let config = write_config(&dir, &pipe);
    let name = format!("dsa-test-{id}");
    let bin_path = format!(
        "\"{}\" service \"{}\"",
        env!("CARGO_BIN_EXE_dsa-hostd"),
        config.display()
    );

    let created = sc(&["create", &name, "binPath=", &bin_path, "start=", "demand"]);
    assert!(created.contains("SUCCESS"), "sc create: {created}");
    let service = Installed(name.clone());
    let started = sc(&["start", &name]);
    assert!(
        started.contains("RUNNING") || started.contains("START_PENDING"),
        "sc start: {started}"
    );

    let mut connector = NamedPipeConnector { name: pipe };
    let mut client = None;
    wait_until("the service's pipe", Duration::from_secs(20), || {
        client =
            HostApiClient::connect(&mut connector, "service test", Duration::from_secs(5)).ok();
        client.is_some()
    });
    let mut client = client.unwrap();

    let status = client.status().expect("status");
    assert_eq!(status.state, SessionState::Stopped);

    client.start().expect("start is accepted");
    let mut last = None;
    wait_until(
        "the failed start to be reported",
        Duration::from_secs(60),
        || {
            let status = client.status().expect("status");
            let done = status.state == SessionState::Stopped && status.last_reason.is_some();
            last = Some(status);
            done
        },
    );
    let reason = last.unwrap().last_reason.unwrap();
    eprintln!("the service reported: {reason}");

    let sid = current_user_sid().unwrap();
    let disk = dir.join("users").join(&sid).join("data.vhdx");
    assert!(
        disk.exists(),
        "the caller's data disk {} was created",
        disk.display()
    );

    // The service keeps serving after the failure.
    assert_eq!(client.status().unwrap().state, SessionState::Stopped);

    let stopped = sc(&["stop", &name]);
    assert!(
        stopped.contains("STOP_PENDING") || stopped.contains("STOPPED"),
        "sc stop: {stopped}"
    );
    wait_until("the service to stop", Duration::from_secs(60), || {
        service.state().contains("STOPPED")
    });
    let log = std::fs::read_to_string(dir.join("dsa-hostd.log")).unwrap();
    assert!(log.contains("serving the local API"), "log: {log}");
    assert!(log.contains("stopping every user's Android"), "log: {log}");
}

#[test]
fn fr14_a_new_image_that_does_not_boot_is_undone_by_the_service() {
    use dsa_host_core::update::manifest::{FileEntry, ImageEntry, Manifest, FORMAT};
    use dsa_host_core::update::policy::InstalledImage;
    use dsa_host_core::update::store::{FileFetch, ImageStore};
    use dsa_host_windows::host_config::HostConfig;
    use dsa_host_windows::service::{create_user_android, Updates};
    use sha2::{Digest, Sha256};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to create VMs");
        return;
    }
    let id = std::process::id();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("service-update-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let images = dir.join("images");
    // Neither image is a real Android image, so neither VM can be created.
    std::fs::create_dir_all(images.join("images").join("16.0.1")).unwrap();
    let first = InstalledImage {
        version: "16.0.1".into(),
        android_major: 16,
        build: 1,
    };
    let store = Arc::new(ImageStore::open(&images, &first).unwrap());
    let offered_file = dir.join("offered.vhdx");
    std::fs::write(&offered_file, b"not an image").unwrap();
    let offered = ImageEntry {
        version: "16.0.2".into(),
        android_major: 16,
        build: 2,
        files: vec![FileEntry {
            name: "system.vhdx".into(),
            url: offered_file.to_string_lossy().into_owned(),
            size: 12,
            sha256: Sha256::digest(b"not an image")
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        }],
    };
    let manifest = Manifest {
        format: FORMAT,
        supported_majors: vec![16],
        images: vec![offered.clone()],
    };
    store.stage(&offered, &manifest, &FileFetch).unwrap();

    let config = HostConfig::parse(
        &serde_json::json!({
            "kernel": dir.join("missing").join("bzImage"),
            "images_dir": images,
            "first_image": first,
            "data_dir": dir.join("users"),
            "data_disk_size_gb": 8,
            "connect_timeout_secs": 5,
            "stop_timeout_secs": 5,
        })
        .to_string(),
    )
    .unwrap();
    let updates = Updates {
        store: Arc::clone(&store),
        notices: Default::default(),
        boot_timeout: Duration::from_secs(10),
        trial_running: Arc::new(AtomicBool::new(false)),
    };
    let sid = current_user_sid().unwrap();
    let android = create_user_android(&config, "service test", &sid, Some(&updates)).unwrap();
    android.start();
    let mut notice = None;
    wait_until("the trial to end", Duration::from_secs(60), || {
        notice = android.update_notice();
        notice.is_some()
    });
    let notice = notice.unwrap();
    eprintln!("the service told the user: {notice}");
    assert!(
        notice.contains("16.0.2") && notice.contains("16.0.1"),
        "{notice}"
    );
    let state = store.state().unwrap();
    assert_eq!(
        state.current.version, "16.0.1",
        "the service went back to the first image"
    );
    assert_eq!(state.failed, ["16.0.2"]);
    assert!(!state.on_trial);
}

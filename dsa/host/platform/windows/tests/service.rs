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

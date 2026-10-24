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

#[test]
fn fr15_the_installed_service_runs_a_signed_setup_and_reports_when_it_fails() {
    use dsa_host_core::update::program::{ProgramManifest, SetupFile, FORMAT};
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};

    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to install and run the service");
        return;
    }
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    let id = std::process::id();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("service-program {id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("images").join("images").join("16.0.1")).unwrap();

    // The stand-in setup is dsa-hostd itself: it refuses setup's options and ends with exit
    // code 1 without replacing anything, which is how a failed setup looks to the service.
    let mirror = dir.join("mirror");
    std::fs::create_dir_all(&mirror).unwrap();
    let setup = mirror.join("setup-9.9.9.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_dsa-hostd"), &setup).unwrap();
    let contents = std::fs::read(&setup).unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    let list = serde_json::to_vec(&ProgramManifest {
        format: FORMAT,
        version: "9.9.9".into(),
        setup: SetupFile {
            url: setup.to_string_lossy().into_owned(),
            size: contents.len() as u64,
            sha256: hex(&Sha256::digest(&contents)),
        },
    })
    .unwrap();
    let list_path = mirror.join("program.json");
    std::fs::write(&list_path, &list).unwrap();
    std::fs::write(
        mirror.join("program.json.sig"),
        hex(&key.sign(&list).to_bytes()),
    )
    .unwrap();

    let pipe = format!("dsa-test-program-{id}");
    let config = serde_json::json!({
        "kernel": dir.join("missing").join("bzImage"),
        "images_dir": dir.join("images"),
        "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
        "data_dir": dir.join("users"),
        "pipe_name": pipe,
        "pipe_sddl": "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)",
        "log_file": dir.join("logs").join("dsa-hostd.log"),
        "stop_timeout_secs": 5,
        "update": {
            "list_url": mirror.join("images.json"),
            "program_list_url": list_path,
            "program_dir": dir.join("updates"),
            "trusted_keys": [hex(key.verifying_key().as_bytes())],
        },
    });
    let config_path = dir.join("dsa-hostd.json");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let name = format!("dsa-test-program-{id}");
    let bin_path = format!(
        "\"{}\" service \"{}\"",
        env!("CARGO_BIN_EXE_dsa-hostd"),
        config_path.display()
    );
    let created = sc(&["create", &name, "binPath=", &bin_path, "start=", "demand"]);
    assert!(created.contains("SUCCESS"), "sc create: {created}");
    let _service = Installed(name.clone());
    sc(&["start", &name]);

    let mut connector = NamedPipeConnector { name: pipe };
    let mut client = None;
    wait_until("the service's pipe", Duration::from_secs(20), || {
        client =
            HostApiClient::connect(&mut connector, "service test", Duration::from_secs(5)).ok();
        client.is_some()
    });
    let mut client = client.unwrap();

    let update = client
        .update_program(Duration::from_secs(120))
        .expect("dsa update");
    assert_eq!(
        update.installing.as_deref(),
        Some("9.9.9"),
        "{}",
        update.detail
    );
    let downloaded = dir.join("updates").join("9.9.9").join("setup-9.9.9.exe");
    assert!(
        downloaded.exists(),
        "{} was downloaded",
        downloaded.display()
    );
    let status = client.status().unwrap();
    assert!(
        status
            .last_reason
            .as_deref()
            .is_some_and(|why| why.contains("being installed")),
        "{status:?}"
    );

    // The service waits a minute after setup ends before it counts the update as failed.
    let mut notice = String::new();
    wait_until(
        "the failure to be reported",
        Duration::from_secs(180),
        || {
            notice = client.status().unwrap().update_notice.unwrap_or_default();
            notice.contains("failed")
        },
    );
    eprintln!("the service told the user: {notice}");
    assert!(notice.contains("updating DSA to 9.9.9 failed"), "{notice}");
    assert!(notice.contains("exit code 1"), "{notice}");
    assert!(
        notice.contains(&format!(
            "DSA {} is still installed",
            dsa_host_windows::VERSION
        )),
        "{notice}"
    );
    let log = dir.join("logs").join("setup-9.9.9.log");
    assert!(notice.contains(&log.display().to_string()), "{notice}");
    // Android may start again.
    let status = client.status().unwrap();
    assert!(!status
        .last_reason
        .unwrap_or_default()
        .contains("being installed"));
}

#[test]
fn nfr5_service_keeps_running_when_update_store_has_bad_file() {
    use dsa_host_windows::host_config::HostConfig;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let id = std::process::id();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("bad-store-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let images = dir.join("images");
    std::fs::create_dir_all(&images).unwrap();
    // A corrupted state file that cannot be parsed as JSON.
    std::fs::write(images.join("state.json"), b"{ invalid json").unwrap();

    let pipe = format!("dsa-test-bad-store-{id}");
    let config = HostConfig::parse(
        &serde_json::json!({
            "kernel": dir.join("missing").join("bzImage"),
            "images_dir": images,
            "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
            "data_dir": dir.join("users"),
            "pipe_name": pipe,
            "pipe_sddl": "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)",
            "log_file": dir.join("dsa-hostd.log"),
            "connect_timeout_secs": 5,
            "stop_timeout_secs": 5,
        })
        .to_string(),
    )
    .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_serve = Arc::clone(&stop);
    let server =
        std::thread::spawn(move || dsa_host_windows::service::serve(config, "test", stop_serve));

    let mut connector = NamedPipeConnector { name: pipe };
    let mut client = None;
    wait_until("the pipe to appear", Duration::from_secs(10), || {
        client = HostApiClient::connect(&mut connector, "test", Duration::from_secs(1)).ok();
        client.is_some()
    });
    use dsa_host_core::host_api::ClientError;
    let mut client = client.unwrap();
    match client.status() {
        Ok(status) => assert_eq!(status.state, SessionState::Stopped),
        Err(ClientError::Service { .. }) => {}
        Err(other) => panic!("expected status or service error, got {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    let res = server.join().unwrap();
    assert!(res.is_ok());
}

#[test]
fn fr3_serve_registers_graphics_service_guids_at_startup() {
    use dsa_host_core::compositor::{GRAPHICS_COMMAND_PORT, GRAPHICS_PROCESS_PIPE_PORT};
    use dsa_host_windows::guest_communication_services::{
        is_service_guid_registered_under, read_service_element_name_under,
        register_graphics_service_guids_under, unregister_service_guid_under, RegistryHive,
        GRAPHICS_COMMAND_ELEMENT_NAME, GRAPHICS_PROCESS_PIPE_ELEMENT_NAME,
    };
    use dsa_host_windows::guid::Guid;
    use dsa_host_windows::host_config::HostConfig;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let id = std::process::id();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("serve-guid-reg-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let images = dir.join("images");
    std::fs::create_dir_all(&images).unwrap();

    let test_key = format!(r"Software\DesktopSubsystemForAndroid\TestGuestCommService_{id}");
    let command_guid = Guid::vsock_service(GRAPHICS_COMMAND_PORT);
    let pipe_guid = Guid::vsock_service(GRAPHICS_PROCESS_PIPE_PORT);

    let _ = unregister_service_guid_under(RegistryHive::CurrentUser, &test_key, command_guid);
    let _ = unregister_service_guid_under(RegistryHive::CurrentUser, &test_key, pipe_guid);

    let pipe = format!("dsa-test-serve-guid-{id}");
    let config = HostConfig::parse(
        &serde_json::json!({
            "kernel": dir.join("missing").join("bzImage"),
            "images_dir": images,
            "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
            "data_dir": dir.join("users"),
            "pipe_name": pipe,
            "pipe_sddl": "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)",
            "log_file": dir.join("dsa-hostd.log"),
            "connect_timeout_secs": 5,
            "stop_timeout_secs": 5,
        })
        .to_string(),
    )
    .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_serve = Arc::clone(&stop);
    let key_clone = test_key.clone();
    let server = std::thread::spawn(move || {
        dsa_host_windows::service::serve_with(config, "test", stop_serve, move || {
            register_graphics_service_guids_under(RegistryHive::CurrentUser, &key_clone)
        })
    });

    let mut connector = NamedPipeConnector { name: pipe };
    wait_until("the pipe to appear", Duration::from_secs(10), || {
        HostApiClient::connect(&mut connector, "test", Duration::from_secs(1)).is_ok()
    });

    assert!(is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &test_key,
        command_guid
    ));
    assert!(is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &test_key,
        pipe_guid
    ));
    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &test_key, command_guid)
            .as_deref(),
        Some(GRAPHICS_COMMAND_ELEMENT_NAME)
    );
    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &test_key, pipe_guid).as_deref(),
        Some(GRAPHICS_PROCESS_PIPE_ELEMENT_NAME)
    );

    stop.store(true, Ordering::Relaxed);
    let res = server.join().unwrap();
    assert!(res.is_ok());

    let _ = unregister_service_guid_under(RegistryHive::CurrentUser, &test_key, command_guid);
    let _ = unregister_service_guid_under(RegistryHive::CurrentUser, &test_key, pipe_guid);
}

#[test]
fn nfr5_serve_survives_graphics_service_guid_registration_error() {
    use dsa_host_windows::host_config::HostConfig;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let id = std::process::id();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("serve-guid-err-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let images = dir.join("images");
    std::fs::create_dir_all(&images).unwrap();

    let pipe = format!("dsa-test-serve-err-{id}");
    let config = HostConfig::parse(
        &serde_json::json!({
            "kernel": dir.join("missing").join("bzImage"),
            "images_dir": images,
            "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
            "data_dir": dir.join("users"),
            "pipe_name": pipe,
            "pipe_sddl": "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)",
            "log_file": dir.join("dsa-hostd.log"),
            "connect_timeout_secs": 5,
            "stop_timeout_secs": 5,
        })
        .to_string(),
    )
    .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_serve = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        dsa_host_windows::service::serve_with(config, "test", stop_serve, || {
            Err("simulated registration failure".to_string())
        })
    });

    let mut connector = NamedPipeConnector { name: pipe };
    let mut client = None;
    wait_until("the pipe to appear", Duration::from_secs(10), || {
        client = HostApiClient::connect(&mut connector, "test", Duration::from_secs(1)).ok();
        client.is_some()
    });
    let mut client = client.unwrap();
    let status = client.status();
    assert!(
        status.is_ok()
            || matches!(
                status,
                Err(dsa_host_core::host_api::ClientError::Service { .. })
            )
    );

    stop.store(true, Ordering::Relaxed);
    let res = server.join().unwrap();
    assert!(res.is_ok());
}

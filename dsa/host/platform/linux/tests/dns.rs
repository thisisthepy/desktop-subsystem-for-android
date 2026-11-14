//! The guest's DNS on the real image, end to end: the service boots the published image with
//! its network on, a name that only the host's resolver knows (an entry the check adds to the
//! runner's hosts file, which no DNS server has) resolves in Android to the same address, and a
//! name that does not exist does not resolve. The queries go from Android's resolver through
//! the control daemon's stub and the DNS port to the host's resolver.
//!
//! It needs crosvm, passt and the image (`DSA_CROSVM`, `DSA_WIDGET_TEST_IMAGE`), adb (`DSA_ADB`),
//! and `sudo` without a password to add the hosts file entry, as CI's runners have. Without
//! crosvm or the image it says it skipped.
#![cfg(target_os = "linux")]

mod boot_check;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boot_check::adb::Adb;
use boot_check::{lock, Process as Service, Watchdog};
use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_unix::socket::UnixLocalConnector;

/// The name only the runner's hosts file has, and its address (TEST-NET-2, never routed).
const HOST_ONLY: &str = "dsa-host-only.dns-check.test";
const HOST_ONLY_ADDRESS: &str = "198.51.100.7";
/// A name nothing resolves (RFC 6761 keeps `.invalid` from ever existing).
const NOWHERE: &str = "dsa-nowhere.invalid";

const BUDGET: Duration = Duration::from_secs(690);
const OVERRUN: Duration = Duration::from_secs(60);

fn scratch() -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("dns-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fails the test with what the service printed and the guest log's last lines.
fn fail(why: String, service: &Service, log: &Mutex<Vec<String>>) -> ! {
    let tail = {
        let log = lock(log);
        let _ = std::fs::write(service.log.with_file_name("guest.log"), log.join("\n"));
        log[log.len().saturating_sub(300)..].join("\n")
    };
    panic!(
        "{why}\n--- the service's output ---\n{}\n--- the guest log's last lines ---\n{tail}",
        service.log(),
    )
}

fn client(socket: &Path, wait: Duration) -> HostApiClient {
    let mut connector = UnixLocalConnector {
        path: socket.to_path_buf(),
    };
    let deadline = Instant::now() + wait;
    loop {
        match HostApiClient::connect(&mut connector, "dns-test", Duration::from_secs(30)) {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("the service did not answer on {}: {why}", socket.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// The hosts file entry, added for the check and removed when it ends.
struct HostsEntry;

impl HostsEntry {
    fn add() -> Result<HostsEntry, String> {
        let line = format!("{HOST_ONLY_ADDRESS} {HOST_ONLY}");
        let status = Command::new("sudo")
            .args([
                "-n",
                "sh",
                "-c",
                &format!("printf '%s\\n' '{line}' >> /etc/hosts"),
            ])
            .status()
            .map_err(|e| format!("sudo: {e}"))?;
        if !status.success() {
            return Err(format!(
                "sudo could not add {line} to /etc/hosts ({status})"
            ));
        }
        Ok(HostsEntry)
    }
}

impl Drop for HostsEntry {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args([
                "-n",
                "sed",
                "-i",
                &format!("/ {}$/d", HOST_ONLY.replace('.', "\\.")),
                "/etc/hosts",
            ])
            .status();
    }
}

/// What Android's resolver gives for `name`: the address `ping` names, or `None` when it says
/// the host is unknown. `ping` resolves through netd like any app does.
fn resolve_in_android(adb: &Adb, name: &str) -> Result<Option<String>, String> {
    let output = adb.shell(&format!("ping -c 1 -W 1 {name} 2>&1; true"))?;
    if output.contains("unknown host") {
        return Ok(None);
    }
    let first = output
        .lines()
        .find(|l| l.starts_with("PING "))
        .ok_or_else(|| {
            format!("ping said neither an address nor an unknown host for {name}: {output}")
        })?;
    let address = first
        .split('(')
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .map(str::to_string);
    Ok(address)
}

#[test]
fn fr28_android_resolves_names_through_the_host() {
    let (Some(crosvm), Some(image)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_WIDGET_TEST_IMAGE name no crosvm and image");
        return;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let adb_program = std::env::var_os("DSA_ADB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("adb"));
    let started = Instant::now();
    let deadline = started + BUDGET;
    let _watchdog = Watchdog::start("the DNS check", BUDGET + OVERRUN);
    let _entry = HostsEntry::add().expect("the hosts file entry only the host knows");
    let dir = scratch();
    let socket = dir.join("host.sock");
    let config = dir.join("dsa-hostd.json");
    let settings = serde_json::json!({
        "image_dir": image,
        "kernel": kernel,
        "crosvm": crosvm,
        "socket": socket,
        "machine_dir": dir.join("machine"),
        "network": true,
        "memory_mb": 4096,
        "processors": 2,
        "data_disk_size_gb": 8,
    });
    std::fs::write(&config, settings.to_string()).unwrap();
    let service = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-linux-hostd"))
            .arg("--config")
            .arg(&config),
        dir.join("hostd.log"),
    );
    let _compositor = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-compositor")).env("DSA_SOCKET", &socket),
        dir.join("compositor.log"),
    );
    let _devices = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-devices"))
            .arg("--socket")
            .arg(&socket),
        dir.join("devices.log"),
    );
    let mut api = client(&socket, Duration::from_secs(30));
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let reader = client(&socket, Duration::from_secs(10))
            .watch_log(&LogFilter::default(), false)
            .expect("reading the guest log");
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            let mut reader = reader;
            loop {
                match reader.next(Duration::from_secs(5)) {
                    Ok(Some(batch)) => {
                        let mut log = lock(&log);
                        for entry in batch.entries {
                            log.push(entry.threadtime());
                        }
                    }
                    Ok(None) => {}
                    Err(_) => return,
                }
            }
        });
    }
    let boot = deadline.saturating_duration_since(Instant::now()) - Duration::from_secs(180);
    if let Err(why) = api.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    }) {
        fail(
            format!("Android did not become ready: {why}"),
            &service,
            &log,
        );
    }

    // Developer mode, so adb reaches the guest through the service's loopback port.
    let mut wanted = match api.settings() {
        Ok(report) => report.settings,
        Err(why) => fail(format!("reading the settings: {why}"), &service, &log),
    };
    wanted.developer_mode = true;
    wanted.adb_auto_connect = false;
    let port = wanted.adb_port;
    if let Err(why) = api.set_settings(&wanted) {
        fail(format!("turning developer mode on: {why}"), &service, &log);
    }
    let adb = Adb {
        program: adb_program,
        serial: format!("127.0.0.1:{port}"),
        blank_screencaps: RefCell::default(),
    };
    if let Err(why) = adb.connect(Duration::from_secs(120)) {
        fail(
            format!("adb did not reach the guest: {why}"),
            &service,
            &log,
        );
    }

    // The redirect goes on once the daemon reaches the host's DNS port, and Android's network
    // comes up by DHCP from passt; both are waited for.
    let wait = Instant::now() + Duration::from_secs(120);
    let mut last;
    let resolved = loop {
        let switch = adb.shell("getprop vendor.dsa.dns").unwrap_or_default();
        match resolve_in_android(&adb, HOST_ONLY) {
            Ok(Some(address)) if switch.trim() == "on" => break address,
            Ok(other) => {
                last = format!("vendor.dsa.dns is {:?}, ping gave {other:?}", switch.trim())
            }
            Err(why) => last = why,
        }
        if Instant::now() >= wait {
            let redirect = adb
                .shell("dumpsys connectivity | grep -i dns | head -20")
                .unwrap_or_default();
            fail(
                format!("{HOST_ONLY}, which only the host's hosts file has, did not resolve in Android: {last}\n{redirect}"),
                &service,
                &log,
            );
        }
        std::thread::sleep(Duration::from_secs(3));
    };
    if resolved != HOST_ONLY_ADDRESS {
        fail(
            format!("{HOST_ONLY} resolved to {resolved} in Android, not to the host's {HOST_ONLY_ADDRESS}"),
            &service,
            &log,
        );
    }
    eprintln!("{HOST_ONLY} resolved in Android to {resolved}, as the host's resolver says");

    match resolve_in_android(&adb, NOWHERE) {
        Ok(None) => eprintln!("{NOWHERE} does not resolve in Android either"),
        Ok(Some(address)) => fail(
            format!("{NOWHERE}, which the host's resolver does not know, resolved in Android to {address}"),
            &service,
            &log,
        ),
        Err(why) => fail(why, &service, &log),
    }
    let service_log = service.log();
    if !service_log.contains("the guest's DNS queries are answered by this host's resolver") {
        fail(
            "the service never said it answered the guest's DNS queries".into(),
            &service,
            &log,
        );
    }
}

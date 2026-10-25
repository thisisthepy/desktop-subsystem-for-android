//! The `dsa-channeld` executable, as init runs it.
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

use dsa_protocol::auth::{bootconfig_entry, BootSecret};

fn bootconfig(name: &str, text: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn pr6_the_daemon_stops_when_the_boot_has_no_secret() {
    let path = bootconfig("no-secret", "androidboot.hardware = \"cutf_cvm\"\n");
    let output = Command::new(env!("CARGO_BIN_EXE_dsa-channeld"))
        .arg("--bootconfig")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dsa.control.secret"), "{stderr}");
}

#[test]
fn pr6_the_daemon_never_prints_the_secret() {
    let secret = BootSecret::from_bytes([0x3c; 32]);
    let path = bootconfig("secret", &bootconfig_entry(&secret));
    // Nothing listens on this port, so the daemon keeps retrying; stop it after a moment.
    let mut child = Command::new(env!("CARGO_BIN_EXE_dsa-channeld"))
        .args(["--cid", "1", "--port", "1"])
        .arg("--bootconfig")
        .arg(&path)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("connecting to the host"), "{stderr}");
    assert!(!stderr.contains(&secret.to_hex()), "{stderr}");
}

/// Runs the executable against a host on vsock loopback that knows the same secret.
#[cfg(feature = "vsock-loopback-tests")]
#[test]
fn pr6_the_daemon_reads_the_secret_and_is_accepted_over_vsock() {
    use std::time::Duration;

    use dsa_guest_transport::vsock::{VsockListener, VMADDR_CID_ANY};
    use dsa_protocol::control::Role;
    use dsa_protocol::handshake::{host_accept, local_hello};
    use dsa_transport::Listener;

    let secret = BootSecret::generate().unwrap();
    let path = bootconfig("vsock-secret", &bootconfig_entry(&secret));
    let port = 48_000 + std::process::id() % 1000;
    let mut listener = VsockListener::bind(VMADDR_CID_ANY, port).expect("binding vsock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_dsa-channeld"))
        .args(["--cid", "1", "--port", &port.to_string()])
        .arg("--bootconfig")
        .arg(&path)
        .spawn()
        .unwrap();
    let mut connection = listener
        .accept(Duration::from_secs(15))
        .expect("the daemon connects");
    let guest = host_accept(
        connection.as_mut(),
        &local_hello(Role::Host, "dsa-hostd test"),
        &secret,
        Duration::from_secs(10),
    );
    child.kill().unwrap();
    let _ = child.wait();
    assert!(guest.expect("accepted").build.starts_with("dsa-channeld "));
}

#[test]
fn pr6_each_message_reaches_the_kernel_log_as_one_record() {
    // The kernel log makes a record of every write. A datagram socket does the same, so
    // a line written in pieces shows up here as pieces.
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixDatagram;
    use std::time::Duration;

    let path = bootconfig("records", "androidboot.hardware = \"cutf_cvm\"\n");
    let (reader, writer) = UnixDatagram::pair().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_dsa-channeld"))
        .arg("--bootconfig")
        .arg(&path)
        .stderr(OwnedFd::from(writer))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    reader
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buffer = [0u8; 4096];
    let size = reader.recv(&mut buffer).expect("a message");
    let record = String::from_utf8_lossy(&buffer[..size]);
    assert!(record.starts_with("dsa-channeld: "), "{record:?}");
    assert!(record.ends_with('\n'), "{record:?}");
    assert!(record.contains("dsa.control.secret"), "{record:?}");
}

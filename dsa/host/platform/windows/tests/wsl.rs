//! The WSL launcher, against a stand-in for `wsl.exe` that keeps the container's state in a
//! file. The stand-in is a shell script, so these tests run where `sh` exists.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use dsa_host_core::session::GuestLauncher;
use dsa_host_windows::wsl::{decode, WslLauncher};

/// Writing an executable while another test thread forks can leave the fork holding the
/// file open for writing, and running it then fails with "Text file busy". These tests take
/// turns.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Makes a fake `wsl.exe` in its own directory. `stop_works` decides whether lxc-stop
/// actually ends the container.
fn fake_wsl(name: &str, stop_works: bool, start_fails: bool) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fake-wsl-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let state = dir.join("running");
    let log = dir.join("log");
    let script = dir.join("wsl.exe");
    let stop_action = if stop_works {
        format!("rm -f '{}'", state.display())
    } else {
        ":".into()
    };
    let start_action = if start_fails {
        "echo 'lxc-start: dsa: no configuration file' >&2; exit 1".to_string()
    } else {
        format!("touch '{}'", state.display())
    };
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
[ "$1 $2 $3 $4 $5" = "-d dsa-distro -u root --exec" ] || {{ echo "bad prefix: $*" >&2; exit 2; }}
shift 5
case "$1" in
  lxc-start) {start_action} ;;
  lxc-stop) {stop_action} ;;
  sh) test -f '{state}' ;;
  *) echo "unexpected command $1" >&2; exit 2 ;;
esac
"#,
            log = log.display(),
            state = state.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, dir)
}

fn launcher(script: PathBuf) -> WslLauncher {
    let mut launcher = WslLauncher::lxc("dsa-distro", "dsa").unwrap();
    launcher.wsl = script;
    launcher.poll_interval = Duration::from_millis(10);
    launcher
}

#[test]
fn fr2_the_launcher_starts_and_stops_the_container_through_wsl() {
    let _turn = serial();
    let (script, dir) = fake_wsl("start-stop", true, false);
    let mut launcher = launcher(script);
    launcher.start().expect("start");
    assert!(dir.join("running").exists());
    launcher.stop(Duration::from_secs(5)).expect("stop");
    assert!(!dir.join("running").exists());

    let log = fs::read_to_string(dir.join("log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], "-d dsa-distro -u root --exec lxc-start -n dsa");
    assert!(lines.contains(&"-d dsa-distro -u root --exec lxc-stop -n dsa -t 5"));
}

#[test]
fn fr2_stop_fails_while_container_processes_remain() {
    let _turn = serial();
    let (script, _dir) = fake_wsl("stuck", false, false);
    let mut launcher = launcher(script);
    launcher.start().expect("start");
    let error = launcher.stop(Duration::from_millis(200)).unwrap_err();
    assert!(error.contains("still has running processes"), "{error}");
}

#[test]
fn fr2_a_failed_start_reports_what_the_container_tool_said() {
    let _turn = serial();
    let (script, _dir) = fake_wsl("bad-start", true, true);
    let mut launcher = launcher(script);
    let error = launcher.start().unwrap_err();
    assert!(error.contains("no configuration file"), "{error}");
}

#[test]
fn fr2_stopping_a_container_that_is_not_running_succeeds_at_once() {
    let _turn = serial();
    let (script, dir) = fake_wsl("not-running", true, false);
    let mut launcher = launcher(script);
    launcher.stop(Duration::from_secs(5)).expect("stop");
    let log = fs::read_to_string(dir.join("log")).unwrap();
    assert!(
        !log.contains("lxc-stop"),
        "nothing to stop, so lxc-stop is not run: {log}"
    );
}

#[test]
fn fr2_a_missing_wsl_is_reported() {
    let _turn = serial();
    let mut launcher = launcher(PathBuf::from("/nonexistent/wsl.exe"));
    let error = launcher.start().unwrap_err();
    assert!(error.contains("could not run"), "{error}");
}

#[test]
fn fr2_container_names_that_would_need_quoting_are_refused() {
    assert!(WslLauncher::lxc("d", "dsa; rm -rf /").is_err());
    assert!(WslLauncher::lxc("d", "").is_err());
}

#[test]
fn wsl_messages_in_utf16_are_decoded() {
    let utf16: Vec<u8> = "There is no distribution with the supplied name.\r\n"
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    assert_eq!(
        decode(&utf16).trim(),
        "There is no distribution with the supplied name."
    );
    assert_eq!(decode("lxc-start: 오류".as_bytes()), "lxc-start: 오류");
}

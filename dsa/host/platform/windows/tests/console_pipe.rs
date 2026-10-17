//! The pipe the Android VM's serial console arrives on, served and read as the service does
//! before a VM starts. A client here stands in for HCS, which opens the pipe when the VM
//! starts.
#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dsa_host_windows::console_log::ConsoleLog;
use dsa_host_windows::console_pipe::{ConsoleCapture, CONSOLE_PIPE_SDDL};
use dsa_host_windows::pipe::current_user_sid;

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("console-pipe")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn pipe_name(test: &str) -> String {
    format!(r"\\.\pipe\dsa-console-test-{test}-{}", std::process::id())
}

/// The console pipe's own rules plus the user running the test, who is not always an
/// administrator.
fn test_sddl() -> String {
    format!(
        "{CONSOLE_PIPE_SDDL}(A;;GRGW;;;{})",
        current_user_sid().unwrap()
    )
}

#[test]
fn fr2_what_the_vm_writes_to_its_console_pipe_is_kept_in_the_log() {
    let dir = scratch("logged");
    let path = dir.join("android-test.log");
    let pipe = pipe_name("logged");
    let mut capture = ConsoleCapture::start_with_sddl(
        &pipe,
        &test_sddl(),
        ConsoleLog::begin_boot(&path, "==== boot\n"),
    )
    .map_err(|failed| failed.0)
    .expect("the pipe is served");

    // Nobody else can serve the same pipe meanwhile.
    assert!(
        ConsoleCapture::start(&pipe, ConsoleLog::begin_boot(&dir.join("other.log"), "")).is_err()
    );

    {
        let mut vm = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pipe)
            .expect("the VM's side opens the pipe");
        vm.write_all(b"[    0.000000] Linux version 6.18.54.1-dsa\r\n")
            .unwrap();
        vm.write_all(b"[    1.500000] init: first stage mount failed\r\n")
            .unwrap();
        vm.write_all(b"[    1.600000] Kernel panic - not syncing")
            .unwrap();
        // Dropping it closes the pipe, as the VM's exit does.
    }
    assert!(
        capture.finish(Duration::from_secs(10)),
        "the reader ends once the pipe closes"
    );
    assert_eq!(
        capture.tail(),
        [
            "[    0.000000] Linux version 6.18.54.1-dsa",
            "[    1.500000] init: first stage mount failed",
            "[    1.600000] Kernel panic - not syncing",
        ]
    );
    let report = capture.failure_report();
    assert!(report.contains("Kernel panic - not syncing"), "{report}");
    assert!(report.contains(&path.display().to_string()), "{report}");
    drop(capture);

    let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    assert_eq!(
        text,
        "==== boot\n[    0.000000] Linux version 6.18.54.1-dsa\r\n\
         [    1.500000] init: first stage mount failed\r\n\
         [    1.600000] Kernel panic - not syncing"
    );

    // Once the capture has ended the pipe's name is free for the next boot.
    let next = ConsoleCapture::start_with_sddl(
        &pipe,
        &test_sddl(),
        ConsoleLog::begin_boot(&path, "==== boot 2\n"),
    );
    assert!(next.is_ok(), "the next boot can serve the pipe again");
}

#[test]
fn fr2_a_console_the_vm_never_opens_ends_promptly_and_says_so() {
    let dir = scratch("never-opened");
    let path = dir.join("android-test.log");
    let mut capture = ConsoleCapture::start(
        &pipe_name("never-opened"),
        ConsoleLog::begin_boot(&path, "==== boot\n"),
    )
    .map_err(|failed| failed.0)
    .expect("the pipe is served with the production security descriptor");
    let asked = Instant::now();
    assert!(capture.finish(Duration::ZERO));
    assert!(
        asked.elapsed() < Duration::from_secs(3),
        "finishing never hangs"
    );
    assert!(capture.ended());
    assert!(capture.failure_report().contains("printed nothing"));
    drop(capture);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("never opened its console"), "{text}");
}

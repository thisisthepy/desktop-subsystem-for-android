//! `dsa` against a running service, over the platform's real local transport.

use std::process::{Command, Output};
use std::sync::Arc;
use std::time::Duration;

use dsa_cli::{parse, Command as Cmd};
use dsa_host_core::host_api::UserAndroid;
use dsa_host_core::host_api_conformance::{FakeAndroids, LocalPair, RunningService};
use dsa_host_core::session::SessionState;

fn args(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn pr5_the_command_line_is_parsed() {
    let invocation = parse(&args("start --wait --timeout 5 --socket /tmp/x.sock")).unwrap();
    assert_eq!(invocation.command, Cmd::Start);
    assert!(invocation.wait);
    assert_eq!(invocation.timeout, Duration::from_secs(5));
    assert_eq!(invocation.endpoint.as_deref(), Some("/tmp/x.sock"));
    assert_eq!(parse(&args("status")).unwrap().command, Cmd::Status);
    assert!(parse(&args("")).unwrap_err().contains("usage"));
    assert!(
        parse(&args("start --user bob")).is_err(),
        "no option names another user"
    );
    assert!(parse(&args("stop --timeout soon")).is_err());
}

#[cfg(unix)]
fn local_pair() -> (LocalPair, String) {
    use dsa_host_unix::socket::{current_uid, UnixLocalConnector, UnixLocalListener};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    // Socket paths are limited to about 100 bytes, so the path is relative to the working
    // directory, which the `dsa` process inherits.
    fn relative_to_cwd(target: &Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let target = target.canonicalize().unwrap();
        let common = cwd
            .components()
            .zip(target.components())
            .take_while(|(a, b)| a == b)
            .count();
        let mut relative: PathBuf = cwd.components().skip(common).map(|_| "..").collect();
        relative.extend(target.components().skip(common));
        relative
    }
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli");
    std::fs::create_dir_all(&dir).unwrap();
    let path = relative_to_cwd(&dir).join(format!(
        "{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixLocalListener::bind(&path, 0o600).unwrap();
    let endpoint = path.to_string_lossy().into_owned();
    (
        (
            Box::new(listener),
            Box::new(UnixLocalConnector { path }),
            format!("uid:{}", current_uid()),
        ),
        endpoint,
    )
}

#[cfg(windows)]
fn local_pair() -> (LocalPair, String) {
    use dsa_host_windows::pipe::{current_user_sid, NamedPipeConnector, NamedPipeListener};
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "dsa-cli-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    // The test may run as a non-interactive account, which the default would not admit.
    let sddl = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)";
    let listener = NamedPipeListener::bind(&name, sddl).unwrap();
    (
        (
            Box::new(listener),
            Box::new(NamedPipeConnector { name: name.clone() }),
            current_user_sid().unwrap(),
        ),
        name,
    )
}

fn endpoint_flag() -> &'static str {
    if cfg!(windows) {
        "--pipe"
    } else {
        "--socket"
    }
}

fn dsa(endpoint: &str, line: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(args(line))
        .args([endpoint_flag(), endpoint])
        .output()
        .expect("running dsa")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

#[test]
fn pr5_dsa_starts_and_stops_the_android_of_the_user_running_it() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let status = dsa(&endpoint, "status");
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(
        stdout(&status),
        "Android: Stopped (last stopped: stopped on request)"
    );

    let started = dsa(&endpoint, "start --wait --timeout 10");
    assert!(started.status.success(), "{}", stderr(&started));
    assert_eq!(stdout(&started), "Android: Active");
    assert_eq!(androids.of(&user).unwrap().state(), SessionState::Active);

    let stopped = dsa(&endpoint, "stop --wait --timeout 10");
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(androids.of(&user).unwrap().state(), SessionState::Stopped);
}

#[test]
fn pr5_dsa_reports_why_the_service_refused() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids {
        refused: vec![user.clone()],
        ..FakeAndroids::default()
    });
    let _service = RunningService::start(listener, androids);
    let output = dsa(&endpoint, "start");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains(&user), "{}", stderr(&output));
}

#[test]
fn pr5_dsa_says_when_the_service_is_not_running() {
    let ((listener, _, _), endpoint) = local_pair();
    drop(listener);
    let output = dsa(&endpoint, "status");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot reach the DSA service"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn pr5_a_wrong_command_line_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_dsa"))
        .arg("launch")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("usage"));
}

#[test]
fn fr14_dsa_shows_news_about_an_update_on_its_own_line() {
    use dsa_host_core::host_api::Status;
    let line = dsa_cli::describe(&Status {
        state: SessionState::Active,
        last_reason: None,
        update_notice: Some("Android was updated to 16.0.3".into()),
    });
    assert_eq!(
        line,
        "Android: Active\nUpdate: Android was updated to 16.0.3"
    );
}

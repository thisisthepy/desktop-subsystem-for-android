//! `dsa` against a running service, over the platform's real local transport.

use std::process::{Command, Output};
use std::sync::Arc;
use std::time::Duration;

use dsa_cli::{parse, Command as Cmd};
use dsa_host_core::host_api::{ProgramUpdate, UserAndroid};
use dsa_host_core::host_api_conformance::{
    FakeAndroids, FakeProgramUpdates, LocalPair, RunningService,
};
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

#[test]
fn fr15_dsa_update_is_parsed() {
    let invocation = parse(&args("update --wait --timeout 300")).unwrap();
    assert_eq!(invocation.command, Cmd::Update);
    assert!(invocation.wait);
    assert_eq!(invocation.timeout, Duration::from_secs(300));
    assert!(dsa_cli::USAGE.contains("update"));
}

#[test]
fn fr15_dsa_update_asks_the_service_to_install_now_and_prints_its_answer() {
    let ((listener, _, _), endpoint) = local_pair();
    let program = Arc::new(FakeProgramUpdates::answering(Ok(ProgramUpdate {
        installing: Some("0.1.1".into()),
        detail: "installing DSA 0.1.1; the service restarts when setup is done".into(),
    })));
    let _service = RunningService::start_updating(
        listener,
        Arc::new(FakeAndroids::default()),
        program.clone(),
    );
    let output = dsa(&endpoint, "update");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "installing DSA 0.1.1; the service restarts when setup is done"
    );
    assert_eq!(*program.requests.lock().unwrap(), 1);
}

#[test]
fn fr15_dsa_update_reports_why_dsa_was_not_updated() {
    let ((listener, _, _), endpoint) = local_pair();
    let program = Arc::new(FakeProgramUpdates::answering(Err(
        "downloading the new version of DSA failed: setup-0.1.1.exe has SHA-256 00".into(),
    )));
    let _service =
        RunningService::start_updating(listener, Arc::new(FakeAndroids::default()), program);
    let output = dsa(&endpoint, "update");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("SHA-256"), "{}", stderr(&output));
}

/// A program updater whose answer is followed by the service restarting, as setup does.
struct Restarting(std::sync::mpsc::Sender<()>);

impl dsa_host_core::host_api::UpdateProgram for Restarting {
    fn update_now(&self) -> Result<ProgramUpdate, String> {
        let _ = self.0.send(());
        Ok(ProgramUpdate {
            installing: Some("0.1.1".into()),
            detail: "installing DSA 0.1.1".into(),
        })
    }
}

/// A notice the restarted service gives.
struct Updated;

impl UserAndroid for Updated {
    fn state(&self) -> SessionState {
        SessionState::Stopped
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {}
    fn stop(&self) {}
    fn update_notice(&self) -> Option<String> {
        Some("DSA was updated to 0.1.1".into())
    }
}

#[test]
fn fr15_dsa_update_waits_for_the_service_to_restart_and_shows_how_it_went() {
    use dsa_host_core::host_api::PerUser;
    let ((listener, _, _), endpoint) = local_pair();
    let (asked, was_asked) = std::sync::mpsc::channel();
    let old = RunningService::start_updating(
        listener,
        Arc::new(FakeAndroids::default()),
        Arc::new(Restarting(asked)),
    );
    let child = Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(args("update --wait --timeout 30"))
        .args([endpoint_flag(), &endpoint])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    was_asked.recv_timeout(Duration::from_secs(20)).unwrap();
    // Setup stops the old service and starts the new one a moment later.
    std::thread::sleep(Duration::from_millis(300));
    drop(old);
    std::thread::sleep(Duration::from_millis(1000));
    let new = RunningService::start(
        listener_at(&endpoint),
        Arc::new(PerUser::new(|_: &str| {
            Ok(Arc::new(Updated) as Arc<dyn UserAndroid>)
        })),
    );
    let output = child.wait_with_output().unwrap();
    drop(new);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "installing DSA 0.1.1\nAndroid: Stopped\nUpdate: DSA was updated to 0.1.1"
    );
}

#[cfg(unix)]
fn listener_at(endpoint: &str) -> Box<dyn dsa_transport::local::LocalListener> {
    Box::new(
        dsa_host_unix::socket::UnixLocalListener::bind(std::path::Path::new(endpoint), 0o600)
            .unwrap(),
    )
}

#[cfg(windows)]
fn listener_at(endpoint: &str) -> Box<dyn dsa_transport::local::LocalListener> {
    let sddl = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;AU)";
    Box::new(dsa_host_windows::pipe::NamedPipeListener::bind(endpoint, sddl).unwrap())
}

#[test]
fn fr15_dsa_shows_news_about_the_image_and_the_program_on_their_own_lines() {
    use dsa_host_core::host_api::Status;
    let line = dsa_cli::describe(&Status {
        state: SessionState::Stopped,
        last_reason: None,
        update_notice: Some(
            "Android was updated to 16.0.3\nDSA 0.1.1 is downloaded and installs when Android \
             is not running"
                .into(),
        ),
    });
    assert_eq!(
        line,
        "Android: Stopped\nUpdate: Android was updated to 16.0.3\nUpdate: DSA 0.1.1 is \
         downloaded and installs when Android is not running"
    );
}

#[test]
fn fr15_dsa_update_says_when_the_service_did_not_restart_and_what_it_says_now() {
    let ((listener, _, _), endpoint) = local_pair();
    let program = Arc::new(FakeProgramUpdates::answering(Ok(ProgramUpdate {
        installing: Some("0.1.1".into()),
        detail: "installing DSA 0.1.1".into(),
    })));
    let _service =
        RunningService::start_updating(listener, Arc::new(FakeAndroids::default()), program);
    let output = dsa(&endpoint, "update --wait --timeout 1");
    assert_eq!(output.status.code(), Some(1));
    let said = stderr(&output);
    assert!(said.contains("has not restarted after 1 seconds"), "{said}");
    assert!(said.contains("Android: Stopped"), "{said}");
}

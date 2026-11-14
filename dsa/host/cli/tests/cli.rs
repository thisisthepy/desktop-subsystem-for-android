//! `dsa` against a running service, over the platform's real local transport.

use dsa_host_core::ime::ImeEvent;
use dsa_host_core::input::PointerEvent;
use dsa_host_core::keys::KeyEvent;
use dsa_host_core::tasks::TaskRequestError;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
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

#[test]
fn fr35_adb_shell_asks_for_root_like_an_app() {
    use dsa_cli::AppsAction;
    let invocation = parse(&args("apps root adb ask")).unwrap();
    assert_eq!(
        invocation.apps,
        Some(AppsAction::Root("com.android.shell".into(), "ask".into()))
    );
}

#[test]
fn fr21_dsa_logcat_takes_buffers_tag_filters_and_a_dump() {
    use dsa_host_core::guest_log::{LogBuffer, LogFilter, DEFAULT_BUFFERS, INFO, SILENT};
    let invocation = parse(&args(
        "logcat -d -b main,crash ActivityManager:I *:S --json",
    ))
    .unwrap();
    assert_eq!(invocation.command, Cmd::Logcat);
    assert!(invocation.dump);
    assert!(invocation.json);
    assert_eq!(
        invocation.log_filter,
        Some(LogFilter {
            buffers: LogBuffer::Main.bit() | LogBuffer::Crash.bit(),
            tags: vec![("ActivityManager".into(), INFO), ("*".into(), SILENT)],
        })
    );
    let plain = parse(&args("logcat")).unwrap();
    assert!(!plain.dump);
    assert_eq!(plain.log_filter.unwrap().buffers, DEFAULT_BUFFERS);
    assert!(parse(&args("logcat -b nope")).is_err());
    assert!(parse(&args("logcat Tag:Q")).is_err());

    let collect = parse(&args("logs collect -o diag.zip")).unwrap();
    assert_eq!(collect.command, Cmd::LogsCollect);
    assert_eq!(
        collect.output.as_deref(),
        Some(std::path::Path::new("diag.zip"))
    );
    assert!(parse(&args("logs collect")).is_err());
    assert!(parse(&args("logs")).is_err());
}

#[test]
fn fr21_dsa_logcat_prints_threadtime_or_one_json_object_a_line() {
    use dsa_host_core::guest_log::{LogBuffer, LogEntry, WARN};
    let entry = LogEntry {
        buffer: LogBuffer::System,
        priority: WARN,
        time_ns: 1_700_000_000_000_000_000,
        pid: 1,
        tid: 2,
        uid: 1000,
        tag: "Tag".into(),
        message: "hello".into(),
        dropped: 0,
    };
    let mut text = Vec::new();
    assert!(dsa_cli::logs::write_entries(
        &mut text,
        &[entry.clone(), LogEntry::dropped(3)],
        false
    ));
    assert_eq!(
        String::from_utf8(text).unwrap(),
        "2023-11-14 22:13:20.000     1     2 W Tag: hello\n--------- 3 records dropped\n"
    );
    let mut json = Vec::new();
    assert!(dsa_cli::logs::write_entries(&mut json, &[entry], true));
    let value: serde_json::Value =
        serde_json::from_slice(json.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(value["priority"], "W");
    assert_eq!(value["buffer"], "system");
    assert_eq!(value["tag"], "Tag");
    assert_eq!(value["message"], "hello");
    assert_eq!(value["time"], "2023-11-14 22:13:20.000");
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
        problem: None,
        program_update: None,
    });
    assert_eq!(
        line,
        "Android: Active\nUpdate: Android was updated to 16.0.3"
    );
}

#[test]
fn fr2_dsa_shows_the_guest_console_of_a_failed_start_below_the_status_line() {
    use dsa_host_core::host_api::Status;
    let line = dsa_cli::describe(&Status {
        state: SessionState::Stopped,
        last_reason: Some(
            "the Android VM exited during boot\nlast console lines:\n  Kernel panic".into(),
        ),
        update_notice: None,
        problem: None,
        program_update: None,
    });
    assert_eq!(
        line,
        "Android: Stopped (last stopped: the Android VM exited during boot)\n\
         last console lines:\n  Kernel panic"
    );
}

#[test]
fn fr17_dsa_status_says_an_error_code_in_a_sentence_and_keeps_the_raw_line() {
    use dsa_host_core::host_api::Status;
    let raw =
        "the guest did not start: HcsStartComputeSystem failed (HRESULT 0x80070057): {\"Error\":1}";
    let line = dsa_cli::describe(&Status {
        state: SessionState::Stopped,
        last_reason: Some(raw.into()),
        update_notice: None,
        problem: None,
        program_update: None,
    });
    assert_eq!(
        line,
        format!(
            "Android: Stopped (last stopped: the host refused the settings for Android's virtual \
             machine; update or reinstall DSA)\ndetails: {raw}"
        )
    );
}

#[test]
fn fr17_dsa_status_gives_each_mapped_code_its_sentence() {
    use dsa_host_core::host_api::Status;
    for (code, sentence) in [
        ("0x80370102", "virtualization is off"),
        ("0xC0351000", "virtualization is off"),
        ("0x8007000e", "not enough memory"),
        ("0x80070070", "a disk problem"),
        ("0x80004005", "Android could not start"),
    ] {
        let line = dsa_cli::describe(&Status {
            state: SessionState::Stopped,
            last_reason: Some(format!("HcsStartComputeSystem failed (HRESULT {code})")),
            update_notice: None,
            problem: None,
            program_update: None,
        });
        let first = line.lines().next().unwrap();
        assert!(first.contains(sentence), "{code}: {line}");
        assert!(!first.contains(code), "{code}: {line}");
    }
}

#[test]
fn fr2_dsa_says_what_keeps_a_stalled_android_from_being_ready() {
    use dsa_host_core::host_api::Status;
    let status = Status {
        state: SessionState::Stalled,
        last_reason: None,
        update_notice: None,
        problem: Some("Android has not finished booting after 300 s\nthe log is there".into()),
        program_update: None,
    };
    assert_eq!(
        dsa_cli::describe(&status),
        "Android: Stalled\nProblem: Android has not finished booting after 300 s\n\
         Problem: the log is there"
    );
    let json = dsa_cli::status_json(&status);
    assert_eq!(json["state"], "Stalled");
    assert!(json["problem"]
        .as_str()
        .unwrap()
        .contains("has not finished booting"));
}

#[test]
fn fr2_dsa_start_waits_longer_than_android_may_take_to_be_called_stalled() {
    let start = parse(&args("start --wait")).unwrap();
    assert!(start.timeout > dsa_host_core::session::DEFAULT_READY_TIMEOUT);
    let stop = parse(&args("stop --wait")).unwrap();
    assert_eq!(stop.timeout, Duration::from_secs(120));
    let told = parse(&args("start --wait --timeout 5")).unwrap();
    assert_eq!(told.timeout, Duration::from_secs(5));
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
fn fr15_dsa_update_json_says_which_version_setup_installs() {
    let ((listener, _, _), endpoint) = local_pair();
    let program = Arc::new(FakeProgramUpdates::answering(Ok(ProgramUpdate {
        installing: Some("0.1.1".into()),
        detail: "installing DSA 0.1.1".into(),
    })));
    let _service =
        RunningService::start_updating(listener, Arc::new(FakeAndroids::default()), program);
    let answer = json_of(&dsa(&endpoint, "update --json"));
    assert_eq!(answer["installing"], "0.1.1");
    assert_eq!(answer["detail"], "installing DSA 0.1.1");
    assert!(answer["status"].is_null());

    let ((listener, _, _), endpoint) = local_pair();
    let program = Arc::new(FakeProgramUpdates::answering(Ok(ProgramUpdate {
        installing: None,
        detail: "DSA 0.1.1 is up to date".into(),
    })));
    let _service =
        RunningService::start_updating(listener, Arc::new(FakeAndroids::default()), program);
    let answer = json_of(&dsa(&endpoint, "update --json"));
    assert!(answer["installing"].is_null(), "nothing newer: {answer}");
    assert_eq!(answer["detail"], "DSA 0.1.1 is up to date");
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
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
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
        problem: None,
        program_update: None,
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

struct FakeStarter<F> {
    start: F,
}

impl<F> dsa_cli::ServiceStarter for FakeStarter<F>
where
    F: Fn() -> Result<(), String> + Send + Sync,
{
    fn start_service(&self) -> Result<(), String> {
        (self.start)()
    }
}

#[test]
fn fr2_dsa_start_starts_a_stopped_service_first() {
    let ((listener, mut connector, _user), _endpoint) = local_pair();
    let listener_slot = Arc::new(std::sync::Mutex::new(Some(listener)));
    let androids = Arc::new(FakeAndroids::default());
    let service_slot = Arc::new(std::sync::Mutex::new(None));
    let service_ref = Arc::clone(&service_slot);
    let fake_starter = FakeStarter {
        start: move || {
            let l = listener_slot.lock().unwrap().take().unwrap();
            *service_ref.lock().unwrap() = Some(RunningService::start(l, androids.clone()));
            Ok(())
        },
    };
    let invocation = parse(&args("start")).unwrap();
    let result = dsa_cli::run_with_starter(&invocation, &mut *connector, Some(&fake_starter));
    assert!(result.is_ok(), "result: {result:?}");
    let line = result.unwrap();
    assert_eq!(line, "Android: Active");
}

#[test]
fn fr2_dsa_start_reports_why_when_it_cannot_start_the_service() {
    let ((listener, mut connector, _), _) = local_pair();
    drop(listener);
    let fake_starter = FakeStarter {
        start: || Err("access denied (system error 5)".into()),
    };
    let invocation = parse(&args("start")).unwrap();
    let result = dsa_cli::run_with_starter(&invocation, &mut *connector, Some(&fake_starter));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("starting the service failed: access denied (system error 5)"),
        "{err}"
    );
}

#[test]
fn fr2_dsa_status_starts_a_stopped_service_when_pipe_is_missing() {
    let ((listener, mut connector, _user), _endpoint) = local_pair();
    let listener_slot = Arc::new(std::sync::Mutex::new(Some(listener)));
    let androids = Arc::new(FakeAndroids::default());
    let service_slot = Arc::new(std::sync::Mutex::new(None));
    let service_ref = Arc::clone(&service_slot);
    let fake_starter = FakeStarter {
        start: move || {
            let l = listener_slot.lock().unwrap().take().unwrap();
            *service_ref.lock().unwrap() = Some(RunningService::start(l, androids.clone()));
            Ok(())
        },
    };
    let invocation = parse(&args("status")).unwrap();
    let result = dsa_cli::run_with_starter(&invocation, &mut *connector, Some(&fake_starter));
    assert!(result.is_ok(), "result: {result:?}");
    assert_eq!(
        result.unwrap(),
        "Android: Stopped (last stopped: stopped on request)"
    );
}

#[test]
fn fr2_dsa_stop_does_not_start_stopped_service() {
    let ((listener, mut connector, _), _) = local_pair();
    drop(listener);
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started_ref = Arc::clone(&started);
    let fake_starter = FakeStarter {
        start: move || {
            started_ref.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        },
    };
    let invocation = parse(&args("stop")).unwrap();
    let result = dsa_cli::run_with_starter(&invocation, &mut *connector, Some(&fake_starter));
    assert!(result.is_err());
    assert!(!started.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn fr10_dsa_freeze_freezes_a_running_android_and_says_when_none_runs() {
    assert_eq!(parse(&args("freeze")).unwrap().command, Cmd::Freeze);
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let refused = dsa(&endpoint, "freeze");
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("not running"),
        "{}",
        stderr(&refused)
    );

    assert!(dsa(&endpoint, "start").status.success());
    let frozen = dsa(&endpoint, "freeze");
    assert!(frozen.status.success(), "{}", stderr(&frozen));
    assert_eq!(stdout(&frozen), "Android: Frozen");
    assert_eq!(androids.of(&user).unwrap().state(), SessionState::Frozen);
}

#[test]
fn fr3_dsa_launch_and_dsa_tasks_are_parsed() {
    let launch = parse(&args("launch com.android.settings --pipe p")).unwrap();
    assert_eq!(launch.command, Cmd::Launch);
    assert_eq!(launch.package.as_deref(), Some("com.android.settings"));
    assert_eq!(launch.endpoint.as_deref(), Some("p"));
    assert_eq!(parse(&args("tasks")).unwrap().command, Cmd::Tasks);
    let missing = parse(&args("launch")).unwrap_err();
    assert!(missing.contains("package"), "{missing}");
    assert!(missing.contains("usage"), "{missing}");
    let odd = parse(&args("launch ../evil")).unwrap_err();
    assert!(odd.contains("not a package name"), "{odd}");
    assert!(parse(&args("launch a.b c.d")).is_err(), "one app at a time");
}

#[test]
fn nfr4_dsa_launch_can_wait_for_the_first_frame() {
    let launch = parse(&args(
        "launch com.android.settings --wait-first-frame --json",
    ))
    .unwrap();
    assert_eq!(launch.command, Cmd::Launch);
    assert!(launch.wait_first_frame);
    assert!(launch.json);
    assert!(
        !parse(&args("launch com.android.settings"))
            .unwrap()
            .wait_first_frame
    );
    assert!(
        parse(&args("status --wait-first-frame")).is_err(),
        "only a launch has a first frame"
    );
}

#[test]
fn nfr4_the_first_frame_times_count_from_the_launch_request() {
    use dsa_cli::{describe_first_frame, first_frame_json, FirstFrame};
    let ms = Duration::from_millis;
    let start = ms(10_000);
    let frame = FirstFrame::since(
        7,
        start,
        start + ms(12),
        start + Duration::from_micros(845_260),
        Some(start + ms(861)),
    );
    assert_eq!(frame.request, ms(12));
    assert_eq!(
        describe_first_frame("com.android.settings", &frame),
        "com.android.settings (task 7): first frame presented after 845.3 ms, shown after \
         861.0 ms; the launch request was answered after 12.0 ms"
    );
    let json = first_frame_json("com.android.settings", &frame);
    assert_eq!(json["task_id"], 7);
    assert_eq!(json["first_frame_ms"], 845.3);
    assert_eq!(json["first_shown_ms"], 861.0);
    assert_eq!(json["request_ms"], 12.0);

    let unshown = FirstFrame::since(7, start, start, start + ms(5), None);
    assert!(describe_first_frame("a.b", &unshown).contains("not known to be shown"));
    assert!(first_frame_json("a.b", &unshown)["first_shown_ms"].is_null());
    // A clock read before the start never makes a negative time.
    assert_eq!(
        FirstFrame::since(1, start, start, start - ms(1), None).presented,
        Duration::ZERO
    );
}

#[test]
fn fr5_dsa_tap_names_the_app_and_the_point() {
    let tap = parse(&args("tap com.android.settings 200 300 --json")).unwrap();
    assert_eq!(tap.command, Cmd::Tap);
    assert_eq!(tap.package.as_deref(), Some("com.android.settings"));
    assert_eq!(tap.point, Some((200, 300)));
    assert!(tap.json);
    for wrong in [
        "tap",
        "tap com.android.settings",
        "tap com.android.settings 200",
        "tap com.android.settings 200 x",
        "tap com.android.settings 200 -3",
        "tap com.android.settings 1 2 3",
    ] {
        assert!(parse(&args(wrong)).is_err(), "{wrong}");
    }
}

#[test]
fn fr5_dsa_tap_presses_and_releases_in_the_apps_task_through_the_local_api() {
    use dsa_host_core::host_api_conformance::sample_task;
    use dsa_host_core::input::{buttons, PointerAction};
    use dsa_host_core::tasks::TaskEvent;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    assert!(dsa(&endpoint, "start").status.success());
    let android = androids.of(&user).unwrap();
    android.publish(TaskEvent::Updated(sample_task(3, "Old")));
    let mut focused = sample_task(5, "Focused");
    focused.focused = true;
    android.publish(TaskEvent::Updated(focused));
    android.publish(TaskEvent::Updated(sample_task(9, "Newest")));

    let output = dsa(&endpoint, "tap com.example.app 200 300");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("Tapped task 5 of com.example.app at (200, 300)"),
        "{}",
        stdout(&output)
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while android.pointers.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pointers = android.pointers.lock().unwrap().clone();
    assert_eq!(pointers.len(), 2, "{pointers:?}");
    assert_eq!(pointers[0].task_id, 5);
    assert_eq!(pointers[0].action, PointerAction::Down);
    assert_eq!(pointers[0].buttons, buttons::PRIMARY);
    assert_eq!((pointers[0].x, pointers[0].y), (200.0, 300.0));
    assert_eq!(pointers[1].action, PointerAction::Up);
    assert_eq!(pointers[1].buttons, 0);

    let outside = dsa(&endpoint, "tap com.example.app 5000 300");
    assert_eq!(outside.status.code(), Some(1));
    assert!(stderr(&outside).contains("outside"), "{}", stderr(&outside));
    let missing = dsa(&endpoint, "tap com.example.none 1 1");
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("no task"), "{}", stderr(&missing));
}

#[test]
fn nfr2_dsa_stop_app_names_the_app_to_stop() {
    let stop = parse(&args("stop-app com.example.music")).unwrap();
    assert_eq!(stop.command, Cmd::StopApp);
    assert_eq!(stop.package.as_deref(), Some("com.example.music"));
    let missing = parse(&args("stop-app")).unwrap_err();
    assert!(missing.contains("package"), "{missing}");
}

#[test]
fn nfr2_dsa_settings_set_memory_limit_takes_auto_or_a_size() {
    use dsa_cli::Change;
    let limit =
        |value: &str| parse(&args(&format!("settings set memory-limit {value}"))).map(|i| i.change);
    assert_eq!(limit("auto").unwrap(), Some(Change::MemoryLimit(0)));
    assert_eq!(limit("8192").unwrap(), Some(Change::MemoryLimit(8192)));
    assert_eq!(limit("8G").unwrap(), Some(Change::MemoryLimit(8192)));
    assert!(limit("512").unwrap_err().contains("2048"));
    assert!(limit("lots").is_err());
}

#[test]
fn fr3_dsa_launch_asks_the_users_android_to_start_the_app() {
    use dsa_host_core::tasks::TaskRequest;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    assert!(dsa(&endpoint, "start").status.success());

    let output = dsa(&endpoint, "launch com.example.clock");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Launching com.example.clock");
    assert_eq!(
        *androids.of(&user).unwrap().requests.lock().unwrap(),
        vec![TaskRequest::Launch {
            package_name: "com.example.clock".into()
        }]
    );
}

#[test]
fn fr3_dsa_launch_says_when_android_is_not_running() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let output = dsa(&endpoint, "launch com.example.clock");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("not running"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn fr3_dsa_tasks_lists_the_tasks_android_shows_as_windows() {
    use dsa_host_core::host_api_conformance::sample_task;
    use dsa_host_core::tasks::TaskEvent;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let none = dsa(&endpoint, "tasks");
    assert!(none.status.success(), "{}", stderr(&none));
    assert_eq!(stdout(&none), "Android has no app tasks");

    let android = androids.of(&user).unwrap();
    android.publish(TaskEvent::Updated(sample_task(3, "Clock")));
    let mut focused = sample_task(8, "");
    focused.focused = true;
    android.publish(TaskEvent::Updated(focused));
    android.publish(TaskEvent::Frames {
        task_id: 3,
        display_id: 2,
    });
    let output = dsa(&endpoint, "tasks");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "Task 3: Clock (com.example.app), 1080x1920, display 2\n\
         Task 8: com.example.app (com.example.app), 1080x1920, focused"
    );
}

fn settings_service(listener: Box<dyn dsa_transport::local::LocalListener>) -> RunningService {
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::MemorySettings;
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(
            Arc::new(MemorySettings::default()),
            ServiceFacts {
                version: "0.0.9".into(),
                log_dir: Some("C:\\ProgramData\\DSA\\logs".into()),
                program_updates_available: true,
                update_channel_available: true,
                image_updates_available: true,
                android_version: Some("17.0.0-16373615-7".into()),
            },
        )
        .with_addon_disks(addon_disks());
    RunningService::start_service(listener, service)
}

/// Each user's add-on disk, a raw disk in a folder of its own for every service a test starts,
/// as the installed service keeps one in each user's data folder. The service puts the Google
/// apps and ARM translation packages there, not on Android's own disk.
fn addon_disks() -> dsa_host_core::host_api::AddonDiskSource {
    use dsa_host_core::addon_disk::{AddonDisk, Format};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dsa-cli-addons-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(move |user: &str| {
        let stem: String = user
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Ok(AddonDisk::new(
            dir.join(format!("{stem}-addons.img")),
            Format::Raw,
        ))
    })
}

fn json_of(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{}", stderr(output));
    serde_json::from_str(&stdout(output)).expect("dsa printed JSON")
}

#[test]
fn fr17_dsa_settings_set_is_parsed() {
    use dsa_cli::Change;
    let invocation = parse(&args("settings set render-gpu discrete --json")).unwrap();
    assert_eq!(invocation.command, Cmd::Settings);
    assert_eq!(
        invocation.change,
        Some(Change::RenderGpu("discrete".into()))
    );
    assert!(invocation.json);
    assert_eq!(
        parse(&args("settings set freeze on")).unwrap().change,
        Some(Change::Freeze(true))
    );
    assert_eq!(
        parse(&args("settings set program-updates off"))
            .unwrap()
            .change,
        Some(Change::ProgramUpdates(false))
    );
    assert_eq!(
        parse(&args("settings set clipboard off")).unwrap().change,
        Some(Change::Clipboard(false))
    );
    assert!(parse(&args("settings set freeze maybe")).is_err());
    assert!(parse(&args("settings set colour blue")).is_err());
    assert!(parse(&args("settings set render-gpu")).is_err());
    assert_eq!(parse(&args("memory --json")).unwrap().command, Cmd::Memory);
    assert_eq!(parse(&args("gpus")).unwrap().command, Cmd::Gpus);
    assert!(!parse(&args("status")).unwrap().json);
}

#[test]
fn fr19_trace_switches_are_settings_dsa_reads_and_parses() {
    use dsa_cli::Change;
    assert_eq!(
        parse(&args("settings set input-trace on")).unwrap().change,
        Some(Change::InputTrace(true))
    );
    assert_eq!(
        parse(&args("settings set resize-trace off"))
            .unwrap()
            .change,
        Some(Change::ResizeTrace(false))
    );
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["input_trace"], false);
    assert_eq!(settings["resize_trace"], false);
}

#[test]
fn fr20_adb_auto_connect_is_on_by_default_and_dsa_turns_it_off() {
    use dsa_cli::Change;
    assert_eq!(
        parse(&args("settings set adb-auto-connect off"))
            .unwrap()
            .change,
        Some(Change::AdbAutoConnect(false))
    );
    assert_eq!(
        parse(&args("settings set adb-auto-connect on"))
            .unwrap()
            .change,
        Some(Change::AdbAutoConnect(true))
    );
    assert!(parse(&args("settings set adb-auto-connect maybe")).is_err());
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["adb_auto_connect"], true);
    assert_eq!(settings["adb_auto_connect_state"], "off");
    let changed = json_of(&dsa(&endpoint, "settings set adb-auto-connect off --json"));
    assert_eq!(changed["adb_auto_connect"], false);
    let output = dsa(&endpoint, "settings");
    assert!(
        stdout(&output).contains("adb auto-connect: off"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn fr5_touch_gestures_off_is_on_by_default_and_dsa_turns_it_off() {
    use dsa_cli::Change;
    assert_eq!(
        parse(&args("settings set touch-gestures-off off"))
            .unwrap()
            .change,
        Some(Change::TouchGesturesOff(false))
    );
    assert_eq!(
        parse(&args("settings set touch-gestures-off on"))
            .unwrap()
            .change,
        Some(Change::TouchGesturesOff(true))
    );
    assert!(parse(&args("settings set touch-gestures-off maybe")).is_err());
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["touch_gestures_off"], true);
    assert_eq!(settings["touch_gestures_state"], "");
    assert_eq!(settings["touch_gestures_problem"], "");
    let changed = json_of(&dsa(
        &endpoint,
        "settings set touch-gestures-off off --json",
    ));
    assert_eq!(changed["touch_gestures_off"], false);
}

#[test]
fn fr28_host_certificates_are_on_by_default_and_dsa_turns_them_off() {
    use dsa_cli::Change;
    assert_eq!(
        parse(&args("settings set host-certificates off"))
            .unwrap()
            .change,
        Some(Change::HostCertificates(false))
    );
    assert!(parse(&args("settings set host-certificates maybe")).is_err());
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["host_certificates"], true);
    assert_eq!(settings["imported_certificates"], serde_json::json!([]));
    let changed = json_of(&dsa(&endpoint, "settings set host-certificates off --json"));
    assert_eq!(changed["host_certificates"], false);
}

#[test]
fn fr28_the_settings_list_the_root_certificates_found_and_where() {
    use dsa_host_core::host_api::SettingsReport;
    use dsa_host_core::host_certificates::ImportedCertificate;
    let report = SettingsReport {
        settings: dsa_host_core::settings::Settings::default(),
        facts: Default::default(),
        network: None,
        adb_auto_connect_state: Default::default(),
        touch_gestures: Default::default(),
        adb_shell_root: String::new(),
        imported_certificates: vec![ImportedCertificate {
            subject: "CN=Filter Root, O=Example Filter".into(),
            issuer: "CN=Filter Root, O=Example Filter".into(),
            source: "machine_group_policy".into(),
            location: "HKLM\\SOFTWARE\\Policies\\Microsoft\\SystemCertificates\\Root".into(),
            sha256: "ab".repeat(32),
            not_after: 1_893_456_000,
        }],
    };
    let json = dsa_cli::settings_json(&report);
    let listed = &json["imported_certificates"][0];
    assert_eq!(listed["subject"], "CN=Filter Root, O=Example Filter");
    assert_eq!(listed["source"], "machine_group_policy");
    assert_eq!(listed["not_after"], 1_893_456_000);
    let text = dsa_cli::describe_settings(&report);
    assert!(
        text.contains("Trust the root certificates added to this computer: on (1 found)"),
        "{text}"
    );
    assert!(
        text.contains("Root certificate: CN=Filter Root, O=Example Filter")
            && text.contains("this computer's group policy"),
        "{text}"
    );
}

#[test]
fn fr5_the_touch_gestures_line_says_why_it_cannot_be_used_and_is_hidden_without_gestures() {
    use dsa_host_core::host_api::SettingsReport;
    use dsa_host_core::touch_gestures::TouchGesturesState;
    let report = |state: TouchGesturesState| SettingsReport {
        settings: dsa_host_core::settings::Settings::default(),
        facts: Default::default(),
        network: None,
        adb_auto_connect_state: Default::default(),
        touch_gestures: state,
        adb_shell_root: String::new(),
        imported_certificates: Vec::new(),
    };
    let line = |report: &SettingsReport| {
        dsa_cli::describe_settings(report)
            .lines()
            .find(|line| line.starts_with("Touch gestures off in DSA app windows:"))
            .map(str::to_string)
    };
    let failed = line(&report(TouchGesturesState::Failed(
        "access is denied".into(),
    )))
    .expect("a touch gestures line");
    assert!(failed.contains("access is denied"), "{failed}");
    assert!(failed.contains("ms-settings:devices-touch"), "{failed}");
    assert_eq!(line(&report(TouchGesturesState::Unsupported)), None);
    assert!(line(&report(TouchGesturesState::Available))
        .unwrap()
        .starts_with("Touch gestures off in DSA app windows: on"));
}

#[test]
fn fr20_the_auto_connect_state_is_described_in_words() {
    use dsa_host_core::adb_discovery::AutoConnectState;
    assert_eq!(
        dsa_cli::describe_auto_connect(AutoConnectState::Attached),
        "adb lists DSA"
    );
    assert!(dsa_cli::describe_auto_connect(AutoConnectState::Detached).contains("restarts"));
}

#[test]
fn fr20_developer_mode_is_off_by_default_and_dsa_turns_it_on_with_the_adb_address() {
    use dsa_cli::Change;
    assert_eq!(
        parse(&args("settings set developer-mode on"))
            .unwrap()
            .change,
        Some(Change::DeveloperMode(true))
    );
    assert_eq!(
        parse(&args("settings set adb-port 5037")).unwrap().change,
        Some(Change::AdbPort(5037))
    );
    assert!(parse(&args("settings set adb-port 0")).is_err());
    assert!(parse(&args("settings set adb-port 70000")).is_err());
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["developer_mode"], false);
    assert_eq!(settings["adb_port"], 58526);
    let changed = json_of(&dsa(&endpoint, "settings set developer-mode on --json"));
    assert_eq!(changed["developer_mode"], true);
    assert_eq!(changed["adb_address"], "127.0.0.1:58526");
    let output = dsa(&endpoint, "settings");
    assert!(
        stdout(&output).contains("Developer mode: on (adb connect 127.0.0.1:58526)"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn fr25_dsa_arm_translation_commands_are_parsed() {
    use dsa_cli::Change;
    assert_eq!(
        parse(&args("arm-translation")).unwrap().command,
        Cmd::ArmTranslationStatus
    );
    assert_eq!(
        parse(&args("arm-translation status --json"))
            .unwrap()
            .command,
        Cmd::ArmTranslationStatus
    );
    let install = parse(&args("arm-translation install C:\\packages\\digitalis")).unwrap();
    assert_eq!(install.command, Cmd::ArmTranslationInstall);
    assert_eq!(
        install.file.as_deref(),
        Some(std::path::Path::new("C:\\packages\\digitalis"))
    );
    assert_eq!(
        parse(&args("arm-translation remove")).unwrap().command,
        Cmd::ArmTranslationRemove
    );
    assert_eq!(
        parse(&args("arm-translation use-default --json"))
            .unwrap()
            .command,
        Cmd::ArmTranslationUseDefault
    );
    assert!(parse(&args("arm-translation install")).is_err());
    assert!(parse(&args("arm-translation enable")).is_err());
    let fetch = parse(&args("arm-translation fetch google")).unwrap();
    assert_eq!(fetch.command, Cmd::ArmTranslationFetchGoogle);
    assert!(!fetch.yes, "it asks first");
    let agreed = parse(&args("arm-translation fetch google --yes --json")).unwrap();
    assert_eq!(agreed.command, Cmd::ArmTranslationFetchGoogle);
    assert!(agreed.yes && agreed.json);
    assert!(parse(&args("arm-translation fetch")).is_err());
    assert!(parse(&args("arm-translation fetch houdini")).is_err());
    assert!(parse(&args("arm-translation status --yes")).is_err());
    assert_eq!(
        parse(&args("settings set arm-translation off"))
            .unwrap()
            .change,
        Some(Change::ArmTranslation(false))
    );
}

#[test]
fn fr25_dsa_installs_reports_and_removes_an_arm_translation_package() {
    let folder = std::env::temp_dir().join(format!("dsa-cli-arm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("arm-translation.json"),
        r#"{"name": "Translator", "version": "1.0", "library": "libtranslator.so",
            "abis": ["arm64-v8a"], "license": "Apache-2.0"}"#,
    )
    .unwrap();
    let mut image = vec![0u8; 8192];
    image[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
    std::fs::write(folder.join("system.img"), &image).unwrap();

    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let none = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert!(none["package"].is_null());
    assert_eq!(none["supported"], false);
    assert_eq!(none["enabled"], true);

    let installed = dsa(
        &endpoint,
        &format!("arm-translation install {} --json", folder.display()),
    );
    let installed = json_of(&installed);
    assert_eq!(installed["package"]["name"], "Translator");
    assert_eq!(installed["supported"], true);
    assert_eq!(installed["android"]["installed"], true);
    assert_eq!(installed["restart_needed"], true);
    let text = stdout(&dsa(&endpoint, "arm-translation status"));
    assert!(
        text.contains("ARM apps: supported by Translator 1.0 (Apache-2.0)"),
        "{text}"
    );
    assert!(text.contains("Restart Android"), "{text}");

    let removed = json_of(&dsa(&endpoint, "arm-translation remove --json"));
    assert!(removed["package"].is_null());
    assert_eq!(removed["android"]["installed"], false);

    // A folder that is not a package is refused before Android is asked for anything.
    std::fs::write(folder.join("system.img"), vec![0u8; 8192]).unwrap();
    let refused = dsa(
        &endpoint,
        &format!("arm-translation install {}", folder.display()),
    );
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("erofs"), "{}", stderr(&refused));
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn fr25_dsa_uses_the_images_default_translator_until_a_package_replaces_it() {
    use dsa_host_core::arm_translation::Package;
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::MemorySettings;
    let folder = std::env::temp_dir().join(format!("dsa-cli-arm-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("arm-translation.json"),
        r#"{"name": "Translator", "version": "1.0", "library": "libtranslator.so",
            "abis": ["arm64-v8a"], "license": "Apache-2.0"}"#,
    )
    .unwrap();
    let mut image = vec![0u8; 8192];
    image[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
    std::fs::write(folder.join("system.img"), &image).unwrap();
    let digitalis = Package {
        name: "Digitalis".into(),
        version: "17-abefbe2b1600".into(),
        library: "libberberis_arm64.so".into(),
        abis: vec!["arm64-v8a".into()],
        license: "Apache-2.0".into(),
    };
    let ((listener, _, _), endpoint) = local_pair();
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(Arc::new(MemorySettings::default()), ServiceFacts::default())
        .with_default_arm_translation(Arc::new(move |_: &str| Some(digitalis.clone())))
        .with_addon_disks(addon_disks());
    let _service = RunningService::start_service(listener, service);

    // From the first start, with nothing installed.
    let first = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert!(first["package"].is_null(), "{first}");
    assert_eq!(first["default"]["name"], "Digitalis");
    assert_eq!(first["translator"]["source"], "default");
    assert_eq!(first["translator"]["name"], "Digitalis");
    assert_eq!(first["translator"]["version"], "17-abefbe2b1600");
    assert_eq!(first["translator"]["license"], "Apache-2.0");
    assert_eq!(first["supported"], true);
    assert_eq!(first["restart_needed"], false);
    let text = stdout(&dsa(&endpoint, "arm-translation"));
    assert!(
        text.contains(
            "ARM apps: supported by Digitalis 17-abefbe2b1600 (Apache-2.0), this Android's default translator"
        ),
        "{text}"
    );
    let already = stdout(&dsa(&endpoint, "arm-translation use-default"));
    assert!(
        already.contains("already uses its default translator"),
        "{already}"
    );

    // A package the user installs replaces it, and use-default goes back.
    let installed = json_of(&dsa(
        &endpoint,
        &format!("arm-translation install {} --json", folder.display()),
    ));
    assert_eq!(installed["translator"]["source"], "user");
    assert_eq!(installed["translator"]["name"], "Translator");
    assert_eq!(installed["default"]["name"], "Digitalis");
    assert_eq!(installed["restart_needed"], true);
    let text = stdout(&dsa(&endpoint, "arm-translation status"));
    assert!(text.contains("the package you installed"), "{text}");
    assert!(text.contains("dsa arm-translation use-default"), "{text}");
    let back = json_of(&dsa(&endpoint, "arm-translation use-default --json"));
    assert!(back["package"].is_null(), "{back}");
    assert_eq!(back["translator"]["source"], "default");
    assert_eq!(back["android"]["installed"], false);

    // Turned off, nothing translates, and the default is still offered.
    let off = dsa(&endpoint, "settings set arm-translation off");
    assert!(off.status.success(), "{}", stderr(&off));
    let off = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert!(off["translator"].is_null(), "{off}");
    assert_eq!(off["supported"], false);
    assert_eq!(off["default"]["name"], "Digitalis");
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn fr25_dsa_offers_no_default_translator_where_the_image_has_none() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let none = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert!(none["default"].is_null(), "{none}");
    assert!(none["translator"].is_null(), "{none}");
    let refused = dsa(&endpoint, "arm-translation use-default");
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("no default translator")
            || stderr(&refused).contains("no default ARM translator"),
        "{}",
        stderr(&refused)
    );
}

/// `dsa` with Google's repository replaced by the one in `repository`.
fn dsa_with_repository(endpoint: &str, line: &str, repository: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(args(line))
        .args([endpoint_flag(), endpoint])
        .env("DSA_GOOGLE_REPOSITORY", repository)
        .output()
        .expect("running dsa")
}

#[test]
fn fr25_dsa_fetches_googles_translator_and_installs_it_only_when_agreed() {
    use dsa_host_core::arm_translation::google::fixture;
    let id = std::process::id();
    let repository = std::env::temp_dir().join(format!("dsa-cli-google-{id}"));
    let _ = std::fs::remove_dir_all(&repository);
    let zip = fixture::image_archive(true);
    fixture::write_repository(&repository, &zip, None, &["36", "37.0"]).unwrap();
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let status = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert_eq!(
        status["google_fetch_available"],
        dsa_cli::GOOGLE_FETCH_AVAILABLE
    );
    let agreed = "arm-translation fetch google --yes --json";
    if !dsa_cli::GOOGLE_FETCH_AVAILABLE {
        // An ARM64 computer runs ARM apps itself.
        let refused = dsa_with_repository(&endpoint, agreed, &repository);
        assert!(!refused.status.success());
        assert!(stderr(&refused).contains("ARM64"), "{}", stderr(&refused));
        let _ = std::fs::remove_dir_all(&repository);
        return;
    }

    // Asked with nobody to answer, it says where the download comes from and its terms, and
    // downloads nothing.
    let asked = dsa_with_repository(&endpoint, "arm-translation fetch google", &repository);
    assert!(!asked.status.success());
    let said = stderr(&asked);
    assert!(said.contains("Android SDK License"), "{said}");
    assert!(
        said.contains("https://developer.android.com/studio/terms"),
        "{said}"
    );
    assert!(said.contains("nothing was changed"), "{said}");
    let none = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert!(none["package"].is_null(), "{none}");

    let fetched = json_of(&dsa_with_repository(&endpoint, agreed, &repository));
    assert_eq!(fetched["package"]["name"], "Google ARM translator");
    assert_eq!(fetched["package"]["library"], "libndk_translation.so");
    assert_eq!(fetched["package"]["license"], "Android SDK License");
    assert_eq!(fetched["package"]["abis"][0], "arm64-v8a");
    assert_eq!(fetched["android"]["installed"], true);
    assert_eq!(fetched["restart_needed"], true);
    let _ = std::fs::remove_dir_all(&repository);
}

#[test]
fn fr25_dsa_refuses_googles_image_when_its_hash_differs() {
    use dsa_host_core::arm_translation::google::fixture;
    if !dsa_cli::GOOGLE_FETCH_AVAILABLE {
        return;
    }
    let id = std::process::id();
    let repository = std::env::temp_dir().join(format!("dsa-cli-google-hash-{id}"));
    let _ = std::fs::remove_dir_all(&repository);
    let zip = fixture::image_archive(true);
    let wrong = Some("0123456789012345678901234567890123456789");
    fixture::write_repository(&repository, &zip, wrong, &["36", "37.0"]).unwrap();
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let line = "arm-translation fetch google --yes";
    let refused = dsa_with_repository(&endpoint, line, &repository);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("SHA-1"), "{}", stderr(&refused));
    let none = json_of(&dsa(&endpoint, "arm-translation --json"));
    assert!(none["package"].is_null(), "the stored state is unchanged");
    let _ = std::fs::remove_dir_all(&repository);
}

#[test]
fn fr37_dsa_drm_widevine_commands_are_parsed() {
    assert_eq!(
        parse(&args("drm widevine")).unwrap().command,
        Cmd::WidevineStatus
    );
    assert_eq!(
        parse(&args("drm widevine status --json")).unwrap().command,
        Cmd::WidevineStatus
    );
    let install = parse(&args("drm widevine install --yes")).unwrap();
    assert_eq!(install.command, Cmd::WidevineInstall);
    assert!(install.yes);
    assert_eq!(
        parse(&args("drm widevine remove")).unwrap().command,
        Cmd::WidevineRemove
    );
    assert!(parse(&args("drm playready")).is_err());
    assert!(parse(&args("drm widevine upgrade")).is_err());
    assert!(parse(&args("drm widevine remove --yes")).is_err());
}

#[test]
fn fr37_dsa_takes_widevine_from_googles_image_only_when_agreed_and_removes_it() {
    use dsa_host_core::widevine::{self, fixture};
    let id = std::process::id();
    let repository = std::env::temp_dir().join(format!("dsa-cli-widevine-{id}"));
    let _ = std::fs::remove_dir_all(&repository);
    let abi = widevine::guest_abi();
    let module = fixture::apex(widevine::MODULE, abi);
    // The service's facts say Android 17, so the image of API 37 is the one taken.
    let zip = fixture::image_archive("37", abi, Some(&module));
    fixture::write_repository(&repository, &[("37.0", abi, &zip)]).unwrap();
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let none = json_of(&dsa(&endpoint, "drm widevine --json"));
    assert!(none["package"].is_null(), "{none}");
    assert_eq!(none["installed"], false);
    assert_eq!(none["security_level"], "L3");

    // Asked with nobody to answer, it says where the download comes from and its terms, and
    // changes nothing.
    let asked = dsa_with_repository(&endpoint, "drm widevine install", &repository);
    assert!(!asked.status.success());
    let said = stderr(&asked);
    assert!(said.contains("Android SDK License"), "{said}");
    assert!(said.contains("nothing was changed"), "{said}");
    assert!(json_of(&dsa(&endpoint, "drm widevine --json"))["package"].is_null());

    let line = "drm widevine install --yes --json";
    let installed = json_of(&dsa_with_repository(&endpoint, line, &repository));
    assert_eq!(installed["installed"], true, "{installed}");
    assert_eq!(installed["on_disk"], true);
    assert_eq!(installed["package"]["abi"], abi);
    assert_eq!(installed["package"]["android_major"], 17);
    assert_eq!(
        installed["package"]["apex"],
        "com.google.android.widevine.apex"
    );
    assert_eq!(installed["package"]["size"], module.len());
    assert_eq!(
        installed["package"]["sha256"],
        widevine::sha256_hex(&module)
    );
    let text = stdout(&dsa(&endpoint, "drm widevine status"));
    assert!(text.contains("standard definition"), "{text}");
    assert!(text.contains("browser or app"), "{text}");

    let removed = json_of(&dsa(&endpoint, "drm widevine remove --json"));
    assert!(removed["package"].is_null());
    assert_eq!(removed["on_disk"], false);
    let _ = std::fs::remove_dir_all(&repository);
}

#[test]
fn fr37_dsa_refuses_an_image_without_widevine_and_changes_nothing() {
    use dsa_host_core::widevine::{self, fixture};
    let id = std::process::id();
    let repository = std::env::temp_dir().join(format!("dsa-cli-widevine-none-{id}"));
    let _ = std::fs::remove_dir_all(&repository);
    let abi = widevine::guest_abi();
    let zip = fixture::image_archive("37", abi, None);
    fixture::write_repository(&repository, &[("37.0", abi, &zip)]).unwrap();
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let refused = dsa_with_repository(&endpoint, "drm widevine install --yes", &repository);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("holds no Widevine module"),
        "{}",
        stderr(&refused)
    );
    let none = json_of(&dsa(&endpoint, "drm widevine --json"));
    assert!(none["package"].is_null(), "the stored state is unchanged");
    assert_eq!(none["on_disk"], false);
    let _ = std::fs::remove_dir_all(&repository);
}

#[test]
fn fr17_dsa_status_json_reports_the_state_and_the_last_reason() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let status = json_of(&dsa(&endpoint, "status --json"));
    assert_eq!(status["state"], "Stopped");
    assert_eq!(status["last_reason"], "stopped on request");
    assert!(status["update_notice"].is_null());
    let started = json_of(&dsa(&endpoint, "start --wait --timeout 10 --json"));
    assert_eq!(started["state"], "Active");
    assert!(
        status["program_update"].is_null(),
        "no update of DSA is reported by a service that does not update DSA"
    );
}

#[test]
fn fr15_dsa_status_says_how_far_an_update_of_dsa_has_got() {
    use dsa_host_core::host_api::{ProgramPhase, ProgramProgress, Status};
    let status = |phase| Status {
        state: SessionState::Stopped,
        last_reason: None,
        update_notice: None,
        problem: None,
        program_update: Some(ProgramProgress::new(phase, "0.1.11")),
    };
    for (phase, name, said) in [
        (
            ProgramPhase::Downloading,
            "downloading",
            "downloading DSA 0.1.11",
        ),
        (
            ProgramPhase::StoppingAndroid,
            "stopping_android",
            "stopping Android to install DSA 0.1.11",
        ),
        (
            ProgramPhase::Installing,
            "installing",
            "installing DSA 0.1.11",
        ),
        (
            ProgramPhase::Restarting,
            "restarting",
            "DSA is restarting to finish the update to 0.1.11",
        ),
    ] {
        let line = dsa_cli::describe(&status(phase));
        assert!(line.contains(&format!("\nUpdating DSA: {said}")), "{line}");
        if phase != ProgramPhase::Downloading {
            assert!(
                line.contains("do not close DSA or start Android apps"),
                "{line}"
            );
        }
        let json = dsa_cli::status_json(&status(phase));
        assert_eq!(json["program_update"]["phase"], name);
        assert_eq!(json["program_update"]["version"], "0.1.11");
        assert_eq!(json["program_update"]["in_progress"], true);
    }
    // An update that has ended is told by the news; the status line adds nothing.
    for (phase, name) in [
        (ProgramPhase::Applied, "applied"),
        (ProgramPhase::Failed, "failed"),
    ] {
        assert_eq!(dsa_cli::describe(&status(phase)), "Android: Stopped");
        let json = dsa_cli::status_json(&status(phase));
        assert_eq!(json["program_update"]["phase"], name);
        assert_eq!(json["program_update"]["in_progress"], false);
    }
}

#[test]
fn fr15_dsa_status_no_start_asks_a_running_service_only() {
    let invocation = parse(&args("status --no-start --json")).unwrap();
    assert_eq!(invocation.command, Cmd::Status);
    assert!(invocation.no_start);
    assert!(!parse(&args("status")).unwrap().no_start);
    assert!(
        parse(&args("start --no-start")).is_err(),
        "only a status can be asked for without starting the service"
    );
}

#[test]
fn fr17_dsa_settings_reads_and_changes_the_settings_through_the_service() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["render_gpu"], "display");
    assert_eq!(settings["freeze"], false);
    assert_eq!(settings["clipboard"], true);
    assert_eq!(settings["program_updates"], true);
    assert_eq!(settings["version"], "0.0.9");
    assert_eq!(settings["android_version"], "17.0.0-16373615-7");
    assert_eq!(settings["log_dir"], "C:\\ProgramData\\DSA\\logs");

    let changed = json_of(&dsa(&endpoint, "settings set render-gpu Integrated --json"));
    assert_eq!(changed["render_gpu"], "integrated");
    let changed = json_of(&dsa(&endpoint, "settings set freeze on --json"));
    assert_eq!(changed["freeze"], true);
    assert_eq!(
        changed["render_gpu"], "integrated",
        "one change keeps the others"
    );

    let changed = json_of(&dsa(&endpoint, "settings set clipboard off --json"));
    assert_eq!(changed["clipboard"], false);
    assert_eq!(changed["freeze"], true, "one change keeps the others");

    let text = dsa(&endpoint, "settings");
    assert!(text.status.success(), "{}", stderr(&text));
    assert!(
        stdout(&text).contains("Clipboard sharing: off"),
        "{}",
        stdout(&text)
    );
    assert!(
        stdout(&text).contains("Render GPU: integrated"),
        "{}",
        stdout(&text)
    );
    assert!(
        stdout(&text).contains("Freeze when idle: on"),
        "{}",
        stdout(&text)
    );
    assert!(
        stdout(&text).contains("Android image: 17.0.0-16373615-7"),
        "{}",
        stdout(&text)
    );
}

#[test]
fn fr17_dsa_settings_reports_why_a_value_was_refused() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(["settings", "set", "render-gpu", "   ", "--json"])
        .args([endpoint_flag(), &endpoint])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("render GPU"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn fr17_dsa_memory_json_says_what_it_knows_of_the_host_and_android() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let memory = json_of(&dsa(&endpoint, "memory --json"));
    assert!(memory["guest"].is_null(), "{memory}");
    assert!(memory["host"].as_array().unwrap().is_empty(), "{memory}");
    assert_eq!(memory["host_total_kb"], 0);
}

#[test]
fn fr17_dsa_memory_json_carries_the_guest_breakdown_and_the_host_processes() {
    use dsa_host_core::memory::{
        GuestMemory, GuestMemoryReport, HostProcessMemory, HostRole, MemorySnapshot, ProcessMemory,
    };
    let json = dsa_cli::memory_json(&MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory {
                mem_total_kb: 4 * 1024 * 1024,
                mem_available_kb: 3 * 1024 * 1024,
                cached_kb: 200 * 1024,
                total_pss_kb: 500 * 1024,
                top_processes: vec![ProcessMemory {
                    pid: 42,
                    name: "system_server".into(),
                    pss_kb: 150 * 1024,
                    swap_pss_kb: 0,
                    google_play: false,
                }],
                ..GuestMemory::default()
            },
            age: Duration::from_secs(12),
        }),
        host: vec![HostProcessMemory {
            role: HostRole::Vm,
            pid: 10,
            working_set_kb: 900_000,
            private_kb: 880_000,
        }],
    });
    assert_eq!(json["guest"]["mem_total_kb"], 4 * 1024 * 1024);
    assert_eq!(json["guest"]["total_pss_kb"], 500 * 1024);
    assert_eq!(json["guest"]["age_seconds"], 12);
    assert_eq!(json["guest"]["top_processes"][0]["name"], "system_server");
    assert_eq!(json["host"][0]["role"], "vm");
    assert_eq!(json["host_total_kb"], 900_000);
}

#[test]
fn nfr2_dsa_memory_json_counts_zram_compressed_and_google_play_services_apart() {
    use dsa_host_core::memory::{
        GooglePlayMemory, GuestMemory, GuestMemoryReport, MemorySnapshot, ProcessMemory,
    };
    const MIB: u64 = 1024;
    let json = dsa_cli::memory_json(&MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory {
                zram_original_kb: 300 * MIB,
                zram_used_kb: 100 * MIB,
                total_pss_kb: 500 * MIB,
                swap_pss_kb: 300 * MIB,
                top_processes: vec![ProcessMemory {
                    pid: 700,
                    name: "com.google.android.gms".into(),
                    pss_kb: 81 * MIB,
                    swap_pss_kb: 75 * MIB,
                    google_play: true,
                }],
                google_play: Some(GooglePlayMemory {
                    processes: 2,
                    pss_kb: 135 * MIB,
                    swap_pss_kb: 123 * MIB,
                }),
                ..GuestMemory::default()
            },
            age: Duration::from_secs(12),
        }),
        host: Vec::new(),
    });
    let guest = &json["guest"];
    assert_eq!(guest["apps"]["resident_kb"], 200 * MIB);
    assert_eq!(guest["apps"]["compressed_kb"], 100 * MIB);
    assert_eq!(guest["apps"]["total_kb"], 300 * MIB);
    assert_eq!(guest["google_play"]["processes"], 2);
    assert_eq!(guest["google_play"]["resident_kb"], 12 * MIB);
    assert_eq!(guest["google_play"]["compressed_kb"], 41 * MIB);
    assert_eq!(guest["google_play"]["total_kb"], 53 * MIB);
    let gms = &guest["top_processes"][0];
    assert_eq!(gms["google_play"], true);
    assert_eq!(gms["resident_kb"], 6 * MIB);
    assert_eq!(gms["compressed_kb"], 25 * MIB);

    let without = dsa_cli::memory_json(&MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory {
                total_pss_kb: 100 * MIB,
                swap_pss_kb: 40 * MIB,
                ..GuestMemory::default()
            },
            age: Duration::from_secs(1),
        }),
        host: Vec::new(),
    });
    assert!(without["guest"]["google_play"].is_null());
    assert!(without["guest"]["apps"]["compressed_kb"].is_null());
    assert_eq!(without["guest"]["apps"]["total_kb"], 60 * MIB);
}

#[test]
fn fr17_dsa_gpus_prints_a_list() {
    let output = Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(["gpus", "--json"])
        .output()
        .unwrap();
    let gpus = json_of(&output);
    assert!(gpus["gpus"].is_array());
}

#[test]
fn nfr1_dsa_memory_is_parsed() {
    assert_eq!(parse(&args("memory")).unwrap().command, Cmd::Memory);
    assert_eq!(
        parse(&args("memory --pipe p")).unwrap().endpoint.as_deref(),
        Some("p")
    );
}

#[test]
fn nfr1_dsa_memory_says_what_it_knows_of_the_host_and_android() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let output = dsa(&endpoint, "memory");
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("Host: 0 MiB in all"), "{text}");
    assert!(
        text.contains("no process of this Android is running"),
        "{text}"
    );
    assert!(text.contains("Android: no report yet"), "{text}");
}

/// A file in a folder of this test run, holding `content`.
fn apk_file(name: &str, content: &[u8]) -> std::path::PathBuf {
    // A folder of its own for each file: tests run at once and two may use the same name
    // (broken.apk) with different contents.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("apks-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn scratch_file(name: &str, content: &[u8]) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cli-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Runs dsa with `before`, then the path, then `after`, so a path with spaces stays whole.
fn dsa_with_path(endpoint: &str, before: &str, path: &std::path::Path, after: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(args(before))
        .arg(path)
        .args(args(after))
        .args([endpoint_flag(), endpoint])
        .output()
        .expect("running dsa")
}

#[test]
fn fr11_dsa_install_is_parsed() {
    let install = parse(&args("install notes.apk --json")).unwrap();
    assert_eq!(install.command, Cmd::Install);
    assert_eq!(
        install.file.as_deref(),
        Some(std::path::Path::new("notes.apk"))
    );
    assert!(install.json);
    assert!(parse(&args("install"))
        .unwrap_err()
        .contains("needs the APK file"));
    assert!(
        parse(&args("install a.apk b.apk")).is_err(),
        "one APK at a time"
    );
}

#[test]
fn fr11_dsa_install_starts_android_and_installs_the_apk() {
    use dsa_host_core::host_api_conformance::INSTALLED_PACKAGE;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let content: Vec<u8> = (0..3 * 1024 * 1024 + 5).map(|i| (i % 249) as u8).collect();
    let path = apk_file("My Notes.apk", &content);

    let output = dsa_with_path(&endpoint, "install", &path, "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("Installed {INSTALLED_PACKAGE} from My Notes.apk")
    );
    let android = androids.of(&user).unwrap();
    assert_eq!(
        android.state(),
        SessionState::Active,
        "Android was started first"
    );
    let installs = android.installs.lock().unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].0, "My Notes.apk");
    assert!(installs[0].1 == content, "the APK arrived changed");
}

#[test]
fn fr11_dsa_install_json_reports_the_package() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let path = apk_file("json.apk", b"PK\x03\x04 an apk");
    let json = json_of(&dsa_with_path(&endpoint, "install", &path, "--json"));
    assert_eq!(json["installed"], true);
    assert_eq!(json["status"], "installed");
    assert_eq!(json["package"], "com.example.installed");
}

#[test]
fn fr11_dsa_apk_info_and_install_progress_are_parsed() {
    let info = parse(&args("apk-info notes.apk --json")).unwrap();
    assert_eq!(info.command, Cmd::ApkInfo);
    assert_eq!(
        info.file.as_deref(),
        Some(std::path::Path::new("notes.apk"))
    );
    assert!(parse(&args("apk-info"))
        .unwrap_err()
        .contains("needs the APK file"));
    assert!(
        parse(&args("install notes.apk --progress"))
            .unwrap()
            .progress
    );
    assert!(parse(&args("status --progress")).is_err());
}

#[test]
fn fr11_dsa_apk_info_reads_the_apk_and_says_it_updates_the_installed_app() {
    use dsa_host_core::apk_info::fixture::notes_apk;
    use dsa_host_core::host_api_conformance::INSTALLED_VERSION;
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let path = apk_file("notes.apk", &notes_apk());

    // Before Android runs, everything comes from the file, and Android is not started.
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &path, "--json"));
    assert_eq!(json["package"], "com.example.notes");
    assert_eq!(json["label"], "Notes");
    assert_eq!(json["version_code"], 42);
    assert_eq!(json["version_name"], "4.2");
    assert_eq!(json["min_android"], "8.0");
    assert_eq!(json["target_android"], "14");
    assert_eq!(json["permissions"][0]["name"], "android.permission.CAMERA");
    assert_eq!(
        json["permissions"][0]["description"],
        "Take pictures and record video"
    );
    assert_eq!(json["icon"]["kind"], "image");
    assert_eq!(json["signer"]["scheme"], "v2");
    assert_eq!(json["android"], "stopped");
    assert!(json["install"].is_null());

    // With Android running, what it has of the package decides what installing does.
    assert!(dsa(&endpoint, "start --wait --timeout 10").status.success());
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &path, "--json"));
    assert_eq!(json["android"], "running");
    assert_eq!(json["installed"]["version_code"], INSTALLED_VERSION);
    assert_eq!(json["install"], "update");
    let text = stdout(&dsa_with_path(&endpoint, "apk-info", &path, ""));
    assert!(text.contains("Notes (com.example.notes)"), "{text}");
    assert!(text.contains("this APK updates it"), "{text}");
}

#[test]
fn fr25_dsa_apk_info_says_whether_android_runs_arm64_code() {
    use dsa_host_core::apk_info::fixture::notes_apk;
    use dsa_host_core::arm_translation::Package;
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::MemorySettings;
    let digitalis = Package {
        name: "Digitalis".into(),
        version: "17-abefbe2b1600".into(),
        library: "libberberis_arm64.so".into(),
        abis: vec!["arm64-v8a".into()],
        license: "Apache-2.0".into(),
    };
    let ((listener, _, _), endpoint) = local_pair();
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(Arc::new(MemorySettings::default()), ServiceFacts::default())
        .with_default_arm_translation(Arc::new(move |_: &str| Some(digitalis.clone())));
    let _service = RunningService::start_service(listener, service);
    let path = apk_file("notes-arm.apk", &notes_apk());

    // Before Android runs: it starts with the image's default translator, so an app with
    // arm64-v8a code runs, and the installer has nothing to warn of.
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &path, "--json"));
    assert_eq!(json["android"], "stopped");
    assert_eq!(json["runs_arm64"], true, "{json}");

    // With the translator turned off, it does not.
    let off = dsa(&endpoint, "settings set arm-translation off");
    assert!(off.status.success(), "{}", stderr(&off));
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &path, "--json"));
    assert_eq!(json["runs_arm64"], false, "{json}");
}

#[test]
fn fr25_dsa_apk_info_cannot_say_whether_arm64_code_runs_without_the_settings() {
    use dsa_host_core::apk_info::fixture::notes_apk;
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let path = apk_file("notes-unknown.apk", &notes_apk());
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &path, "--json"));
    assert!(json["runs_arm64"].is_null(), "{json}");
}

#[test]
fn fr11_dsa_apk_info_refuses_a_file_that_is_not_an_apk() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let path = apk_file("broken.apk", b"PK\x03\x04 not really");
    let output = dsa_with_path(&endpoint, "apk-info", &path, "--json");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("not a ZIP archive"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn fr11_dsa_install_progress_says_each_step() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let content: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let path = apk_file("progress.apk", &content);
    let output = dsa_with_path(&endpoint, "install", &path, "--progress --json");
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "stage starting", "{text}");
    assert_eq!(lines[1], "stage sending", "{text}");
    assert!(lines.contains(&"progress 100"), "{text}");
    let installing = lines
        .iter()
        .position(|l| *l == "stage installing")
        .expect(&text);
    let result: serde_json::Value = serde_json::from_str(lines[installing + 1]).expect(&text);
    assert_eq!(result["installed"], true);
}

#[test]
fn fr11_dsa_install_says_why_android_refused_the_apk_and_fails() {
    use dsa_host_core::host_api_conformance::BAD_APK;
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let path = apk_file("broken.apk", BAD_APK);
    let output = dsa_with_path(&endpoint, "install", &path, "");
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.starts_with("dsa: not installed (invalid-apk): broken.apk"),
        "{error}"
    );
    assert!(error.contains("INSTALL_PARSE_FAILED_NOT_APK"), "{error}");
}

#[test]
fn fr25_dsa_install_refuses_an_arm64_only_app_with_its_reason_while_translation_is_off() {
    use dsa_host_core::apk_info::fixture::archive;
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::MemorySettings;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let service = HostApiService::new(androids.clone(), "test service")
        .with_settings(Arc::new(MemorySettings::default()), ServiceFacts::default());
    let _service = RunningService::start_service(listener, service);
    let content = archive(
        &[
            ("AndroidManifest.xml", &b"manifest"[..]),
            ("lib/arm64-v8a/libgame.so", &b"\x7fELF"[..]),
        ],
        b"",
    );
    let path = apk_file("arm-game.apk", &content);

    // The running Android does not translate ARM code: refused with the reason, unsent.
    let output = dsa_with_path(&endpoint, "install", &path, "");
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.starts_with("dsa: not installed (translation-off): arm-game.apk: "),
        "{error}"
    );
    assert!(error.contains("ARM translation is not on"), "{error}");
    assert!(error.contains("Turn it on under ARM apps"), "{error}");
    let android = androids.of(&user).unwrap();
    assert!(android.installs.lock().unwrap().is_empty());

    // It does: installed as arm64-v8a.
    *android.translating.lock().unwrap() = true;
    let output = dsa_with_path(&endpoint, "install", &path, "");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        *android.install_abis.lock().unwrap(),
        vec![Some("arm64-v8a".to_string())]
    );
}

#[test]
fn fr11_dsa_install_refuses_a_file_that_is_no_apk_before_starting_android() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let path = apk_file("notes.zip", b"PK");
    let output = dsa_with_path(&endpoint, "install", &path, "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("not an APK file"),
        "{}",
        stderr(&output)
    );
    let missing = dsa(&endpoint, "install no-such-file.apk");
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("cannot read"),
        "{}",
        stderr(&missing)
    );
    // Android was not started for either.
    assert!(androids
        .of(&user)
        .is_none_or(|android| android.state() == SessionState::Stopped));
}

/// An APKPure bundle of the notes app: its base APK, which has complete x86_64 and arm64-v8a
/// libraries, splits for two languages and two densities, and an expansion file.
fn notes_xapk() -> Vec<u8> {
    use dsa_host_core::apk_info::fixture::{archive, notes_apk, notes_split};
    let base = notes_apk();
    let en = notes_split("config.en", &[]);
    let fr = notes_split("config.fr", &[]);
    let mdpi = notes_split("config.mdpi", &[]);
    let xxxhdpi = notes_split("config.xxxhdpi", &[]);
    archive(
        &[
            ("manifest.json", &b"{}"[..]),
            ("com.example.notes.apk", &base[..]),
            ("config.en.apk", &en[..]),
            ("config.fr.apk", &fr[..]),
            ("config.mdpi.apk", &mdpi[..]),
            ("config.xxxhdpi.apk", &xxxhdpi[..]),
            (
                "Android/obb/com.example.notes/main.42.com.example.notes.obb",
                &b"expansion"[..],
            ),
        ],
        b"",
    )
}

#[test]
fn fr11_dsa_install_installs_the_splits_of_an_xapk_this_android_takes_in_one_install() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let path = apk_file("Notes.xapk", &notes_xapk());

    let output = dsa_with_path(&endpoint, "install", &path, "--json");
    assert!(output.status.success(), "{}", stderr(&output));
    let android = androids.of(&user).unwrap();
    let installs = android.part_installs.lock().unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].name, "Notes.xapk");
    let names: Vec<&str> = installs[0]
        .parts
        .iter()
        .map(|part| part.name.as_str())
        .collect();
    // The base, one density, English (and the host's language, when it is French), and the
    // expansion file last.
    assert_eq!(names[0], "base.apk", "{names:?}");
    assert!(names.contains(&"split_config.en.apk"), "{names:?}");
    assert_eq!(
        names.iter().filter(|name| name.contains("dpi")).count(),
        1,
        "{names:?}"
    );
    assert_eq!(
        names.last(),
        Some(&"main.42.com.example.notes.obb"),
        "{names:?}"
    );
    assert!(installs[0].parts.last().unwrap().obb);
    // The base APK's x86_64 libraries are whole, so the app is x86_64.
    assert_eq!(installs[0].abi.as_deref(), Some("x86_64"));
    assert!(android.installs.lock().unwrap().is_empty());
}

#[test]
fn fr11_dsa_apk_info_reads_the_base_of_a_bundle_and_says_what_installing_sends() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    // Told by what is in it, whatever its name.
    let path = apk_file("notes.zip", &notes_xapk());
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &path, "--json"));
    assert_eq!(json["package"], "com.example.notes");
    assert_eq!(json["label"], "Notes");
    assert_eq!(json["format"], "xapk");
    assert_eq!(json["apks"][0], "base.apk");
    assert!(json["split_count"].as_u64().unwrap() >= 2, "{json}");
    assert_eq!(json["obbs"][0], "main.42.com.example.notes.obb");
    assert!(json["total_size"].as_u64().unwrap() > json["size"].as_u64().unwrap());
    let text = stdout(&dsa_with_path(&endpoint, "apk-info", &path, ""));
    assert!(text.contains("split-APK bundle (.xapk)"), "{text}");
    // One APK says it is one.
    let apk = apk_file("notes.apk", &dsa_host_core::apk_info::fixture::notes_apk());
    let json = json_of(&dsa_with_path(&endpoint, "apk-info", &apk, "--json"));
    assert_eq!(json["format"], "apk");
    assert_eq!(json["split_count"], 0);
    assert_eq!(json["total_size"], json["size"]);
}

#[test]
fn fr11_dsa_install_refuses_an_app_bundle_and_an_encrypted_bundle_with_their_reasons() {
    use dsa_host_core::apk_info::fixture::archive;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let aab = archive(
        &[
            ("BundleConfig.pb", &b"\x0a\x00"[..]),
            ("base/manifest/AndroidManifest.xml", &b"\x03\x00"[..]),
        ],
        b"",
    );
    let aab = apk_file("notes.aab", &aab);
    let output = dsa_with_path(&endpoint, "install", &aab, "");
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.starts_with("dsa: not installed (app-bundle): notes.aab: "),
        "{error}"
    );
    assert!(error.contains("another key"), "{error}");
    let sealed = archive(
        &[
            ("info.json", &b"{}"[..]),
            ("base.apk", &b"\x13\x37 sealed"[..]),
        ],
        b"",
    );
    let sealed = apk_file("notes.apkm", &sealed);
    for command in ["install", "apk-info"] {
        let output = dsa_with_path(&endpoint, command, &sealed, "");
        assert_eq!(output.status.code(), Some(1), "{command}");
        let error = stderr(&output);
        assert!(
            error.starts_with("dsa: not installed (encrypted-bundle): notes.apkm: "),
            "{command}: {error}"
        );
    }
    // Android was not started for any of them.
    assert!(androids
        .of(&user)
        .is_none_or(|android| android.state() == SessionState::Stopped));
}

#[test]
fn fr11_two_dsa_installs_at_once_both_succeed_one_after_the_other() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    assert!(dsa(&endpoint, "start").status.success());
    *androids.of(&user).unwrap().install_time.lock().unwrap() = Duration::from_millis(300);
    let one = apk_file("one.apk", b"one");
    let two = apk_file("two.apk", b"two");
    let first = {
        let endpoint = endpoint.clone();
        std::thread::spawn(move || dsa_with_path(&endpoint, "install", &one, ""))
    };
    let second = dsa_with_path(&endpoint, "install", &two, "");
    let first = first.join().unwrap();
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(second.status.success(), "{}", stderr(&second));
    let steps = androids
        .of(&user)
        .unwrap()
        .install_steps
        .lock()
        .unwrap()
        .clone();
    assert_eq!(steps.len(), 4, "{steps:?}");
    assert_eq!(
        steps[0][1..],
        steps[1][1..],
        "the installs overlapped: {steps:?}"
    );
}

#[test]
fn fr9_dsa_open_and_dsa_share_are_parsed() {
    use dsa_cli::Change;
    let open = parse(&args("open C:\\Users\\me\\photo.jpg")).unwrap();
    assert_eq!(open.command, Cmd::Open);
    assert_eq!(
        open.file.as_deref(),
        Some(std::path::Path::new("C:\\Users\\me\\photo.jpg"))
    );
    assert_eq!(parse(&args("share notes.txt")).unwrap().command, Cmd::Share);
    let missing = parse(&args("open")).unwrap_err();
    assert!(missing.contains("file"), "{missing}");
    assert!(
        parse(&args("share a.txt b.txt")).is_err(),
        "one file at a time"
    );
    assert_eq!(
        parse(&args("settings set links-in-android on"))
            .unwrap()
            .change,
        Some(Change::LinksInAndroid(true))
    );
}

#[test]
fn fr9_dsa_open_sends_the_file_to_the_users_android() {
    use dsa_host_core::files::FileAction;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let path = scratch_file("report.pdf", b"%PDF-1.7 hello");

    // Android is started first when it is not running.
    let output = dsa(&endpoint, &format!("open {}", path.display()));
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("report.pdf"),
        "{}",
        stdout(&output)
    );
    let output = dsa(&endpoint, &format!("share {}", path.display()));
    assert!(output.status.success(), "{}", stderr(&output));

    let files = androids.of(&user).unwrap().files.lock().unwrap().clone();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].name, "report.pdf");
    assert_eq!(files[0].content, b"%PDF-1.7 hello");
    assert_eq!(files[0].action, FileAction::View);
    assert_eq!(files[1].action, FileAction::Send);
}

#[test]
fn fr9_dsa_open_refuses_a_file_larger_than_the_limit() {
    use dsa_host_core::files::MAX_FILE_BYTES;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let path = scratch_file("movie.mkv", b"");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    let output = dsa(&endpoint, &format!("open {}", path.display()));
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("64 MiB"), "{}", stderr(&output));
    assert!(
        androids
            .of(&user)
            .is_none_or(|android| android.files.lock().unwrap().is_empty()),
        "nothing was sent"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fr28_dsa_turns_advanced_networking_on_and_dsa_network_shows_the_address_and_dns() {
    use dsa_cli::Change;
    use dsa_host_core::host_api::{HostApiService, NetworkSource, NetworkStatus, ServiceFacts};
    use dsa_host_core::settings::{MemorySettings, Settings};
    assert_eq!(
        parse(&args("settings set advanced-networking on"))
            .unwrap()
            .change,
        Some(Change::AdvancedNetworking(true))
    );
    assert_eq!(
        parse(&args("network --json")).unwrap().command,
        Cmd::Network
    );
    let ((listener, _, _), endpoint) = local_pair();
    let resolver = Arc::new(Mutex::new(String::new()));
    let network: NetworkSource = Arc::new({
        let resolver = Arc::clone(&resolver);
        move |_: &str, settings: &Settings| {
            Some(NetworkStatus {
                mode: if settings.advanced_networking {
                    NetworkStatus::ADVANCED.into()
                } else {
                    NetworkStatus::NAT.into()
                },
                addresses: vec!["172.28.16.2".into(), "fd5a:d5a:0:1::2".into()],
                dns_servers: vec!["172.28.16.1".into()],
                gateway: "172.28.16.1".into(),
                problem: None,
                dns_resolver: resolver.lock().unwrap().clone(),
            })
        }
    });
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(Arc::new(MemorySettings::default()), ServiceFacts::default())
        .with_network(network);
    let _service = RunningService::start_service(listener, service);

    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["advanced_networking"], false);
    assert_eq!(settings["network"]["mode"], "nat");
    let status = json_of(&dsa(&endpoint, "network --json"));
    assert_eq!(status["addresses"][0], "172.28.16.2");
    assert_eq!(status["addresses"][1], "fd5a:d5a:0:1::2");
    assert_eq!(status["dns_servers"][0], "172.28.16.1");
    assert_eq!(status["gateway"], "172.28.16.1");
    let text = stdout(&dsa(&endpoint, "network"));
    assert!(text.contains("Network: NAT"), "{text}");
    assert!(
        text.contains("Addresses: 172.28.16.2, fd5a:d5a:0:1::2"),
        "{text}"
    );
    assert!(text.contains("DNS servers: 172.28.16.1"), "{text}");

    // Who answers Android's names: this computer's resolver, or the network's DNS servers
    // when that does not answer.
    *resolver.lock().unwrap() = NetworkStatus::DNS_HOST.into();
    let status = json_of(&dsa(&endpoint, "network --json"));
    assert_eq!(status["dns_resolver"], "host");
    let text = stdout(&dsa(&endpoint, "network"));
    assert!(
        text.contains("DNS: answered by this computer's resolver"),
        "{text}"
    );
    *resolver.lock().unwrap() = NetworkStatus::DNS_NETWORK.into();
    let status = json_of(&dsa(&endpoint, "network --json"));
    assert_eq!(status["dns_resolver"], "network");
    let text = stdout(&dsa(&endpoint, "network"));
    assert!(
        text.contains("DNS: this computer's resolver does not answer, so Android asks 172.28.16.1"),
        "{text}"
    );

    let changed = json_of(&dsa(
        &endpoint,
        "settings set advanced-networking on --json",
    ));
    assert_eq!(changed["advanced_networking"], true);
    assert_eq!(changed["network"]["mode"], "advanced");
    let text = stdout(&dsa(&endpoint, "settings"));
    assert!(text.contains("Advanced networking: on"), "{text}");
}

#[test]
fn fr9_dsa_settings_turns_links_in_android_on_and_off() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["links_in_android"], false);
    let changed = json_of(&dsa(&endpoint, "settings set links-in-android on --json"));
    assert_eq!(changed["links_in_android"], true);
    let text = dsa(&endpoint, "settings");
    assert!(
        stdout(&text).contains("Web links open in: Android apps"),
        "{}",
        stdout(&text)
    );
    let changed = json_of(&dsa(&endpoint, "settings set links-in-android off --json"));
    assert_eq!(changed["links_in_android"], false);
}

#[test]
fn fr29_dsa_settings_turns_file_sharing_off_and_chooses_the_folders() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["file_sharing"], true);
    assert_eq!(
        settings["shared_folders"],
        serde_json::json!([
            "Documents",
            "Downloads",
            "Pictures",
            "Music",
            "Videos",
            "Desktop"
        ])
    );
    let changed = json_of(&dsa(
        &endpoint,
        "settings set shared-folders pictures,Documents --json",
    ));
    assert_eq!(
        changed["shared_folders"],
        serde_json::json!(["Documents", "Pictures"])
    );
    let changed = json_of(&dsa(&endpoint, "settings set file-sharing off --json"));
    assert_eq!(changed["file_sharing"], false);
    let text = dsa(&endpoint, "settings");
    assert!(
        stdout(&text).contains("File sharing: off (Documents, Pictures)"),
        "{}",
        stdout(&text)
    );
    let changed = json_of(&dsa(&endpoint, "settings set shared-folders none --json"));
    assert_eq!(changed["shared_folders"], serde_json::json!([]));
}

#[test]
fn fr29_dsa_settings_refuses_a_folder_it_cannot_share() {
    assert!(parse(&args("settings set shared-folders C:\\Windows")).is_err());
    assert_eq!(
        parse(&args("settings set shared-folders all"))
            .unwrap()
            .change,
        Some(dsa_cli::Change::SharedFolders(
            dsa_host_core::file_share::all_folders()
        ))
    );
}

#[test]
fn fr29_dsa_files_is_parsed() {
    assert_eq!(parse(&args("files")).unwrap().command, Cmd::Files);
    assert_eq!(parse(&args("files open")).unwrap().command, Cmd::FilesOpen);
}

#[test]
fn fr12_dsa_widgets_is_parsed() {
    use dsa_cli::WidgetAction;
    assert_eq!(
        parse(&args("widgets")).unwrap().widgets,
        Some(WidgetAction::List)
    );
    assert_eq!(
        parse(&args("widgets list --json")).unwrap().widgets,
        Some(WidgetAction::List)
    );
    assert_eq!(
        parse(&args("widgets add com.example.clock/.ClockWidget"))
            .unwrap()
            .widgets,
        Some(WidgetAction::Add("com.example.clock/.ClockWidget".into()))
    );
    assert_eq!(
        parse(&args("widgets remove 12")).unwrap().widgets,
        Some(WidgetAction::Remove(12))
    );
    assert_eq!(
        parse(&args("widgets watch --json")).unwrap().widgets,
        Some(WidgetAction::Watch)
    );
    assert!(parse(&args("widgets add")).is_err());
    assert!(parse(&args("widgets add clock")).is_err());
    assert!(parse(&args("widgets remove twelve")).is_err());
    assert!(parse(&args("widgets remove -1")).is_err());
    assert_eq!(
        parse(&args("widgets settings 12")).unwrap().widgets,
        Some(WidgetAction::Settings(12))
    );
    assert!(parse(&args("widgets settings")).is_err());
    assert!(parse(&args("widgets settings 0")).is_err());
    assert!(dsa_cli::USAGE.contains("settings <id>"));
    assert_eq!(parse(&args("status")).unwrap().widgets, None);
}

#[test]
fn fr12_widgets_watch_prints_one_json_line_per_change_with_the_app_name() {
    use dsa_host_core::widgets::{Placement, Widget, WidgetEvent};
    let widget = Widget {
        widget_id: 3,
        provider: dsa_host_core::host_api_conformance::sample_widget_provider(),
        placement: Placement {
            x: 64,
            y: 64,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        },
    };
    let first: serde_json::Value = serde_json::from_str(&dsa_cli::widgets_now_line(
        std::slice::from_ref(&widget),
        true,
    ))
    .unwrap();
    assert_eq!(first["event"], "widgets");
    assert_eq!(first["widgets"][0]["id"], 3);
    assert_eq!(first["widgets"][0]["app"], "Clock App");
    let placed: serde_json::Value = serde_json::from_str(
        &dsa_cli::widget_event_line(&WidgetEvent::Placed(widget.clone()), true).unwrap(),
    )
    .unwrap();
    assert_eq!(placed["event"], "placed");
    assert_eq!(placed["widget"]["label"], "Clock");
    let removed: serde_json::Value = serde_json::from_str(
        &dsa_cli::widget_event_line(&WidgetEvent::Removed { widget_id: 3 }, true).unwrap(),
    )
    .unwrap();
    assert_eq!(removed, serde_json::json!({ "event": "removed", "id": 3 }));
    assert_eq!(
        dsa_cli::widget_event_line(&WidgetEvent::Listed(vec![3]), true),
        None
    );
    assert_eq!(
        dsa_cli::widget_event_line(&WidgetEvent::Removed { widget_id: 3 }, false).unwrap(),
        "Removed widget 3"
    );
}

#[test]
fn fr12_previews_are_printed_in_base64() {
    assert_eq!(dsa_cli::base64(b""), "");
    assert_eq!(dsa_cli::base64(b"f"), "Zg==");
    assert_eq!(dsa_cli::base64(b"fo"), "Zm8=");
    assert_eq!(dsa_cli::base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(dsa_cli::base64(&[0x89, b'P', b'N', b'G']), "iVBORw==");
}

fn offer_clock(androids: &FakeAndroids, user: &str) {
    use dsa_host_core::host_api::UserAndroids;
    let _ = androids.android(user).unwrap();
    androids
        .of(user)
        .unwrap()
        .widget_offer
        .lock()
        .unwrap()
        .push(dsa_host_core::host_api_conformance::sample_widget_provider());
}

#[test]
fn fr12_dsa_widgets_lists_adds_and_removes_widgets() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    offer_clock(&androids, &user);

    let output = dsa(&endpoint, "widgets list");
    assert!(output.status.success(), "{}", stderr(&output));
    let listed = stdout(&output);
    assert!(
        listed.contains("com.example.clock/.ClockWidget"),
        "{listed}"
    );
    assert!(listed.contains("No widget is placed."), "{listed}");
    assert_eq!(
        androids.of(&user).unwrap().state(),
        SessionState::Active,
        "Android was started first"
    );

    let output = dsa(&endpoint, "widgets add com.example.clock/.ClockWidget");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Placed Clock (widget 1)");

    let json = json_of(&dsa(&endpoint, "widgets list --json"));
    assert_eq!(
        json["providers"][0]["provider"],
        "com.example.clock/.ClockWidget"
    );
    assert_eq!(json["providers"][0]["preview_png"], "iVBORw==");
    assert_eq!(json["providers"][0]["default_width_dp"], 180);
    assert_eq!(json["widgets"][0]["id"], 1);
    assert_eq!(json["widgets"][0]["placement"]["width_dp"], 180);

    let output = dsa(&endpoint, "widgets remove 1");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Removed widget 1");
    let json = json_of(&dsa(&endpoint, "widgets list --json"));
    assert_eq!(json["widgets"].as_array().unwrap().len(), 0);
}

#[test]
fn fr12_dsa_widgets_says_why_a_widget_was_not_placed() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    offer_clock(&androids, &user);
    let output = dsa(&endpoint, "widgets add com.example.other/.Nothing");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("the widget was not placed"),
        "{}",
        stderr(&output)
    );
    let output = dsa(&endpoint, "widgets remove 77");
    assert_eq!(output.status.code(), Some(1));
}

fn identity_service(listener: Box<dyn dsa_transport::local::LocalListener>) -> RunningService {
    identity_service_on(listener, None)
}

fn identity_service_on(
    listener: Box<dyn dsa_transport::local::LocalListener>,
    computer: Option<dsa_host_core::computer_identity::ComputerInfo>,
) -> RunningService {
    use dsa_host_core::device_identity::{ImageBuild, MemoryDeviceIdentities};
    use dsa_host_core::host_api::HostApiService;
    let build = ImageBuild {
        release: "16".into(),
        id: "BP2A".into(),
        incremental: "1.dsaab".into(),
        tags: "release-keys".into(),
        arch: "x86_64".into(),
    };
    let mut service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_device_identities(
            Arc::new(MemoryDeviceIdentities::default()),
            Arc::new(move |_: &str| Some(build.clone())),
        );
    if let Some(computer) = computer {
        service = service.with_computer_identity(Arc::new(move || computer.clone()));
    }
    RunningService::start_service(listener, service)
}

#[test]
fn fr17_dsa_shows_the_computers_identity_and_where_each_default_came_from() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = identity_service_on(
        listener,
        Some(dsa_host_core::computer_identity::ComputerInfo {
            system_manufacturer: "SAMSUNG ELECTRONICS CO., LTD.".into(),
            system_product: "960XGL".into(),
            system_family: "Galaxy Book4 Ultra".into(),
            system_version: "P10ALX".into(),
            baseboard_product: "NT960XGP-G72AG".into(),
            computer_name: "BREW-BOOK".into(),
            cpu_vendor: "GenuineIntel".into(),
            cpu_name: "Intel(R) Core(TM) Ultra 7 155H".into(),
        }),
    );
    let shown = json_of(&dsa(&endpoint, "settings device --json"));
    assert_eq!(shown["effective"]["manufacturer"], "Samsung");
    assert_eq!(shown["effective"]["model"], "NT960XGP-G72AG");
    assert_eq!(shown["effective"]["model_name"], "Galaxy Book4 Ultra");
    assert_eq!(shown["effective"]["device_name"], "BREW-BOOK");
    assert_eq!(shown["effective"]["soc_manufacturer"], "Intel");
    assert_eq!(shown["effective"]["soc_model"], "Core Ultra 7 155H");
    assert_eq!(shown["sources"]["model"], "computer");
    assert_eq!(shown["sources"]["device_name"], "computer");
    assert_eq!(shown["sources"]["device"], "dsa");
    assert_eq!(shown["sources"]["fingerprint_type"], "dsa");

    let changed = json_of(&dsa(&endpoint, "settings set device.model Tab --json"));
    assert_eq!(changed["effective"]["model"], "Tab");
    assert_eq!(changed["defaults"]["model"], "NT960XGP-G72AG");
    let reset = json_of(&dsa(&endpoint, "settings reset device.model --json"));
    assert_eq!(reset["effective"]["model"], "NT960XGP-G72AG");

    let text = stdout(&dsa(&endpoint, "settings device"));
    assert!(
        text.contains("model: NT960XGP-G72AG (from this computer)"),
        "{text}"
    );
    assert!(text.contains("device: dsa_x86_64 (DSA's value)"), "{text}");
    assert!(
        text.contains("device_name: BREW-BOOK (from this computer)"),
        "{text}"
    );
}

fn dsa_words(endpoint: &str, words: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dsa"))
        .args(words)
        .args([endpoint_flag(), endpoint])
        .output()
        .expect("running dsa")
}

#[test]
fn fr17_dsa_settings_device_fields_are_parsed() {
    use dsa_cli::Change;
    use dsa_host_core::device_identity::Field;
    let words = ["settings", "set", "device.model", "My tablet"].map(String::from);
    assert_eq!(
        parse(&words).unwrap().change,
        Some(Change::Device(Field::Model, "My tablet".into()))
    );
    assert_eq!(
        parse(&args("settings set device.fingerprint.type userdebug"))
            .unwrap()
            .change,
        Some(Change::Device(Field::FingerprintType, "userdebug".into()))
    );
    assert_eq!(
        parse(&args("settings reset device")).unwrap().change,
        Some(Change::ResetDevice(None))
    );
    assert_eq!(
        parse(&args("settings reset device.brand")).unwrap().change,
        Some(Change::ResetDevice(Some(Field::Brand)))
    );
    assert_eq!(
        parse(&args("settings device --json")).unwrap().change,
        Some(Change::ShowDevice)
    );
    assert!(parse(&args("settings set device.serial X")).is_err());
    assert!(parse(&args("settings set device.brand two/parts")).is_err());
    assert!(parse(&args("settings reset gpu")).is_err());
}

#[test]
fn fr17_dsa_changes_and_resets_the_device_identity_through_the_service() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = identity_service(listener);
    let shown = json_of(&dsa(&endpoint, "settings device --json"));
    assert_eq!(shown["effective"]["model"], "Desktop Subsystem for Android");
    assert_eq!(shown["effective"]["manufacturer"], "thisisthepy");
    assert_eq!(shown["chosen"]["model"], "");

    let changed = json_of(&dsa_words(
        &endpoint,
        &["settings", "set", "device.model", "My tablet", "--json"],
    ));
    assert_eq!(changed["chosen"]["model"], "My tablet");
    assert_eq!(changed["effective"]["model"], "My tablet");
    let changed = json_of(&dsa(&endpoint, "settings set device.brand mine --json"));
    assert_eq!(
        changed["effective"]["model"], "My tablet",
        "one change keeps the others"
    );
    assert_eq!(
        changed["effective"]["fingerprint"],
        "mine/dsa_x86_64/dsa_x86_64:16/BP2A/1.dsaab:user/release-keys"
    );

    let refused = dsa(&endpoint, "settings set device.fingerprint not-one");
    assert_eq!(refused.status.code(), Some(2), "refused before it is sent");
    assert!(
        stderr(&refused).contains("fingerprint"),
        "{}",
        stderr(&refused)
    );

    let reset = json_of(&dsa(&endpoint, "settings reset device.brand --json"));
    assert_eq!(reset["effective"]["brand"], "DSA");
    assert_eq!(reset["chosen"]["model"], "My tablet");
    let reset = json_of(&dsa(&endpoint, "settings reset device --json"));
    assert_eq!(reset["effective"]["model"], "Desktop Subsystem for Android");

    let text = dsa(&endpoint, "settings device");
    assert!(text.status.success(), "{}", stderr(&text));
    assert!(stdout(&text).contains("model: Desktop Subsystem for Android"));
    assert!(stdout(&text).contains("next start"));
}

#[test]
fn fr23_dsa_apps_is_parsed() {
    use dsa_cli::AppsAction;
    assert_eq!(parse(&args("apps")).unwrap().apps, Some(AppsAction::List));
    assert_eq!(
        parse(&args("apps list --json")).unwrap().apps,
        Some(AppsAction::List)
    );
    assert_eq!(
        parse(&args("apps info com.example.notes")).unwrap().apps,
        Some(AppsAction::Info("com.example.notes".into()))
    );
    assert_eq!(
        parse(&args("apps uninstall com.example.notes"))
            .unwrap()
            .apps,
        Some(AppsAction::Uninstall("com.example.notes".into()))
    );
    assert_eq!(
        parse(&args("apps settings")).unwrap().apps,
        Some(AppsAction::Settings(None))
    );
    assert_eq!(
        parse(&args("apps settings com.example.notes"))
            .unwrap()
            .apps,
        Some(AppsAction::Settings(Some("com.example.notes".into())))
    );
    assert_eq!(parse(&args("apps")).unwrap().command, Cmd::Apps);
    assert!(parse(&args("apps uninstall"))
        .unwrap_err()
        .contains("needs the app's package"));
    assert!(parse(&args("apps uninstall ../evil"))
        .unwrap_err()
        .contains("not a package name"));
}

#[test]
fn fr23_dsa_apps_list_and_info_show_each_app_with_its_details() {
    use dsa_host_core::host_api_conformance::{KNOWN_PACKAGE, SYSTEM_PACKAGE};
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let list = dsa(&endpoint, "apps list");
    assert!(list.status.success(), "{}", stderr(&list));
    let text = stdout(&list);
    assert!(
        text.contains("com.example.notes  Notes 4.1  2.9 MB"),
        "{text}"
    );
    assert!(text.contains("com.android.settings  Settings 16"), "{text}");
    assert!(text.contains("(system)"), "{text}");
    let android = androids.of(&user).unwrap();
    assert_eq!(
        android.state(),
        SessionState::Active,
        "Android was started first"
    );
    assert!(
        *android.app_refreshes.lock().unwrap() >= 1,
        "sizes were read again"
    );

    let json = json_of(&dsa(&endpoint, "apps list --json"));
    let apps = json["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 2);
    let notes = apps
        .iter()
        .find(|app| app["package"] == KNOWN_PACKAGE)
        .unwrap();
    assert_eq!(notes["label"], "Notes");
    assert_eq!(notes["version_name"], "4.1");
    assert_eq!(notes["version_code"], 41);
    assert_eq!(notes["first_install_time_ms"], 1_700_000_000_000u64);
    assert_eq!(notes["system_app"], false);
    assert_eq!(notes["total_bytes"], 3_021_504);
    assert_eq!(notes["icon_png"], "iVBORwI=");
    let settings = apps
        .iter()
        .find(|app| app["package"] == SYSTEM_PACKAGE)
        .unwrap();
    assert_eq!(settings["system_app"], true);

    let info = dsa(&endpoint, "apps info com.example.notes");
    assert!(info.status.success(), "{}", stderr(&info));
    let text = stdout(&info);
    assert!(text.starts_with("Notes (com.example.notes)"), "{text}");
    assert!(text.contains("Version: 4.1 (41)"), "{text}");
    assert!(text.contains("Installed: 2023-11-14"), "{text}");
    let missing = dsa(&endpoint, "apps info com.example.missing");
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("no app of com.example.missing"));
}

#[test]
fn fr23_dsa_apps_uninstall_removes_a_user_app_and_refuses_a_system_app() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));

    let removed = dsa(&endpoint, "apps uninstall com.example.notes");
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert_eq!(stdout(&removed), "Uninstalled com.example.notes");
    let json = json_of(&dsa(&endpoint, "apps list --json"));
    assert_eq!(json["apps"].as_array().unwrap().len(), 1, "the app is gone");

    let system = dsa(&endpoint, "apps uninstall com.android.settings");
    assert_eq!(system.status.code(), Some(1));
    assert!(
        stderr(&system).contains("not uninstalled (system-app)"),
        "{}",
        stderr(&system)
    );
    let gone = dsa(&endpoint, "apps uninstall com.example.notes");
    assert!(
        stderr(&gone).contains("not uninstalled (not-installed)"),
        "{}",
        stderr(&gone)
    );
}

#[test]
fn fr25_dsa_apps_run_as_arm_is_parsed() {
    use dsa_cli::AppsAction;
    let line = args("apps run-as-arm com.example.game on");
    let on = parse(&line).unwrap();
    assert_eq!(
        on.apps,
        Some(AppsAction::RunAsArm("com.example.game".into(), true))
    );
    assert!(!on.erase_data);
    let line = args("apps run-as-arm com.example.game off --erase-data");
    let erase = parse(&line).unwrap();
    assert_eq!(
        erase.apps,
        Some(AppsAction::RunAsArm("com.example.game".into(), false))
    );
    assert!(erase.erase_data);
    assert!(parse(&args("apps run-as-arm com.example.game")).is_err());
    assert!(parse(&args("apps run-as-arm com.example.game maybe")).is_err());
    assert!(parse(&args("apps run-as-arm not/a/package on")).is_err());
}

#[test]
fn fr7_dsa_apps_shortcut_and_desktop_shortcut_are_parsed() {
    use dsa_cli::AppsAction;
    assert_eq!(
        parse(&args("apps shortcut com.example.chat new-chat"))
            .unwrap()
            .apps,
        Some(AppsAction::Shortcut(
            "com.example.chat".into(),
            "new-chat".into()
        ))
    );
    assert_eq!(
        parse(&args("apps desktop-shortcut com.example.chat --json"))
            .unwrap()
            .apps,
        Some(AppsAction::DesktopShortcut("com.example.chat".into()))
    );
    assert!(parse(&args("apps shortcut com.example.chat")).is_err());
    assert!(parse(&args("apps shortcut not/a/package new-chat")).is_err());
    assert!(parse(&args("apps desktop-shortcut")).is_err());
}

#[test]
fn fr7_dsa_apps_shortcut_starts_the_shortcut_in_the_users_android() {
    use dsa_host_core::tasks::TaskRequest;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let output = dsa(&endpoint, "apps shortcut com.example.chat new-chat --json");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("\"started\":true"),
        "{}",
        stdout(&output)
    );
    assert!(androids
        .of(&user)
        .unwrap()
        .requests
        .lock()
        .unwrap()
        .contains(&TaskRequest::LaunchShortcut {
            package_name: "com.example.chat".into(),
            shortcut_id: "new-chat".into(),
        }));
}

#[test]
fn fr7_dsa_apps_lists_each_apps_shortcuts() {
    use dsa_host_core::tasks::{AppInfo, AppShortcut};
    let app = AppInfo {
        package_name: "com.example.chat".into(),
        label: "Chat".into(),
        shortcuts: vec![AppShortcut {
            id: "new-chat".into(),
            short_label: "New chat".into(),
            dynamic: true,
            icon_png: vec![1, 2, 3],
            ..Default::default()
        }],
        ..Default::default()
    };
    let listed = dsa_cli::app_json(&app, true);
    assert_eq!(listed["shortcuts"][0]["id"], "new-chat");
    assert_eq!(listed["shortcuts"][0]["label"], "New chat");
    assert_eq!(listed["shortcuts"][0]["in_jump_list"], true);
    assert_eq!(listed["shortcuts"][0]["pinned"], false);
    assert_eq!(listed["shortcuts"][0]["icon_png"], "AQID");
    assert!(dsa_cli::app_json(&app, false)["shortcuts"][0]
        .get("icon_png")
        .is_none());
}

#[test]
fn fr25_dsa_apps_run_as_arm_asks_before_erasing_data_and_lists_the_switch() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let off = dsa(&endpoint, "apps run-as-arm com.example.notes on");
    assert_eq!(off.status.code(), Some(1));
    assert!(
        stderr(&off).contains("not changed (translation-off)"),
        "{}",
        stderr(&off)
    );
    *androids.of(&user).unwrap().translating.lock().unwrap() = true;
    let asks = dsa(&endpoint, "apps run-as-arm com.example.notes on");
    assert_eq!(asks.status.code(), Some(1));
    assert!(
        stderr(&asks).contains("not changed (data-would-be-erased)"),
        "{}",
        stderr(&asks)
    );
    let done = dsa(
        &endpoint,
        "apps run-as-arm com.example.notes on --erase-data --json",
    );
    assert!(done.status.success(), "{}", stderr(&done));
    let json = json_of(&done);
    assert_eq!(json["status"], "done");
    assert_eq!(json["on"], true);

    // The sample apps have no native code, so their switch cannot be changed.
    let list = json_of(&dsa(&endpoint, "apps list --json"));
    let notes = list["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|app| app["package"] == "com.example.notes")
        .expect("the notes app")
        .clone();
    assert_eq!(notes["run_as_arm"]["available"], false);
    assert_eq!(notes["run_as_arm"]["reason"], "no-arm-libraries");
    assert_eq!(notes["native_abis"], serde_json::json!([]));
}

#[test]
fn fr23_dsa_apps_settings_opens_androids_app_page_or_its_list_of_apps() {
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let one = dsa(&endpoint, "apps settings com.example.notes");
    assert!(one.status.success(), "{}", stderr(&one));
    assert_eq!(stdout(&one), "Opening the settings of com.example.notes");
    let all = dsa(&endpoint, "apps settings");
    assert!(all.status.success(), "{}", stderr(&all));
    assert_eq!(
        *androids.of(&user).unwrap().app_settings.lock().unwrap(),
        vec![Some("com.example.notes".to_string()), None]
    );
}

#[test]
fn fr24_dsa_background_is_parsed() {
    use dsa_cli::BackgroundAction;
    assert_eq!(
        parse(&args("background")).unwrap().background,
        Some(BackgroundAction::List)
    );
    assert_eq!(
        parse(&args("background list --json")).unwrap().background,
        Some(BackgroundAction::List)
    );
    assert_eq!(
        parse(&args("background stop com.example.music"))
            .unwrap()
            .background,
        Some(BackgroundAction::Stop("com.example.music".into()))
    );
    assert_eq!(
        parse(&args("background open com.example.music"))
            .unwrap()
            .background,
        Some(BackgroundAction::Open("com.example.music".into()))
    );
    assert!(parse(&args("background stop")).is_err());
    assert!(parse(&args("background stop ../music")).is_err());
    assert_eq!(parse(&args("status")).unwrap().background, None);
    assert!(parse(&args("settings set stop-without-asking on")).is_ok());
    assert!(parse(&args("settings set stop-without-asking maybe")).is_err());
}

#[test]
fn fr24_dsa_background_lists_and_stops_the_apps_that_run_in_the_background() {
    use dsa_host_core::background::{BackgroundApp, BackgroundCommand, BackgroundEvent};
    use dsa_host_core::host_api::UserAndroids;
    use dsa_host_core::session::BackgroundRequest;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());

    let json = json_of(&dsa(&endpoint, "background --json"));
    assert_eq!(json["apps"].as_array().unwrap().len(), 0);
    let output = dsa(&endpoint, "background");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "No app runs in the background.");

    let _ = androids.android(&user).unwrap();
    let android = androids.of(&user).unwrap();
    android.background_event(BackgroundEvent::App(BackgroundApp {
        package_name: "com.example.music".into(),
        app_label: "Music".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
        title: "Playing".into(),
        text: "Song".into(),
        actions: vec!["Pause".into()],
    }));
    let json = json_of(&dsa(&endpoint, "background list --json"));
    assert_eq!(json["apps"][0]["package"], "com.example.music");
    assert_eq!(json["apps"][0]["label"], "Music");
    assert_eq!(json["apps"][0]["title"], "Playing");
    assert_eq!(json["apps"][0]["actions"][0], "Pause");
    assert_eq!(json["apps"][0]["icon_png"], "iVBORw==");

    let output = dsa(&endpoint, "start --wait --timeout 5");
    assert!(output.status.success(), "{}", stderr(&output));
    let json = json_of(&dsa(&endpoint, "background stop com.example.music --json"));
    assert_eq!(json["stopped"], "com.example.music");
    assert_eq!(
        *android.background_requests.lock().unwrap(),
        vec![BackgroundRequest::App(
            "com.example.music".into(),
            BackgroundCommand::Stop
        )]
    );
}

#[test]
fn fr24_stopping_without_asking_is_a_setting_off_by_default() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["stop_without_asking"], false);
    let changed = json_of(&dsa(
        &endpoint,
        "settings set stop-without-asking on --json",
    ));
    assert_eq!(changed["stop_without_asking"], true);
}

#[test]
fn fr30_dsa_app_window_is_parsed() {
    use dsa_cli::AppWindowAction;
    let list = parse(&args("app-window --json")).unwrap();
    assert_eq!(list.command, Cmd::AppWindow);
    assert_eq!(list.app_window, Some(AppWindowAction::List));
    assert_eq!(
        parse(&args("app-window com.example.notes"))
            .unwrap()
            .app_window,
        Some(AppWindowAction::Show("com.example.notes".into()))
    );
    assert_eq!(
        parse(&args(
            "app-window com.example.notes --fixed-size on --start-size 400x700"
        ))
        .unwrap()
        .app_window,
        Some(AppWindowAction::Set {
            package: "com.example.notes".into(),
            fixed_size: Some(true),
            start_size: Some(Some((400, 700))),
            dpi_scaling: None,
        })
    );
    assert_eq!(
        parse(&args("app-window com.example.notes --start-size auto"))
            .unwrap()
            .app_window,
        Some(AppWindowAction::Set {
            package: "com.example.notes".into(),
            fixed_size: None,
            start_size: Some(None),
            dpi_scaling: None,
        })
    );
    assert_eq!(
        parse(&args("app-window com.example.notes reset"))
            .unwrap()
            .app_window,
        Some(AppWindowAction::Reset("com.example.notes".into()))
    );
    assert!(parse(&args("app-window com.example.notes --fixed-size maybe")).is_err());
    assert!(parse(&args("app-window com.example.notes --start-size big")).is_err());
    assert!(parse(&args("app-window --fixed-size on")).is_err());
}

#[test]
fn fr30_dsa_app_window_changes_and_resets_an_apps_window_settings() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let none = json_of(&dsa(&endpoint, "app-window --json"));
    assert_eq!(none["apps"], serde_json::json!([]));
    let set = json_of(&dsa(
        &endpoint,
        "app-window com.example.notes --fixed-size on --start-size 400x700 --json",
    ));
    assert_eq!(set["apps"][0]["package"], "com.example.notes");
    assert_eq!(set["apps"][0]["fixed_size"], true);
    assert_eq!(set["apps"][0]["start_size"], serde_json::json!([400, 700]));
    // Changing one setting keeps the other.
    let resizable = json_of(&dsa(
        &endpoint,
        "app-window com.example.notes --fixed-size off --json",
    ));
    assert_eq!(resizable["apps"][0]["fixed_size"], false);
    assert_eq!(
        resizable["apps"][0]["start_size"],
        serde_json::json!([400, 700])
    );
    let listed = json_of(&dsa(&endpoint, "app-window --json"));
    assert_eq!(listed["apps"].as_array().unwrap().len(), 1);
    let output = dsa(&endpoint, "app-window com.example.notes");
    assert_eq!(
        stdout(&output).trim(),
        "com.example.notes: resizable, opens at 400 x 700 dp, scales its own way on another \
         monitor"
    );
    let reset = json_of(&dsa(&endpoint, "app-window com.example.notes reset --json"));
    assert_eq!(reset["apps"][0]["start_size"], serde_json::Value::Null);
    let none = json_of(&dsa(&endpoint, "app-window --json"));
    assert_eq!(none["apps"], serde_json::json!([]));
    let refused = dsa(&endpoint, "app-window com.example.notes --start-size 10x10");
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("start size"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn fr4_dsa_app_window_dpi_scaling_is_parsed() {
    use dsa_cli::AppWindowAction;
    use dsa_host_core::app_windows::DpiScaling;
    for (name, scaling) in [
        ("app", DpiScaling::App),
        ("system", DpiScaling::System),
        ("system-forced", DpiScaling::SystemForced),
        ("AUTO", DpiScaling::Auto),
    ] {
        assert_eq!(
            parse(&args(&format!(
                "app-window com.example.notes --dpi-scaling {name}"
            )))
            .unwrap()
            .app_window,
            Some(AppWindowAction::Set {
                package: "com.example.notes".into(),
                fixed_size: None,
                start_size: None,
                dpi_scaling: Some(scaling),
            })
        );
    }
    // With the other settings in one call, as the settings apps send it.
    assert_eq!(
        parse(&args(
            "app-window com.example.notes --fixed-size off --start-size auto --dpi-scaling system"
        ))
        .unwrap()
        .app_window,
        Some(AppWindowAction::Set {
            package: "com.example.notes".into(),
            fixed_size: Some(false),
            start_size: Some(None),
            dpi_scaling: Some(DpiScaling::System),
        })
    );
    assert!(parse(&args("app-window com.example.notes --dpi-scaling enhanced")).is_err());
    assert!(parse(&args("app-window com.example.notes --dpi-scaling")).is_err());
    assert!(parse(&args("app-window --dpi-scaling app")).is_err());
}

#[test]
fn fr4_dsa_app_window_sets_and_keeps_an_apps_dpi_scaling() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let none = json_of(&dsa(&endpoint, "app-window --json"));
    // The forced way is stored but does not do what it says yet.
    assert_eq!(none["system_forced_available"], false);
    let set = json_of(&dsa(
        &endpoint,
        "app-window com.example.notes --dpi-scaling system --json",
    ));
    assert_eq!(set["apps"][0]["dpi_scaling"], "system");
    // Changing another setting keeps it.
    let fixed = json_of(&dsa(
        &endpoint,
        "app-window com.example.notes --fixed-size on --json",
    ));
    assert_eq!(fixed["apps"][0]["dpi_scaling"], "system");
    assert_eq!(fixed["apps"][0]["fixed_size"], true);
    let output = dsa(&endpoint, "app-window com.example.notes");
    assert_eq!(
        stdout(&output).trim(),
        "com.example.notes: fixed size, opens at the size Android gives it, is scaled by DSA on \
         another monitor"
    );
    let forced = dsa(
        &endpoint,
        "app-window com.example.notes --dpi-scaling system-forced",
    );
    assert!(
        stdout(&forced).contains("not available yet"),
        "{}",
        stdout(&forced)
    );
    // Back to auto and resizable is the defaults: the entry goes.
    let auto = json_of(&dsa(
        &endpoint,
        "app-window com.example.notes --dpi-scaling auto --fixed-size off --json",
    ));
    assert_eq!(auto["apps"][0]["dpi_scaling"], "auto");
    let none = json_of(&dsa(&endpoint, "app-window --json"));
    assert_eq!(none["apps"], serde_json::json!([]));
}

#[test]
fn fr31_there_is_no_run_mode_setting_to_stop_an_idle_android() {
    for name in [
        "run-mode continuous",
        "stop-after 5m",
        "keep-for-notifications on",
    ] {
        assert!(
            parse(&args(&format!("settings set {name}"))).is_err(),
            "{name}"
        );
    }
}

#[test]
fn fr31_dsa_reset_data_says_what_is_lost_and_resets_only_with_yes() {
    use dsa_host_core::data_reset::TemplateDataDisk;
    use dsa_host_core::host_api::HostApiService;
    let ((listener, _, user), endpoint) = local_pair();
    let dir = std::env::temp_dir().join(format!("dsa-cli-reset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let disk = dir.join(format!("{}.vhdx", user.replace([':', '\\', '/'], "_")));
    std::fs::write(&disk, b"old data").unwrap();
    let reset = {
        let disk = disk.clone();
        TemplateDataDisk::new(
            Box::new(move |_: &str| Ok(disk.clone())),
            Box::new(|fresh: &std::path::Path| {
                std::fs::write(fresh, b"fresh").map_err(|e| e.to_string())
            }),
        )
    };
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_data_reset(Arc::new(reset));
    let _service = RunningService::start_service(listener, service);

    let asked = dsa(&endpoint, "reset-data");
    assert!(!asked.status.success());
    let warning = stderr(&asked);
    assert!(warning.contains("every app you installed"), "{warning}");
    assert!(warning.contains("accounts"), "{warning}");
    assert!(warning.contains("--yes"), "{warning}");
    assert_eq!(
        std::fs::read(&disk).unwrap(),
        b"old data",
        "nothing was reset"
    );

    let json = json_of(&dsa(&endpoint, "reset-data --yes --json"));
    assert_eq!(json["reset"], true);
    assert_eq!(std::fs::read(&disk).unwrap(), b"fresh");
    assert_eq!(
        parse(&args("reset-data --yes")).unwrap().command,
        Cmd::ResetData
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr29_android_files_open_as_a_dav_location_on_linux() {
    assert_eq!(
        dsa_cli::dav_location("http://127.0.0.1:50000/token/"),
        "dav://127.0.0.1:50000/token/"
    );
    assert_eq!(dsa_cli::dav_location("dav://a/"), "dav://a/");
    #[cfg(target_os = "linux")]
    assert_eq!(
        dsa_cli::files_path("http://127.0.0.1:50000/token/"),
        "dav://127.0.0.1:50000/token/"
    );
}

#[test]
fn fr15_settings_say_where_program_updates_come_from() {
    use dsa_host_core::host_api::{ServiceFacts, SettingsReport};
    let report = SettingsReport {
        settings: dsa_host_core::settings::Settings::default(),
        facts: ServiceFacts {
            version: "0.0.9".into(),
            log_dir: None,
            program_updates_available: false,
            update_channel_available: false,
            image_updates_available: true,
            android_version: None,
        },
        network: None,
        adb_auto_connect_state: Default::default(),
        touch_gestures: Default::default(),
        adb_shell_root: String::new(),
        imported_certificates: Vec::new(),
    };
    let text = dsa_cli::describe_settings(&report);
    let line = text
        .lines()
        .find(|line| line.starts_with("Program updates:"))
        .expect("a program updates line");
    if cfg!(target_os = "linux") {
        assert!(line.contains("package manager"), "{line}");
    } else {
        assert!(line.contains("does not update it"), "{line}");
    }
}

/// Android versions kept in memory: 17 runs, and 16 can be chosen.
#[derive(Default)]
struct FakeVersions(std::sync::Mutex<dsa_host_core::update::versions::Channel>);

impl dsa_host_core::update::versions::AndroidVersions for FakeVersions {
    fn needs_administrator(&self) -> bool {
        false
    }
    fn report(&self) -> Result<dsa_host_core::update::versions::VersionReport, String> {
        use dsa_host_core::update::versions::{target_major, VersionReport};
        let channel = *self.0.lock().unwrap();
        Ok(VersionReport {
            version: "17.0.0-16373615-8".into(),
            android_major: 17,
            build: 1637361508,
            build_id: Some("16373615".into()),
            security_patch: Some("2026-06-05".into()),
            channel,
            supported_majors: vec![16, 17],
            target_major: target_major(channel, &[16, 17]).unwrap(),
            on_trial: false,
            notice: None,
            restart_pending: None,
        })
    }
    fn choose(
        &self,
        channel: dsa_host_core::update::versions::Channel,
        erase_data: bool,
    ) -> Result<
        dsa_host_core::update::versions::VersionReport,
        dsa_host_core::update::versions::ChooseError,
    > {
        use dsa_host_core::update::versions::{Channel, ChooseError};
        if channel == Channel::Major(16) && !erase_data {
            return Err(ChooseError::DataWouldBeLost("16 is older".into()));
        }
        *self.0.lock().unwrap() = channel;
        self.report().map_err(ChooseError::Store)
    }
}

#[test]
fn fr14_dsa_android_shows_the_version_and_moves_down_only_with_erase_data() {
    use dsa_host_core::host_api::HostApiService;
    use dsa_host_core::update::versions::Channel;
    let ((listener, _, _), endpoint) = local_pair();
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_android_versions(Arc::new(FakeVersions::default()));
    let _service = RunningService::start_service(listener, service);

    let shown = stdout(&dsa(&endpoint, "android version"));
    assert!(shown.contains("Android 17 (17.0.0-16373615-8)"), "{shown}");
    assert!(shown.contains("security patch 2026-06-05"), "{shown}");
    assert!(shown.contains("Supported: Android 16, 17"), "{shown}");
    let json = json_of(&dsa(&endpoint, "android --json"));
    assert_eq!(json["android_major"], 17);
    assert_eq!(json["channel"], "latest");

    let refused = dsa(&endpoint, "android use 16");
    assert!(!refused.status.success());
    let warning = stderr(&refused);
    assert!(warning.contains("accounts"), "{warning}");
    assert!(warning.contains("--erase-data"), "{warning}");
    let moved = json_of(&dsa(&endpoint, "android use 16 --erase-data --json"));
    assert_eq!(moved["channel"], "16");
    assert_eq!(moved["target_major"], 16);

    let parsed = parse(&args("android use latest")).unwrap();
    assert_eq!(parsed.command, Cmd::AndroidUse);
    assert_eq!(parsed.android, Some(Channel::Latest));
    assert!(parse(&args("android use seventeen")).is_err());
    assert!(parse(&args("android use")).is_err());
}

#[test]
fn fr14_dsa_android_install_image_takes_the_folder_and_refuses_one_without_a_list() {
    let parsed = parse(&args("android install-image dev-build")).unwrap();
    assert_eq!(parsed.command, Cmd::AndroidInstallImage);
    assert_eq!(
        parsed.file.as_deref(),
        Some(std::path::Path::new("dev-build"))
    );
    let why = parse(&args("android install-image")).unwrap_err();
    assert!(why.contains("folder"), "{why}");
    assert!(parse(&args("android install-image --json")).is_err());

    // A folder without a signed list is refused before anything runs, and no service is
    // needed to say so.
    let empty = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("install-image-empty-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dsa"))
        .arg("android")
        .arg("install-image")
        .arg(&empty)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("images.json"), "{said}");
}

#[test]
fn fr14_a_downloaded_image_is_applied_without_the_user_restarting() {
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::MemorySettings;
    let ((listener, _, user), endpoint) = local_pair();
    let androids = Arc::new(FakeAndroids::default());
    let service = HostApiService::new(androids.clone(), "test service")
        .with_android_versions(Arc::new(FakeVersions::default()))
        .with_settings(
            Arc::new(MemorySettings::default()),
            ServiceFacts {
                version: "0.0.9".into(),
                log_dir: None,
                program_updates_available: true,
                update_channel_available: true,
                image_updates_available: true,
                android_version: None,
            },
        );
    let _service = RunningService::start_service(listener, service);

    // The setting is on by default, and the user's Android follows a change at once.
    assert_eq!(
        json_of(&dsa(&endpoint, "settings --json"))["image_auto_restart"],
        true
    );
    let shown = stdout(&dsa(&endpoint, "settings"));
    assert!(
        shown.contains("Restart into a downloaded Android image by itself: on"),
        "{shown}"
    );
    let changed = json_of(&dsa(
        &endpoint,
        "settings set image-auto-restart off --json",
    ));
    assert_eq!(changed["image_auto_restart"], false);
    let android = androids.of(&user).unwrap();
    assert_eq!(*android.image_auto_restart.lock().unwrap(), [false]);
    assert!(parse(&args("settings set image-auto-restart maybe")).is_err());

    // Nothing waits: `dsa android restart` says so and restarts nothing.
    let json = json_of(&dsa(&endpoint, "android --json"));
    assert_eq!(json["restart_pending"], serde_json::Value::Null);
    let nothing = json_of(&dsa(&endpoint, "android restart --json"));
    assert_eq!(nothing["restarting"], serde_json::Value::Null);
    assert!(android.restarted_into.lock().unwrap().is_empty());

    // An image waits: the version report says which, and the restart applies it.
    *android.restart_pending.lock().unwrap() = Some("17.0.0-16373615-9".into());
    let json = json_of(&dsa(&endpoint, "android version --json"));
    assert_eq!(json["restart_pending"], "17.0.0-16373615-9");
    let shown = stdout(&dsa(&endpoint, "android version"));
    assert!(shown.contains("dsa android restart"), "{shown}");
    let restart = dsa(&endpoint, "android restart");
    assert!(restart.status.success(), "{}", stderr(&restart));
    assert!(
        stdout(&restart).contains("17.0.0-16373615-9"),
        "{}",
        stdout(&restart)
    );
    assert_eq!(
        *android.restarted_into.lock().unwrap(),
        ["17.0.0-16373615-9"]
    );
    assert_eq!(
        parse(&args("android restart")).unwrap().command,
        Cmd::AndroidRestart
    );
}

#[test]
fn fr36_dsa_backup_commands_are_parsed() {
    assert_eq!(parse(&args("backup")).unwrap().command, Cmd::BackupStatus);
    assert_eq!(
        parse(&args("backup --json")).unwrap().command,
        Cmd::BackupStatus
    );
    assert_eq!(parse(&args("backup now")).unwrap().command, Cmd::BackupNow);
    assert_eq!(
        parse(&args("backup restore")).unwrap().command,
        Cmd::BackupRestore
    );
    assert_eq!(
        parse(&args("backup key show")).unwrap().command,
        Cmd::BackupKeyShow
    );
    let import = parse(&args("backup key import ABCDE FGHJK --json")).unwrap();
    assert_eq!(import.command, Cmd::BackupKeyImport);
    assert_eq!(import.backup_key.as_deref(), Some("ABCDE FGHJK"));
    assert!(import.json);
    assert!(parse(&args("backup key import")).is_err());
    assert!(parse(&args("backup key forget")).is_err());
    assert!(parse(&args("backup everything")).is_err());
    assert_eq!(parse(&args("backup on")).unwrap().command, Cmd::BackupOn);
    let off = parse(&args("backup off")).unwrap();
    assert_eq!(off.command, Cmd::BackupOff);
    assert!(!off.delete_backups, "the backups stay unless asked");
    let delete = parse(&args("backup off --delete-backups --json")).unwrap();
    assert_eq!(delete.command, Cmd::BackupOff);
    assert!(delete.delete_backups && delete.json);
    assert!(
        parse(&args("backup on --delete-backups")).is_err(),
        "deleting goes only with turning off"
    );
}

#[test]
fn fr36_dsa_backup_says_backup_is_off_and_offers_to_delete_what_is_kept() {
    let off = dsa_host_core::backup::BackupReport {
        folder: "C:\\Users\\ada\\DSA Backup".into(),
        ..Default::default()
    };
    let text = dsa_cli::describe_backup(Cmd::BackupStatus, &off);
    assert!(text.contains("Backup: off"), "{text}");
    assert!(text.contains("dsa backup on"), "{text}");
    assert!(text.contains("second copy"), "{text}");
    let kept = dsa_host_core::backup::BackupReport {
        packages: 4,
        size: 3 * 1024 * 1024,
        ..off.clone()
    };
    let text = dsa_cli::describe_backup(Cmd::BackupOff, &kept);
    assert!(text.contains("still kept"), "{text}");
    assert!(text.contains("--delete-backups"), "{text}");
    let text = dsa_cli::describe_backup(Cmd::BackupOff, &off);
    assert!(text.contains("no backups are kept"), "{text}");
    let on = dsa_host_core::backup::BackupReport {
        enabled: true,
        available: true,
        ..off
    };
    assert!(dsa_cli::describe_backup(Cmd::BackupOn, &on).contains("Backup is on"));
    assert_eq!(dsa_cli::backup_json(&on, None)["enabled"], true);
}

#[test]
fn fr36_dsa_status_says_what_seals_the_keymint_root_key() {
    use dsa_host_core::host_keys::KeyProtection;
    use dsa_host_core::session::KeyMintKeyState;
    let tpm = KeyMintKeyState {
        protection: Some(KeyProtection {
            sealed_by: "tpm".into(),
            hardware: true,
            description: "the computer's TPM 2.0 and DPAPI under your account".into(),
            fallback_reason: String::new(),
        }),
        problem: String::new(),
    };
    let text = dsa_cli::describe_keymint_key(&tpm);
    assert!(text.contains("sealed by the computer's TPM 2.0"), "{text}");
    let json = dsa_cli::keymint_key_json(&tpm);
    assert_eq!(json["sealed_by"], "tpm");
    assert_eq!(json["hardware"], true);
    assert_eq!(json["kept"], true);

    let dpapi = KeyMintKeyState {
        protection: Some(KeyProtection {
            sealed_by: "dpapi".into(),
            hardware: false,
            description: "DPAPI under your account".into(),
            fallback_reason: "this computer has no TPM".into(),
        }),
        problem: "the TPM is locked out".into(),
    };
    let text = dsa_cli::describe_keymint_key(&dpapi);
    assert!(
        text.contains("kept by DPAPI under your account only"),
        "{text}"
    );
    assert!(text.contains("this computer has no TPM"), "{text}");
    assert!(text.contains("the TPM is locked out"), "{text}");
    let none = KeyMintKeyState::default();
    assert!(dsa_cli::describe_keymint_key(&none).contains("no root key yet"));
    assert_eq!(dsa_cli::keymint_key_json(&none)["kept"], false);
}

#[test]
fn fr36_dsa_backup_describes_where_the_backup_stands() {
    let report = dsa_host_core::backup::BackupReport {
        available: true,
        folder: "C:\\Users\\ada\\DSA Backup".into(),
        last_backup_ms: 86_400_000 * 365,
        packages: 3,
        size: 5 * 1024 * 1024,
        ..Default::default()
    };
    let text = dsa_cli::describe_backup(Cmd::BackupStatus, &report);
    assert!(text.contains("DSA Backup"), "{text}");
    assert!(text.contains("1971-01-01 00:00 UTC"), "{text}");
    assert!(text.contains("Apps: 3 (5 MiB"), "{text}");
    let json = dsa_cli::backup_json(&report, None);
    assert_eq!(json["packages"], 3);
    assert_eq!(json["available"], true);
}

#[test]
fn fr32_dsa_gapps_commands_are_parsed() {
    assert_eq!(parse(&args("gapps")).unwrap().command, Cmd::GappsStatus);
    assert_eq!(
        parse(&args("gapps --json")).unwrap().command,
        Cmd::GappsStatus
    );
    assert_eq!(
        parse(&args("gapps status")).unwrap().command,
        Cmd::GappsStatus
    );
    let install = parse(&args("gapps install --progress")).unwrap();
    assert_eq!(install.command, Cmd::GappsInstall);
    assert!(install.progress);
    assert_eq!(
        parse(&args("gapps remove")).unwrap().command,
        Cmd::GappsRemove
    );
    assert!(parse(&args("gapps enable")).is_err());
}

/// LiteGapps' file host, from memory: one x86_64 build for API level 36.
struct FakeLiteGapps(std::collections::HashMap<String, Vec<u8>>);

impl dsa_host_core::update::store::Fetch for FakeLiteGapps {
    fn fetch(&self, url: &str, out: &mut dyn std::io::Write) -> Result<u64, String> {
        let body = self.0.get(url).ok_or_else(|| format!("404 for {url}"))?;
        out.write_all(body).map_err(|e| e.to_string())?;
        Ok(body.len() as u64)
    }
}

fn gapps_service(listener: Box<dyn dsa_transport::local::LocalListener>) -> RunningService {
    use dsa_host_core::gapps::{listing, md5, package, Source};
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::MemorySettings;
    let project = "https://litegapps.test/projects/litegapps";
    let path =
        "/litegapps/x86_64/36/core/2026-10-01/LiteGapps-core-x86_64-16.0-20261001-official.zip";
    let zip = package::fixture::sample();
    let mut digest = md5::Md5::new();
    digest.update(&zip);
    let feed = listing::fixture::feed(&[listing::fixture::item(
        path,
        zip.len() as u64,
        &md5::hex(&digest.finish()),
    )]);
    let host = FakeLiteGapps(std::collections::HashMap::from([
        (
            format!("{project}/rss?path=/litegapps/x86_64/36/core"),
            feed.into_bytes(),
        ),
        (format!("{project}/files{path}/download"), zip),
    ]));
    let work = std::env::temp_dir().join(format!("dsa-cli-gapps-{}", std::process::id()));
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(Arc::new(MemorySettings::default()), ServiceFacts::default())
        .with_gapps(Source::new(Arc::new(host), work).with_project(project))
        .with_addon_disks(addon_disks());
    RunningService::start_service(listener, service)
}

#[test]
fn fr32_dsa_installs_reports_and_removes_google_play() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = gapps_service(listener);
    let none = json_of(&dsa(&endpoint, "gapps --json"));
    assert!(none["package"].is_null());
    assert_eq!(none["installed"], false);
    assert!(none["android"].is_null(), "Android is not running yet");
    assert_eq!(
        none["registration_url"],
        "https://www.google.com/android/uncertified/"
    );

    let installed = json_of(&dsa(&endpoint, "gapps install --json"));
    assert_eq!(installed["package"]["variant"], "core");
    assert_eq!(installed["package"]["version"], "20261001");
    assert_eq!(installed["package"]["android_version"], "16.0");
    assert_eq!(installed["package"]["arch"], "x86_64");
    assert_eq!(installed["installed"], true);
    assert_eq!(installed["android"]["installed"], true);
    assert_eq!(installed["android"]["active"], false);
    assert_eq!(installed["restart_needed"], true);
    let text = stdout(&dsa(&endpoint, "gapps status"));
    assert!(
        text.contains("Google Play: LiteGapps core 20261001 for Android 16.0 (x86_64)"),
        "{text}"
    );
    assert!(text.contains("Restart Android"), "{text}");
    assert!(text.contains("google.com/android/uncertified"), "{text}");

    let removed = json_of(&dsa(&endpoint, "gapps remove --json"));
    assert!(removed["package"].is_null());
    assert_eq!(removed["android"]["installed"], false);
    assert_eq!(removed["restart_needed"], false);
}

#[test]
fn fr32_dsa_gapps_remove_works_while_android_is_stopped_and_starts_nothing() {
    // An Android that Google's apps keep from starting is exactly when they must go.
    use dsa_host_core::gapps::GappsPackage;
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::{MemorySettings, SettingsStore};
    let ((listener, _, user), endpoint) = local_pair();
    let store = Arc::new(MemorySettings::default());
    // What 0.2.0 recorded, from an image this DSA no longer lays over.
    let old = GappsPackage {
        variant: "core".into(),
        version: "20261001".into(),
        android_version: "17.0".into(),
        sdk: 37,
        arch: "x86_64".into(),
        file: "LiteGapps-core-x86_64-17.0-20261001-official.zip".into(),
        source: "https://example.com/x.zip".into(),
        md5: "ea9945a6d63849fa9e51782edfe613a4".into(),
        image_format: 0,
    };
    store.set_gapps(&user, Some(&old)).unwrap();
    let androids = Arc::new(FakeAndroids::default());
    let work = std::env::temp_dir().join(format!("dsa-cli-gapps-stopped-{}", std::process::id()));
    let service = HostApiService::new(androids.clone(), "test service")
        .with_settings(store.clone(), ServiceFacts::default())
        .with_gapps(dsa_host_core::gapps::Source::new(
            Arc::new(FakeLiteGapps(Default::default())),
            work,
        ));
    let _service = RunningService::start_service(listener, service);

    let status = json_of(&dsa(&endpoint, "gapps --json"));
    assert_eq!(status["reinstall_needed"], true);
    assert_eq!(status["package"]["image_format"], 0);
    let text = stdout(&dsa(&endpoint, "gapps status"));
    assert!(text.contains("Install Google Play again"), "{text}");
    assert!(!text.contains("Restart Android"), "{text}");

    let removed = json_of(&dsa(&endpoint, "gapps remove --json"));
    assert!(removed["package"].is_null());
    assert!(removed["android"].is_null(), "Android was not started");
    assert_eq!(removed["reinstall_needed"], false);
    assert_eq!(store.gapps(&user).unwrap(), None);
    let state = json_of(&dsa(&endpoint, "status --json"));
    assert_eq!(state["state"], "Stopped", "{state}");
    let text = stdout(&dsa(&endpoint, "gapps remove"));
    assert!(text.contains("from its next start"), "{text}");
}

#[test]
fn fr15_dsa_settings_chooses_whether_to_take_prereleases() {
    use dsa_cli::Change;
    let parse = |line: &str| {
        dsa_cli::parse(&line.split(' ').map(str::to_string).collect::<Vec<_>>())
            .unwrap()
            .change
    };
    assert_eq!(
        parse("settings set prerelease-updates on"),
        Some(Change::PrereleaseUpdates(true))
    );
    assert_eq!(
        parse("settings set prerelease-updates off"),
        Some(Change::PrereleaseUpdates(false))
    );
    assert!(dsa_cli::parse(
        &"settings set prerelease-updates later"
            .split(' ')
            .map(str::to_string)
            .collect::<Vec<_>>()
    )
    .is_err());

    use dsa_host_core::host_api::{ServiceFacts, SettingsReport};
    let report = |prereleases: bool, available: bool| SettingsReport {
        settings: dsa_host_core::settings::Settings {
            prerelease_updates: prereleases,
            ..Default::default()
        },
        facts: ServiceFacts {
            version: "0.1.10".into(),
            program_updates_available: available,
            update_channel_available: available,
            ..Default::default()
        },
        network: None,
        adb_auto_connect_state: Default::default(),
        touch_gestures: Default::default(),
        adb_shell_root: String::new(),
        imported_certificates: Vec::new(),
    };
    let channel_line = |report: &SettingsReport| {
        dsa_cli::describe_settings(report)
            .lines()
            .find(|line| line.starts_with("Update channel:"))
            .expect("an update channel line")
            .to_string()
    };
    let line = channel_line(&report(true, true));
    assert!(line.contains("pre-release"), "{line}");
    let line = channel_line(&report(false, true));
    assert!(line.starts_with("Update channel: stable"), "{line}");
    let line = channel_line(&report(false, false));
    assert!(line.contains("does not update DSA"), "{line}");
    let json = dsa_cli::settings_json(&report(true, true));
    assert_eq!(json["prerelease_updates"], true);
    assert_eq!(json["update_channel_available"], true);
}

#[test]
fn fr35_the_settings_apps_read_root_elevation_and_each_apps_level() {
    use dsa_host_core::host_api::{ServiceFacts, SettingsReport};
    let report = SettingsReport {
        settings: dsa_host_core::settings::Settings {
            root_elevation: true,
            ..Default::default()
        },
        facts: ServiceFacts::default(),
        network: None,
        adb_auto_connect_state: Default::default(),
        touch_gestures: Default::default(),
        adb_shell_root: String::new(),
        imported_certificates: Vec::new(),
    };
    assert_eq!(dsa_cli::settings_json(&report)["root_elevation"], true);
    let line = dsa_cli::describe_settings(&report)
        .lines()
        .find(|line| line.starts_with("Root elevation:"))
        .expect("a root elevation line")
        .to_string();
    assert!(line.contains("on"), "{line}");
    assert!(line.contains("next start of Android"), "{line}");

    for level in ["off", "ask", "remember", "unavailable"] {
        let app = dsa_host_core::tasks::AppInfo {
            package_name: "com.example.backup".into(),
            root: level.into(),
            ..Default::default()
        };
        assert_eq!(dsa_cli::app_json(&app, false)["root"], level);
        let described = dsa_cli::describe_app(&app);
        let root = described
            .lines()
            .find(|line| line.starts_with("Root: "))
            .expect("a root line");
        assert!(root.starts_with(&format!("Root: {level}")), "{root}");
    }
}

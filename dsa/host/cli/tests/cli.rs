//! `dsa` against a running service, over the platform's real local transport.

use dsa_host_core::ime::ImeEvent;
use dsa_host_core::input::PointerEvent;
use dsa_host_core::keys::KeyEvent;
use dsa_host_core::tasks::TaskRequestError;
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
fn fr2_dsa_shows_the_guest_console_of_a_failed_start_below_the_status_line() {
    use dsa_host_core::host_api::Status;
    let line = dsa_cli::describe(&Status {
        state: SessionState::Stopped,
        last_reason: Some(
            "the Android VM exited during boot\nlast console lines:\n  Kernel panic".into(),
        ),
        update_notice: None,
    });
    assert_eq!(
        line,
        "Android: Stopped (last stopped: the Android VM exited during boot)\n\
         last console lines:\n  Kernel panic"
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
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
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
                image_updates_available: true,
            },
        );
    RunningService::start_service(listener, service)
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
    assert!(parse(&args("settings set freeze maybe")).is_err());
    assert!(parse(&args("settings set colour blue")).is_err());
    assert!(parse(&args("settings set render-gpu")).is_err());
    assert_eq!(parse(&args("memory --json")).unwrap().command, Cmd::Memory);
    assert_eq!(parse(&args("gpus")).unwrap().command, Cmd::Gpus);
    assert!(!parse(&args("status")).unwrap().json);
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
}

#[test]
fn fr17_dsa_settings_reads_and_changes_the_settings_through_the_service() {
    let ((listener, _, _), endpoint) = local_pair();
    let _service = settings_service(listener);
    let settings = json_of(&dsa(&endpoint, "settings --json"));
    assert_eq!(settings["render_gpu"], "display");
    assert_eq!(settings["freeze"], false);
    assert_eq!(settings["program_updates"], true);
    assert_eq!(settings["version"], "0.0.9");
    assert_eq!(settings["log_dir"], "C:\\ProgramData\\DSA\\logs");

    let changed = json_of(&dsa(&endpoint, "settings set render-gpu Integrated --json"));
    assert_eq!(changed["render_gpu"], "integrated");
    let changed = json_of(&dsa(&endpoint, "settings set freeze on --json"));
    assert_eq!(changed["freeze"], true);
    assert_eq!(
        changed["render_gpu"], "integrated",
        "one change keeps the others"
    );

    let text = dsa(&endpoint, "settings");
    assert!(text.status.success(), "{}", stderr(&text));
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

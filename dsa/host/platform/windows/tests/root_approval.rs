//! Acceptance tests for the Windows administrator approval of an Android app's su call, with
//! the in-memory local transport standing in for the approval pipe and a fake standing in for
//! the app's window program, so no real UAC prompt is shown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::root_approval::{ApprovalRequest, PromptAnswer, Purpose};
use dsa_host_core::tasks::AppInfo;
use dsa_host_core::ui_text::root_approval::AskedWords;
use dsa_host_windows::root_approval::{
    answer_of_exit, approval_arguments, exit, helper_mode, is_pipe_name, pipe_name, prompt_program,
    screen_reader_of_exit, wait_for_answer, HelperMode, PromptProcess, APPROVAL_ARGUMENT,
    APPROVED_ARGUMENT, RECEIVED, SCREEN_READER_ARGUMENT, SHARED_PROGRAM_KEY, SHELL_PROGRAM_KEY,
    SYSTEM_PROGRAM_KEY,
};
use dsa_host_windows::ui_language::UiLanguage;
use dsa_transport::local::{self, Peer};

/// A window program that ends with `code` after `after`, or runs until it is ended.
struct FakeProgram {
    started: Instant,
    ends: Option<(Duration, u32)>,
    ended: Arc<AtomicBool>,
}

impl FakeProgram {
    fn ending(after: Duration, code: u32) -> FakeProgram {
        FakeProgram {
            started: Instant::now(),
            ends: Some((after, code)),
            ended: Arc::default(),
        }
    }

    fn running() -> FakeProgram {
        FakeProgram {
            started: Instant::now(),
            ends: None,
            ended: Arc::default(),
        }
    }
}

impl PromptProcess for FakeProgram {
    fn exit_code(&mut self) -> Option<u32> {
        if self.ended.load(Ordering::Relaxed) {
            return Some(exit::FAILED);
        }
        self.ends
            .filter(|(after, _)| self.started.elapsed() >= *after)
            .map(|(_, code)| code)
    }

    fn end(&mut self) {
        self.ended.store(true, Ordering::Relaxed);
    }
}

fn secret() -> String {
    pipe_name(&[0xab; 16])
}

fn administrator() -> Peer {
    Peer {
        user: "S-1-5-21-1-2-3-500".into(),
        administrator: true,
    }
}

#[test]
fn fr35_an_elevated_administrators_answer_approves() {
    let (mut listener, connector) = local::pair();
    let pipe = secret();
    let answer = {
        let pipe = pipe.clone();
        let mut connector = connector.clone();
        thread::spawn(move || {
            let mut elevated = connector.connect_as(administrator()).unwrap();
            elevated.send(pipe.as_bytes()).unwrap();
            elevated.recv(Some(Duration::from_secs(5))).unwrap()
        })
    };
    let mut program = FakeProgram::running();
    let got = wait_for_answer(
        &mut listener,
        pipe.as_bytes(),
        &mut program,
        Duration::from_secs(5),
    );
    assert_eq!(got, PromptAnswer::Approved);
    // The elevated copy is told the approval arrived.
    assert_eq!(answer.join().unwrap(), RECEIVED);
    drop(connector);
}

#[test]
fn fr35_an_answer_without_administrator_rights_does_not_approve() {
    let (mut listener, mut connector) = local::pair();
    let pipe = secret();
    // An unelevated process of the user, and an administrator that does not know the pipe's
    // name, both try.
    let mut unelevated = connector
        .connect_as(Peer {
            user: "S-1-5-21-1-2-3-1001".into(),
            administrator: false,
        })
        .unwrap();
    unelevated.send(pipe.as_bytes()).unwrap();
    let mut guesser = connector.connect_as(administrator()).unwrap();
    guesser.send(b"dsa-root-approval-00").unwrap();
    let mut program = FakeProgram::running();
    let got = wait_for_answer(
        &mut listener,
        pipe.as_bytes(),
        &mut program,
        Duration::from_millis(600),
    );
    assert_eq!(got, PromptAnswer::TimedOut);
}

#[test]
fn fr35_a_cancelled_uac_prompt_is_refused() {
    let (mut listener, _connector) = local::pair();
    let mut program = FakeProgram::ending(Duration::from_millis(50), exit::REFUSED);
    let started = Instant::now();
    let got = wait_for_answer(
        &mut listener,
        secret().as_bytes(),
        &mut program,
        Duration::from_secs(10),
    );
    assert_eq!(got, PromptAnswer::Denied);
    // The refusal comes as soon as the prompt closes, not at the limit.
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn fr35_a_standard_user_with_uac_off_is_refused() {
    let (mut listener, _connector) = local::pair();
    let mut program = FakeProgram::ending(Duration::ZERO, exit::NOT_ADMINISTRATOR);
    let got = wait_for_answer(
        &mut listener,
        secret().as_bytes(),
        &mut program,
        Duration::from_secs(10),
    );
    assert_eq!(got, PromptAnswer::NotAdministrator);
}

#[test]
fn fr35_an_unanswered_prompt_times_out_and_is_ended() {
    let (mut listener, _connector) = local::pair();
    let mut program = FakeProgram::running();
    let ended = Arc::clone(&program.ended);
    let started = Instant::now();
    let got = wait_for_answer(
        &mut listener,
        secret().as_bytes(),
        &mut program,
        Duration::from_millis(400),
    );
    assert_eq!(got, PromptAnswer::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert!(ended.load(Ordering::Relaxed), "the program was ended");
}

#[test]
fn fr35_a_program_that_ends_without_an_approval_never_approves() {
    // Only the pipe approves: an exit code, even the one the elevated copy ends with after
    // delivering, does not.
    for code in [
        exit::APPROVED,
        exit::FAILED,
        exit::NOT_DELIVERED,
        exit::ELEVATION_FAILED,
        exit::BAD_ARGUMENTS,
        77,
    ] {
        assert!(
            matches!(answer_of_exit(code), PromptAnswer::Unavailable(_)),
            "code {code}"
        );
    }
    assert_eq!(answer_of_exit(exit::REFUSED), PromptAnswer::Denied);
    assert_eq!(
        answer_of_exit(exit::NOT_ADMINISTRATOR),
        PromptAnswer::NotAdministrator
    );
}

#[test]
fn fr35_the_window_program_reads_what_the_service_asks() {
    let pipe = secret();
    assert!(is_pipe_name(&pipe));
    let words = AskedWords {
        label: "Backup \"Pro\"".into(),
        detail: "com.example.backup".into(),
        command: "sh -c 'echo hi'".into(),
        language: UiLanguage::Korean,
        seconds: 55,
    };
    let line = approval_arguments(&pipe, &words);
    let args: Vec<String> = line.split(' ').map(str::to_string).collect();
    assert_eq!(
        helper_mode(&args),
        Some(HelperMode::Ask {
            pipe: pipe.clone(),
            words: words.clone(),
        })
    );
    // An empty line still leaves every other flag its value.
    let bare = AskedWords {
        detail: String::new(),
        language: UiLanguage::English,
        ..words.clone()
    };
    let args: Vec<String> = approval_arguments(&pipe, &bare)
        .split(' ')
        .map(str::to_string)
        .collect();
    assert_eq!(
        helper_mode(&args),
        Some(HelperMode::Ask {
            pipe: pipe.clone(),
            words: bare,
        })
    );
    // Every language reaches the program as it was asked in.
    for language in UiLanguage::ALL {
        let asked = AskedWords {
            language,
            ..words.clone()
        };
        let args: Vec<String> = approval_arguments(&pipe, &asked)
            .split(' ')
            .map(str::to_string)
            .collect();
        let Some(HelperMode::Ask { words: read, .. }) = helper_mode(&args) else {
            panic!("the program was not asked to approve in {language:?}");
        };
        assert_eq!(read.language, language);
    }
    let elevated = vec![APPROVED_ARGUMENT.to_string(), pipe.clone()];
    assert_eq!(
        helper_mode(&elevated),
        Some(HelperMode::Deliver { pipe: pipe.clone() })
    );
    // A pipe that is not an approval pipe is refused, so nothing else can be dialled.
    let bad = vec![APPROVAL_ARGUMENT.to_string(), r"..\other-pipe".to_string()];
    assert_eq!(helper_mode(&bad), Some(HelperMode::Invalid));
    assert!(!is_pipe_name("dsa-root-approval-"));
    assert!(!is_pipe_name("dsa-root-approval-AB"));
    // A window host started for its app asks for nothing.
    assert_eq!(
        helper_mode(&["--package".into(), "com.example".into()]),
        None
    );
}

fn su_from(package: &str, uid: u32, label: &str, icon: &[u8]) -> ApprovalRequest {
    ApprovalRequest {
        request_id: 1,
        app: AppInfo {
            package_name: package.into(),
            label: label.into(),
            icon_png: icon.to_vec(),
            ..Default::default()
        },
        uid,
        command: vec!["id".into()],
        interactive: false,
        purpose: Purpose::Run,
        shared_apps: Vec::new(),
        language: "ko-KR".into(),
    }
}

#[test]
fn fr35_the_root_prompt_names_an_app_by_its_label_and_icon() {
    let icon = [0x89, b'P', b'N', b'G', 7];
    let request = su_from("com.example.backup", 10_200, "백업", &icon);
    // The app's own window program: its label is the name UAC shows, its icon the icon.
    let (folder, app) = prompt_program(&request);
    assert_eq!(folder, "com.example.backup");
    assert_eq!(app.label, "백업");
    assert_eq!(app.icon_png, icon);
}

#[test]
fn fr35_the_root_prompt_names_the_adb_shell_as_the_adb_shell() {
    let request = su_from("com.android.shell", 2000, "Shell", &[]);
    let (folder, app) = prompt_program(&request);
    // A program of its own, never the folder of an Android package.
    assert_eq!(folder, SHELL_PROGRAM_KEY);
    assert_eq!(app.package_name, SHELL_PROGRAM_KEY);
    assert_eq!(app.label, "adb 셸");
    // A generic terminal icon the host draws, in place of DSA's own.
    assert_eq!(&app.icon_png[1..4], b"PNG");
    let words = AskedWords::of(&request);
    assert_eq!(words.detail, "com.android.shell (uid 2000)");

    let mut english = request.clone();
    english.language = "en-US".into();
    assert_eq!(prompt_program(&english).1.label, "adb shell");

    let system = su_from("android", 1000, "Android System", &[]);
    let (folder, app) = prompt_program(&system);
    assert_eq!(folder, SYSTEM_PROGRAM_KEY);
    assert_eq!(app.label, "Android 시스템");
    assert_ne!(app.icon_png, prompt_program(&request).1.icon_png);
}

#[test]
fn fr35_the_root_prompt_names_a_shared_user_id_by_its_apps() {
    let icon = [0x89, b'P', b'N', b'G', 3];
    let mut request = su_from("com.shared.one, com.shared.two", 10_150, "", &[]);
    request.shared_apps = vec![
        AppInfo {
            package_name: "com.shared.one".into(),
            label: "하나".into(),
            ..Default::default()
        },
        AppInfo {
            package_name: "com.shared.two".into(),
            label: "둘".into(),
            icon_png: icon.to_vec(),
            ..Default::default()
        },
    ];
    let (folder, app) = prompt_program(&request);
    assert_eq!(folder, SHARED_PROGRAM_KEY);
    assert_eq!(app.label, "하나, 둘");
    assert_eq!(app.icon_png, icon);
}

#[test]
fn fr18_the_window_program_answers_whether_a_screen_reader_runs() {
    // The service asks the window program in the user's session, and reads the answer from
    // its exit code.
    assert_eq!(
        helper_mode(&[SCREEN_READER_ARGUMENT.to_string()]),
        Some(HelperMode::ScreenReader)
    );
    assert!(screen_reader_of_exit(exit::SCREEN_READER_RUNNING));
    assert!(!screen_reader_of_exit(exit::NO_SCREEN_READER));
    // A program that failed, or one too old to know the question, says no: the prompt keeps
    // its usual time rather than not opening.
    for code in [exit::FAILED, exit::BAD_ARGUMENTS, exit::APPROVED, 77] {
        assert!(!screen_reader_of_exit(code), "code {code}");
    }
    // The answers are not taken for an approval's.
    assert!(matches!(
        answer_of_exit(exit::SCREEN_READER_RUNNING),
        PromptAnswer::Unavailable(_)
    ));
}

#[test]
fn fr18_the_time_the_prompt_has_reaches_the_window_program() {
    let request = su_from("com.example.backup", 10_200, "Backup", &[]);
    let mut english = request.clone();
    english.language = "en-US".into();
    // The time the prompt has goes to the program, longer while a screen reader runs.
    let words = AskedWords::of(&english).within(Duration::from_secs(300));
    assert_eq!(words.seconds, 300);
    let args: Vec<String> = approval_arguments(&secret(), &words)
        .split(' ')
        .map(str::to_string)
        .collect();
    let Some(HelperMode::Ask { words: read, .. }) = helper_mode(&args) else {
        panic!("the program was not asked to approve");
    };
    assert_eq!(read.seconds, 300);
}

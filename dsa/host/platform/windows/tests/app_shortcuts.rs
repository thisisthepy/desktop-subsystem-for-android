//! Android app shortcuts as jump list tasks and desktop shortcuts: the jump lists the window
//! manager builds from the installed apps, the desktop shortcuts it and `dsa` make, an app's
//! request to pin a shortcut, and their removal with the app, through fakes of the shell's
//! jump lists, shortcut files and the user's answer.
#![cfg(windows)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use dsa_host_core::tasks::{AppInfo, AppShortcut, PinShortcutRequest};
use dsa_host_windows::compositor::pin_shortcuts::{
    accepted, decide, dialog_request, dialog_text, question, Ask, Place, Question,
    Texts as PinTexts, ACCESS_KEYS, ANSWER_WITHIN,
};
use dsa_host_windows::desktop_shortcuts::{entry_in, read_note, Desktop};
use dsa_host_windows::jump_list::{sync_apps, Committed, JumpList, JumpLists, Texts};
use dsa_host_windows::start_menu::{app_user_model_id, Shortcut, Shortcuts};

const CHAT: &str = "com.example.chat";

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("app-shortcuts")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Shortcut files kept in memory, each also an empty file on disk, so a name it took is taken.
#[derive(Default)]
struct FakeLinks(Mutex<HashMap<PathBuf, Shortcut>>);

impl Shortcuts for FakeLinks {
    fn write(&self, path: &Path, shortcut: &Shortcut) -> Result<(), String> {
        std::fs::write(path, b"").map_err(|e| e.to_string())?;
        self.0
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), shortcut.clone());
        Ok(())
    }

    fn read(&self, path: &Path) -> Result<Shortcut, String> {
        self.0
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| "no such shortcut".into())
    }
}

#[derive(Default)]
struct FakeJumpLists {
    committed: Mutex<Vec<JumpList>>,
    deleted: Mutex<Vec<String>>,
}

impl JumpLists for FakeJumpLists {
    fn commit(&self, list: &JumpList) -> Result<(), String> {
        self.committed.lock().unwrap().push(list.clone());
        Ok(())
    }

    fn delete(&self, app_user_model_id: &str) -> Result<(), String> {
        self.deleted
            .lock()
            .unwrap()
            .push(app_user_model_id.to_string());
        Ok(())
    }
}

/// The user's answer, and the questions put.
struct FakeAsk {
    yes: bool,
    asked: Mutex<Vec<Question>>,
}

impl FakeAsk {
    fn answering(yes: bool) -> FakeAsk {
        FakeAsk {
            yes,
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl Ask for FakeAsk {
    fn ask(&self, question: &Question, within: Duration) -> bool {
        assert!(within <= ANSWER_WITHIN);
        self.asked.lock().unwrap().push(question.clone());
        self.yes
    }
}

/// A PNG header of an image `side` pixels square, which is all an icon file needs of it.
fn png(side: u32, tag: u8) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&side.to_be_bytes());
    png.extend_from_slice(&side.to_be_bytes());
    png.push(tag);
    png
}

fn shortcut(id: &str, label: &str, rank: u32) -> AppShortcut {
    AppShortcut {
        id: id.into(),
        short_label: label.into(),
        icon_png: png(96, rank as u8),
        rank,
        dynamic: true,
        ..Default::default()
    }
}

fn chat(shortcuts: Vec<AppShortcut>) -> AppInfo {
    AppInfo {
        package_name: CHAT.into(),
        label: "Chat".into(),
        shortcuts,
        ..Default::default()
    }
}

/// The app's window program, as the Start menu makes it.
fn make_program(apps_dir: &Path) -> PathBuf {
    let program = dsa_host_windows::app_exe::program_path(apps_dir, CHAT, "Chat");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, b"").unwrap();
    program
}

#[test]
fn fr7_app_shortcuts_are_jump_list_tasks_and_desktop_shortcuts() {
    let dir = fresh_dir("criterion");
    let apps_dir = dir.join("apps");
    let program = make_program(&apps_dir);
    let desktop = Desktop {
        dir: dir.join("Desktop"),
        note: dir.join("desktop-shortcuts.json"),
    };
    let links = FakeLinks::default();
    let lists = FakeJumpLists::default();
    let mut committed = Committed::new();

    // The app's shortcuts become its jump list's tasks, by rank, each starting its shortcut
    // through the app's program under the app's ID, with its own icon; then Create desktop
    // shortcut.
    let apps = vec![chat(vec![
        shortcut("ana", "Ana", 1),
        shortcut("new-chat", "New chat", 0),
    ])];
    let texts = Texts::english();
    assert!(sync_apps(&apps, &apps_dir, &texts, &mut committed, &lists).is_empty());
    let list = lists.committed.lock().unwrap()[0].clone();
    assert_eq!(list.app_user_model_id, app_user_model_id(CHAT));
    assert_eq!(list.program, program);
    let titles: Vec<&str> = list.tasks.iter().map(|task| task.title.as_str()).collect();
    assert_eq!(titles, vec!["New chat", "Ana", "Create desktop shortcut"]);
    assert_eq!(
        list.tasks[0].arguments,
        format!("--package {CHAT} --shortcut new-chat")
    );
    let icon = &list.tasks[0].icon.0;
    assert_eq!(icon.extension().unwrap(), "ico");
    assert!(icon.starts_with(apps_dir.join(CHAT)));
    assert_eq!(&std::fs::read(icon).unwrap()[22..], &png(96, 0)[..]);
    assert_eq!(
        list.tasks[2].arguments,
        format!("--create-desktop-shortcut --package {CHAT}")
    );
    assert_eq!(list.tasks[2].icon, (program.clone(), 0));
    assert_eq!(
        Texts::korean().create_desktop_shortcut,
        "바탕 화면 바로 가기 만들기"
    );

    // The same list is not committed again; a change is.
    sync_apps(&apps, &apps_dir, &texts, &mut committed, &lists);
    assert_eq!(lists.committed.lock().unwrap().len(), 1);
    let fewer = vec![chat(vec![shortcut("new-chat", "New chat", 0)])];
    sync_apps(&fewer, &apps_dir, &texts, &mut committed, &lists);
    assert_eq!(lists.committed.lock().unwrap()[1].tasks.len(), 2);

    // Create desktop shortcut: the app's own shortcut, as its Start menu entry is. A file of
    // the user's under the app's name stays; the shortcut takes the next name.
    std::fs::create_dir_all(&desktop.dir).unwrap();
    std::fs::write(desktop.dir.join("Chat.lnk"), b"the user's").unwrap();
    let entry = entry_in(&apps_dir, CHAT, "Chat").unwrap();
    let app_link = desktop.place_app(&entry, &links).unwrap();
    assert_eq!(app_link, desktop.dir.join(format!("Chat ({CHAT}).lnk")));
    assert_eq!(links.read(&app_link).unwrap(), entry.shortcut());
    assert_eq!(
        std::fs::read(desktop.dir.join("Chat.lnk")).unwrap(),
        b"the user's"
    );
    // Made again, it is the same file.
    assert_eq!(desktop.place_app(&entry, &links).unwrap(), app_link);

    // The app asks to pin a shortcut: the user is asked under the app's name, and on yes the
    // desktop shortcut starts that shortcut.
    let request = PinShortcutRequest {
        request_id: 4,
        package_name: CHAT.into(),
        app_label: "Chat".into(),
        shortcut: AppShortcut {
            pinned: true,
            ..shortcut("ana", "Ana", 1)
        },
    };
    let place = Place {
        desktop: Desktop {
            dir: desktop.dir.clone(),
            note: desktop.note.clone(),
        },
        apps_dir: apps_dir.clone(),
    };
    let ask = FakeAsk::answering(true);
    let (accepted, _) = decide(
        &request,
        Duration::ZERO,
        &ask,
        &PinTexts::english(),
        Some(&place),
        &links,
    );
    assert!(accepted);
    let asked = ask.asked.lock().unwrap()[0].clone();
    assert_eq!(asked.title, "Chat");
    assert_eq!(asked.add, "Add to desktop");
    assert_eq!(asked.cancel, "Cancel");
    assert!(asked.instruction.contains("Ana"), "{}", asked.instruction);
    let pinned = desktop.dir.join("Ana.lnk");
    let link = links.read(&pinned).unwrap();
    assert_eq!(link.target, program);
    assert_eq!(link.arguments, format!("--package {CHAT} --shortcut ana"));
    assert_eq!(link.app_user_model_id, app_user_model_id(CHAT));
    assert_eq!(link.icon.0.extension().unwrap(), "ico");

    // Declined, or asked too late: no shortcut, and Android hears no.
    let other = PinShortcutRequest {
        shortcut: shortcut("bo", "Bo", 2),
        ..request.clone()
    };
    let (accepted, _) = decide(
        &other,
        Duration::ZERO,
        &FakeAsk::answering(false),
        &PinTexts::english(),
        Some(&place),
        &links,
    );
    assert!(!accepted);
    assert!(!desktop.dir.join("Bo.lnk").exists());
    let late = FakeAsk::answering(true);
    let (accepted, _) = decide(
        &other,
        ANSWER_WITHIN,
        &late,
        &PinTexts::english(),
        Some(&place),
        &links,
    );
    assert!(!accepted);
    assert!(late.asked.lock().unwrap().is_empty());

    // Uninstalled: the jump list is deleted and the desktop shortcuts go; the user's file stays.
    sync_apps(&[], &apps_dir, &texts, &mut committed, &lists);
    assert_eq!(
        *lists.deleted.lock().unwrap(),
        vec![app_user_model_id(CHAT)]
    );
    assert!(desktop.keep_only(&BTreeSet::new()).is_empty());
    assert!(!app_link.exists());
    assert!(!pinned.exists());
    assert!(desktop.dir.join("Chat.lnk").exists());
    assert!(read_note(&desktop.note).apps.is_empty());
}

#[test]
fn fr7_the_pin_question_speaks_korean_and_english() {
    let request = PinShortcutRequest {
        request_id: 1,
        package_name: CHAT.into(),
        app_label: "Chat".into(),
        shortcut: shortcut("ana", "Ana", 1),
    };
    let korean = question(&request, &PinTexts::korean());
    assert_eq!(korean.add, "바탕 화면에 추가");
    assert_eq!(korean.cancel, "취소");
    assert!(korean.instruction.contains("Ana"));
    let english = question(&request, &PinTexts::english());
    assert!(english.content.contains("Chat"));
    // An app without a name is named by its package.
    let unnamed = PinShortcutRequest {
        app_label: " ".into(),
        ..request
    };
    assert_eq!(question(&unnamed, &PinTexts::english()).title, CHAT);
}

#[test]
fn fr7_an_app_whose_program_is_not_made_has_no_jump_list_or_desktop_shortcut_yet() {
    let dir = fresh_dir("no-program");
    let apps_dir = dir.join("apps");
    let lists = FakeJumpLists::default();
    let mut committed = Committed::new();
    let apps = vec![chat(vec![shortcut("new-chat", "New chat", 0)])];
    assert!(sync_apps(&apps, &apps_dir, &Texts::english(), &mut committed, &lists).is_empty());
    assert!(lists.committed.lock().unwrap().is_empty());
    assert!(entry_in(&apps_dir, CHAT, "Chat").is_err());
    // Once it is made, under an older name, the app has both.
    let old = apps_dir.join(CHAT).join("Old chat.exe");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"").unwrap();
    sync_apps(&apps, &apps_dir, &Texts::english(), &mut committed, &lists);
    assert_eq!(lists.committed.lock().unwrap()[0].program, old);
}

#[test]
fn fr7_jump_lists_are_remembered_across_restarts_of_the_window_manager() {
    let dir = fresh_dir("restart");
    let apps_dir = dir.join("apps");
    make_program(&apps_dir);
    let note = dir.join("jump-lists.json");
    let lists = FakeJumpLists::default();
    let apps = vec![chat(vec![shortcut("new-chat", "New chat", 0)])];
    let mut first = Committed::in_note(note.clone());
    sync_apps(&apps, &apps_dir, &Texts::english(), &mut first, &lists);
    assert_eq!(lists.committed.lock().unwrap().len(), 1);
    assert!(note.exists());

    // Restarted: the unchanged list is not committed again; a changed one is.
    let mut second = Committed::in_note(note.clone());
    assert_eq!(
        second.get(CHAT).map(|list| list.app_user_model_id.clone()),
        Some(app_user_model_id(CHAT))
    );
    sync_apps(&apps, &apps_dir, &Texts::english(), &mut second, &lists);
    assert_eq!(lists.committed.lock().unwrap().len(), 1);
    let changed = vec![chat(vec![shortcut("ana", "Ana", 0)])];
    sync_apps(&changed, &apps_dir, &Texts::english(), &mut second, &lists);
    assert_eq!(lists.committed.lock().unwrap().len(), 2);

    // The app went while the window manager was off: the next start deletes its list.
    let mut third = Committed::in_note(note.clone());
    sync_apps(&[], &apps_dir, &Texts::english(), &mut third, &lists);
    assert_eq!(
        *lists.deleted.lock().unwrap(),
        vec![app_user_model_id(CHAT)]
    );
    assert!(Committed::in_note(note.clone()).get(CHAT).is_none());

    // A note that does not read records nothing, and the lists are made again.
    std::fs::write(&note, b"not json").unwrap();
    let mut damaged = Committed::in_note(note.clone());
    sync_apps(&apps, &apps_dir, &Texts::english(), &mut damaged, &lists);
    assert_eq!(lists.committed.lock().unwrap().len(), 3);

    // Uninstalling DSA removes the note with the user's other files.
    let root = dsa_host_windows::paths::UserRoot::from_dir(&dir);
    assert_eq!(root.jump_lists_note(), note);
    dsa_host_windows::uninstall::remove_user_files(&root, &dir.join("Programs"));
    assert!(!note.exists());
}

#[test]
fn fr7_uninstalling_dsa_removes_the_desktop_shortcuts_it_made() {
    let dir = fresh_dir("uninstall");
    let desktop = Desktop {
        dir: dir.join("Desktop"),
        note: dir.join("desktop-shortcuts.json"),
    };
    let links = FakeLinks::default();
    let apps_dir = dir.join("apps");
    make_program(&apps_dir);
    let entry = entry_in(&apps_dir, CHAT, "Chat").unwrap();
    let made = desktop.place_app(&entry, &links).unwrap();
    std::fs::write(desktop.dir.join("mine.txt"), b"").unwrap();
    assert_eq!(
        dsa_host_windows::uninstall::remove_desktop_shortcuts(&desktop.note),
        Ok(1)
    );
    assert!(!made.exists());
    assert!(!desktop.note.exists());
    assert!(desktop.dir.join("mine.txt").exists());
    assert_eq!(
        dsa_host_windows::uninstall::remove_desktop_shortcuts(&desktop.note),
        Ok(0)
    );
}

#[test]
fn fr7_the_pin_question_is_a_windows_11_dialog_with_add_as_its_accent_button() {
    use dsa_host_windows::compositor::fluent_dialog::{
        layout, Answer, Control, DialogState, Key, Outcome,
    };
    let request = PinShortcutRequest {
        request_id: 4,
        package_name: CHAT.into(),
        app_label: "Chat".into(),
        shortcut: shortcut("ana", "Ana", 1),
    };
    for (texts, add, cancel) in [
        (PinTexts::english(), "Add to desktop", "Cancel"),
        (PinTexts::korean(), "바탕 화면에 추가", "취소"),
    ] {
        let text = dialog_text(&question(&request, &texts));
        // The heading names the shortcut, the body the app; no check box.
        assert!(text.title.contains("Ana"), "{}", text.title);
        assert!(text.body.contains("Chat"), "{}", text.body);
        assert_eq!(text.check, None);
        // Add is the primary button, which the dialog draws in the accent color.
        assert_eq!(text.primary, add);
        assert_eq!(text.secondary, cancel);
        assert_eq!(text.keys, ACCESS_KEYS);
        let laid = layout(&text, 96, &|line, _| line.chars().count() as i32 * 7);
        assert!(laid.check_box.is_none());
        assert!(laid.primary.x < laid.secondary.x, "Add comes first");
        // The dialog closes as no once the three minutes are up.
        let shown = dialog_request(&text, ANSWER_WITHIN);
        assert_eq!(shown.timeout, Some(ANSWER_WITHIN));
    }

    // The keyboard: Add has the focus, so Enter adds; Escape declines; the access keys press
    // their buttons; Tab moves between the two buttons only.
    let keys = ACCESS_KEYS;
    let mut state = DialogState::new(false);
    assert_eq!(state.focus, Control::Primary);
    assert_eq!(
        state.key(Key::Enter, &keys),
        Outcome::Done(Answer::Confirmed { checked: false })
    );
    let mut state = DialogState::new(false);
    assert_eq!(
        state.key(Key::Escape, &keys),
        Outcome::Done(Answer::Cancelled)
    );
    let mut state = DialogState::new(false);
    state.key(Key::Tab { shift: false }, &keys);
    assert_eq!(state.focus, Control::Secondary);
    state.key(Key::Tab { shift: false }, &keys);
    assert_eq!(state.focus, Control::Primary);
    let mut state = DialogState::new(false);
    assert_eq!(
        state.key(Key::Char('c'), &keys),
        Outcome::Done(Answer::Cancelled)
    );
    let mut state = DialogState::new(false);
    assert_eq!(
        state.key(Key::Char('a'), &keys),
        Outcome::Done(Answer::Confirmed { checked: false })
    );

    // Only Add is a yes.
    assert!(accepted(Answer::Confirmed { checked: false }));
    assert!(!accepted(Answer::Cancelled));
}

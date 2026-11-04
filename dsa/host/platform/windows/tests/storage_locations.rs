//! Where DSA keeps its files: two roots, a fixed place for each file, and moving files from
//! where earlier releases kept them.

use std::path::{Path, PathBuf};

use dsa_host_windows::paths::{
    clear_tmp, migrate, migrate_user_disk, old_trace_markers, unique_name, MachineRoot, Moved,
    OldTraceMarkers, UserRoot,
};
use dsa_host_windows::uninstall::{remove_machine_files, remove_user_files};

/// What builds a path from a user's or the machine's folders, or from a temporary folder.
const FORBIDDEN: &[&str] = &[
    "(\"LOCALAPPDATA\")",
    "(\"ProgramData\")",
    "(\"PROGRAMDATA\")",
    "(\"APPDATA\")",
    "(\"USERPROFILE\")",
    "(\"TEMP\")",
    "(\"TMP\")",
    "temp_dir()",
    "GetTempPath",
    "\"AppData\"",
    "SpecialFolder.LocalApplicationData",
    "SpecialFolder.CommonApplicationData",
    "SpecialFolder.ApplicationData",
];

/// The files allowed to name those folders: the paths modules themselves.
fn is_paths_module(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text.ends_with("platform/windows/src/paths.rs")
        || text.ends_with("settings/DsaSettings.Core/StoragePaths.cs")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            // Tests may use temporary folders; build outputs are not sources. A Rust crate's
            // `src\bin` holds programs, so only a .NET project's `bin` is skipped.
            let rust_programs = name == "bin" && dir.file_name().is_some_and(|d| d == "src");
            let skipped = matches!(name.as_str(), "tests" | "target" | "obj")
                || (name == "bin" && !rust_programs)
                || name.ends_with(".Tests");
            if !skipped {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") || name.ends_with(".cs") {
            out.push(path);
        }
    }
}

/// The production part of a source file: up to its unit tests, without comments.
fn production_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .take_while(|(_, line)| line.trim() != "#[cfg(test)]")
        .filter(|(_, line)| {
            let line = line.trim_start();
            !line.starts_with("//") && !line.starts_with("///")
        })
}

#[test]
fn fr19_production_code_builds_runtime_paths_only_in_the_paths_module() {
    let host = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    sources(&host, &mut files);
    assert!(
        files.iter().any(|f| is_paths_module(f)),
        "the paths module was not found under {}",
        host.display()
    );
    let mut found = Vec::new();
    for file in files.iter().filter(|f| !is_paths_module(f)) {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (number, line) in production_lines(&text) {
            for pattern in FORBIDDEN.iter().filter(|p| line.contains(**p)) {
                found.push(format!("{}:{}: {pattern}", file.display(), number + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "these build a path from a user's, the machine's or a temporary folder outside the \
         paths module, so the file would land outside DSA's two folders:\n{}",
        found.join("\n")
    );
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("storage-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fr19_every_file_has_its_place_in_one_of_the_two_roots() {
    let machine = MachineRoot::at(r"C:\ProgramData\DSA");
    assert_eq!(
        machine.images(),
        PathBuf::from(r"C:\ProgramData\DSA\images")
    );
    assert_eq!(
        machine.updates(),
        PathBuf::from(r"C:\ProgramData\DSA\updates")
    );
    assert_eq!(
        machine.settings(),
        PathBuf::from(r"C:\ProgramData\DSA\settings")
    );
    assert_eq!(
        machine.service_log(),
        PathBuf::from(r"C:\ProgramData\DSA\logs\dsa-hostd.log")
    );
    assert_eq!(machine.tmp(), PathBuf::from(r"C:\ProgramData\DSA\tmp"));
    assert_eq!(
        MachineRoot::of_config_file(Path::new(r"D:\Data\DSA\dsa-hostd.json")),
        MachineRoot::at(r"D:\Data\DSA")
    );

    let user = UserRoot::in_local_app_data(Path::new(r"C:\Users\alice\AppData\Local"));
    let under = |rest: &str| PathBuf::from(format!(r"C:\Users\alice\AppData\Local\DSA\{rest}"));
    assert_eq!(user.android_disk(), under("android.vhdx"));
    assert_eq!(user.apps(), under("apps"));
    assert_eq!(user.notifications(), under("notifications"));
    assert_eq!(user.window_manager_log(), under(r"logs\dsa-wm.log"));
    assert_eq!(user.app_window_log(), under(r"logs\dsa-app-window.log"));
    assert_eq!(user.compositor_settings(), under("compositor.json"));
    assert_eq!(user.monitors(), under("monitors.txt"));
    assert_eq!(user.start_menu_note(), under("start-menu.json"));
    assert_eq!(user.tmp(), under("tmp"));
    assert_eq!(user.print_spool(), under(r"tmp\print"));
    assert_eq!(
        UserRoot::in_profile(Path::new(r"C:\Users\alice")),
        user,
        "a profile whose local application data was not moved"
    );
}

#[test]
fn fr19_temporary_names_are_unique_and_leftovers_of_other_runs_are_cleared() {
    let tmp = scratch("tmp");
    let (a, b) = (unique_name("vm", ".initrd"), unique_name("vm", ".initrd"));
    assert_ne!(a, b);
    assert!(a.contains(&format!("-{}-", std::process::id())), "{a}");
    std::fs::write(tmp.join(&a), b"mine").unwrap();
    std::fs::write(tmp.join("vm-1-0.initrd"), b"an earlier run's").unwrap();
    std::fs::create_dir_all(tmp.join("left-1-2")).unwrap();
    assert!(clear_tmp(&tmp).is_empty());
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec![a], "only this process's own file stays");
}

#[test]
fn fr19_moving_an_old_file_never_replaces_the_one_in_its_new_place() {
    let dir = scratch("migrate");
    let (old, new) = (dir.join("old.vhdx"), dir.join("new").join("android.vhdx"));
    assert_eq!(migrate(&old, &new), Moved::Nothing);

    std::fs::write(&old, b"old disk").unwrap();
    assert_eq!(migrate(&old, &new), Moved::Moved);
    assert_eq!(std::fs::read(&new).unwrap(), b"old disk");
    assert!(!old.exists());

    std::fs::write(&old, b"another disk").unwrap();
    assert!(matches!(migrate(&old, &new), Moved::Left(_)));
    assert_eq!(std::fs::read(&new).unwrap(), b"old disk", "never replaced");
    assert_eq!(
        std::fs::read(&old).unwrap(),
        b"another disk",
        "never removed"
    );
}

#[test]
fn fr19_a_disk_the_first_releases_kept_for_the_machine_moves_into_the_users_root() {
    let dir = scratch("user-disk");
    let machine = MachineRoot::at(dir.join("machine"));
    let user = UserRoot::in_local_app_data(&dir.join("local"));
    let sid = "S-1-5-21-1-2-3-1001";
    let old = machine.old_user_disk(sid);
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"disk").unwrap();
    std::fs::write(old.with_extension("image-version"), b"16.0.0-1").unwrap();

    let notes = migrate_user_disk(&machine, &user, sid);
    assert!(notes.iter().any(|n| n.contains("moved")), "{notes:?}");
    assert_eq!(std::fs::read(user.android_disk()).unwrap(), b"disk");
    assert_eq!(
        std::fs::read(user.android_disk_version()).unwrap(),
        b"16.0.0-1"
    );
    assert!(!old.parent().unwrap().exists(), "the empty old folder goes");

    // A disk already in the user's root wins, and the old one stays where it is.
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"older disk").unwrap();
    let notes = migrate_user_disk(&machine, &user, sid);
    assert!(notes.iter().any(|n| n.contains("stays")), "{notes:?}");
    assert_eq!(std::fs::read(user.android_disk()).unwrap(), b"disk");
    assert_eq!(std::fs::read(&old).unwrap(), b"older disk");
}

#[test]
fn fr19_uninstalling_leaves_only_the_users_android_disk() {
    let dir = scratch("uninstall-user");
    let user = UserRoot::in_local_app_data(&dir.join("local"));
    let programs = dir.join("programs");
    std::fs::create_dir_all(user.apps().join("org.example")).unwrap();
    std::fs::create_dir_all(user.logs()).unwrap();
    std::fs::create_dir_all(user.notifications()).unwrap();
    std::fs::create_dir_all(&programs).unwrap();
    std::fs::write(user.android_disk(), b"disk").unwrap();
    std::fs::write(user.android_disk_version(), b"16").unwrap();
    std::fs::write(user.window_manager_log(), b"log").unwrap();
    std::fs::write(user.compositor_settings(), b"{}").unwrap();
    std::fs::write(user.monitors(), b"1920x1080").unwrap();
    std::fs::write(programs.join("Notes.lnk"), b"lnk").unwrap();
    std::fs::write(programs.join("Other.lnk"), b"not ours").unwrap();
    std::fs::write(
        user.start_menu_note(),
        r#"{"shortcuts": {"org.example": "Notes.lnk"}}"#,
    )
    .unwrap();
    for marker in user.old_trace_markers() {
        std::fs::write(marker, b"").unwrap();
    }

    assert_eq!(remove_user_files(&user, &programs), Vec::<String>::new());
    let mut left: Vec<_> = std::fs::read_dir(user.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, vec!["android.image-version", "android.vhdx"]);
    assert!(!programs.join("Notes.lnk").exists());
    assert!(
        programs.join("Other.lnk").exists(),
        "a shortcut DSA did not make stays"
    );

    let empty = UserRoot::in_local_app_data(&dir.join("nobody"));
    std::fs::create_dir_all(empty.logs()).unwrap();
    remove_user_files(&empty, &programs);
    assert!(!empty.dir().exists(), "a folder with no disk goes entirely");
}

#[test]
fn fr19_old_trace_switch_files_become_settings() {
    let dir = scratch("markers");
    let machine = MachineRoot::at(dir.join("machine"));
    let user = UserRoot::in_local_app_data(&dir.join("local"));
    std::fs::create_dir_all(machine.dir()).unwrap();
    std::fs::create_dir_all(user.dir()).unwrap();
    let mut markers = machine.old_trace_markers().to_vec();
    markers.extend(user.old_trace_markers());
    assert_eq!(old_trace_markers(&markers), OldTraceMarkers::default());

    std::fs::write(&user.old_trace_markers()[0], b"").unwrap();
    let old = old_trace_markers(&markers);
    assert!(old.input_trace && !old.resize_trace);
    assert_eq!(old.found, vec![user.old_trace_markers()[0].clone()]);

    std::fs::write(&machine.old_trace_markers()[1], b"").unwrap();
    let old = old_trace_markers(&markers);
    assert!(old.input_trace && old.resize_trace);
    assert_eq!(old.found.len(), 2);
}

#[test]
fn fr19_uninstalling_removes_what_the_service_made_but_an_unmoved_disk() {
    let dir = scratch("uninstall-machine");
    let machine = MachineRoot::at(dir.clone());
    for folder in [machine.settings(), machine.logs(), machine.tmp()] {
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("file"), b"x").unwrap();
    }
    for marker in machine.old_trace_markers() {
        std::fs::write(marker, b"").unwrap();
    }
    let disk = machine.old_user_disk("S-1-5-21-1");
    std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
    std::fs::write(&disk, b"disk").unwrap();

    assert_eq!(remove_machine_files(&machine), Vec::<String>::new());
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["users"]);
    assert!(disk.exists());
}

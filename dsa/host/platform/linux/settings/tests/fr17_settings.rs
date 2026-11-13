//! DSA Settings for Linux, without a display: the requests it makes through the local API,
//! what it reads from the answers, what its pages show, its translations and its desktop
//! files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dsa_settings_linux::api::{Dsa, Failure, Problem};
use dsa_settings_linux::model::{Client, LogLine, Memory, Outcome, Settings, State, Status};
use dsa_settings_linux::pages::Page;
use dsa_settings_linux::view;
use serde_json::json;

/// A stand-in for the service: it records each command line and answers with `answer`.
struct Fake {
    calls: Mutex<Vec<Vec<String>>>,
    answer: Mutex<Result<String, Failure>>,
}

impl Fake {
    fn new(answer: serde_json::Value) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Ok(answer.to_string())),
        })
    }

    fn failing(failure: Failure) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Err(failure)),
        })
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

impl Dsa for Fake {
    fn run(&self, args: &[String]) -> Result<String, Failure> {
        self.calls.lock().unwrap().push(args.to_vec());
        self.answer.lock().unwrap().clone()
    }
}

fn line(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn client(fake: &Arc<Fake>, elevated: &Arc<Fake>) -> Client {
    Client::new(fake.clone(), elevated.clone(), true)
}

fn settings_json() -> serde_json::Value {
    json!({
        "render_gpu": "display", "freeze": false, "clipboard": true,
        "program_updates": false, "image_updates": true,
        "prerelease_updates": true, "update_channel_available": true,
        "image_updates_available": true, "version": "0.1.0",
        "log_dir": "/var/lib/dsa/logs", "android_version": "17.0.0-1",
        "developer_mode": false, "adb_port": 58526, "memory_limit_mb": 0,
        "shared_folders": ["Documents", "Pictures"], "advanced_networking": false,
        "network": { "mode": "nat", "addresses": ["10.0.2.15"], "dns_servers": ["10.0.2.3"],
                     "gateway": "10.0.2.2", "problem": null }
    })
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn fr17_the_app_has_every_page_the_windows_settings_app_has() {
    let main_window = std::fs::read_to_string(
        repo().join("dsa/host/platform/windows/settings/DsaSettings/MainWindow.xaml"),
    )
    .unwrap();
    // Each page is a pane item: the app's own `PaneItem` (a NavigationViewItem
    // that names itself to UI Automation) or a plain NavigationViewItem.
    let windows_pages = main_window.matches("<l:PaneItem ").count()
        + main_window.matches("<ui:NavigationViewItem ").count();
    assert_eq!(
        Page::ALL.len(),
        windows_pages,
        "the Windows settings app has {windows_pages} pages and this one {}",
        Page::ALL.len()
    );
    for page in Page::ALL {
        assert_eq!(Page::from_id(page.id()), Some(page));
    }
}

#[test]
fn fr17_changes_go_through_the_local_api_with_the_same_command_lines() {
    let fake = Fake::new(settings_json());
    let elevated = Fake::new(settings_json());
    let c = client(&fake, &elevated);
    c.set("render-gpu", "integrated").unwrap();
    c.switch("freeze", true).unwrap();
    c.set_memory_limit(0).unwrap();
    c.set_memory_limit(8192).unwrap();
    c.set_shared_folders(&[]).unwrap();
    c.set_shared_folders(&["Documents".into(), "Music".into()])
        .unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            line(&["settings", "set", "render-gpu", "integrated", "--json"]),
            line(&["settings", "set", "freeze", "on", "--json"]),
            line(&["settings", "set", "memory-limit", "auto", "--json"]),
            line(&["settings", "set", "memory-limit", "8192", "--json"]),
            line(&["settings", "set", "shared-folders", "none", "--json"]),
            line(&[
                "settings",
                "set",
                "shared-folders",
                "Documents,Music",
                "--json"
            ]),
        ]
    );
    assert!(elevated.calls().is_empty());
}

#[test]
fn fr17_settings_of_every_user_ask_for_an_administrator() {
    let fake = Fake::new(settings_json());
    let elevated = Fake::new(settings_json());
    let c = client(&fake, &elevated);
    c.set_machine("image-updates", false).unwrap();
    c.set_machine("input-trace", true).unwrap();
    c.use_android("16", true).ok();
    assert!(fake.calls().is_empty());
    assert_eq!(
        elevated.calls(),
        vec![
            line(&["settings", "set", "image-updates", "off", "--json"]),
            line(&["settings", "set", "input-trace", "on", "--json"]),
            line(&["android", "use", "16", "--erase-data", "--json"]),
        ]
    );

    // An administrator's app asks the service directly.
    let direct = Client::new(fake.clone(), elevated.clone(), false);
    direct.set_machine("resize-trace", true).unwrap();
    assert_eq!(
        fake.calls(),
        vec![line(&["settings", "set", "resize-trace", "on", "--json"])]
    );
}

#[test]
fn fr17_the_windows_settings_app_makes_the_same_requests() {
    let service = std::fs::read_to_string(
        repo().join("dsa/host/platform/windows/settings/DsaSettings.Core/DsaService.cs"),
    )
    .unwrap();
    for request in [
        r#""start", "--wait", "--timeout", "360""#,
        r#""stop", "--wait", "--timeout", "120""#,
        r#""settings", "set", name, value"#,
        r#""reset-data", "--yes""#,
        r#""files", "open""#,
        r#""widgets", "list""#,
        r#""background", "list""#,
        r#""logs", "collect", "-o", path"#,
    ] {
        assert!(
            service.contains(request),
            "DsaService.cs no longer makes {request}"
        );
    }
    let fake = Fake::new(json!({}));
    let c = client(&fake, &fake);
    c.start().unwrap();
    c.stop().unwrap();
    c.reset_data().unwrap();
    c.open_android_files().unwrap();
    c.widgets().unwrap();
    c.background().unwrap();
    c.app_windows().unwrap();
    c.set_app_window(
        "com.example",
        true,
        Some((412, 915)),
        dsa_settings_linux::model::DpiScaling::Auto,
    )
    .unwrap();
    c.set_app_window(
        "com.example",
        false,
        None,
        dsa_settings_linux::model::DpiScaling::System,
    )
    .unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            line(&["start", "--wait", "--timeout", "360", "--json"]),
            line(&["stop", "--wait", "--timeout", "120", "--json"]),
            line(&["reset-data", "--yes", "--json"]),
            line(&["files", "open", "--json"]),
            line(&["widgets", "list", "--json"]),
            line(&["background", "list", "--json"]),
            line(&["app-window", "--json"]),
            line(&[
                "app-window",
                "com.example",
                "--fixed-size",
                "on",
                "--start-size",
                "412x915",
                "--dpi-scaling",
                "auto",
                "--json"
            ]),
            line(&[
                "app-window",
                "com.example",
                "--fixed-size",
                "off",
                "--start-size",
                "auto",
                "--dpi-scaling",
                "system",
                "--json"
            ]),
        ]
    );
}

#[test]
fn fr17_the_device_identity_is_set_and_reset_through_the_service() {
    let fake = Fake::new(json!({
        "chosen": { "model": "Pixel" },
        "effective": { "model": "Pixel", "brand": "DSA" }
    }));
    let c = client(&fake, &fake);
    let identity = c.set_identity("model", "Pixel").unwrap();
    assert_eq!(identity.chosen("model"), "Pixel");
    assert_eq!(identity.effective("brand"), "DSA");
    c.set_identity("fingerprint.tags", "").unwrap();
    c.reset_identity().unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            line(&["settings", "set", "device.model", "Pixel", "--json"]),
            line(&["settings", "reset", "device.fingerprint.tags", "--json"]),
            line(&["settings", "reset", "device", "--json"]),
        ]
    );
    assert!(view::identity_problem("brand", "has space").is_some());
    assert!(view::identity_problem("brand", "").is_none());
    assert!(view::identity_problem("brand", "DSA").is_none());
}

#[test]
fn fr17_the_identity_page_shows_the_computers_values_and_where_defaults_came_from() {
    let identity = dsa_settings_linux::model::Identity::read(&json!({
        "chosen": { "model": "" },
        "effective": {
            "manufacturer": "Samsung", "model": "NT960XGP-G72AG", "device": "dsa_x86_64",
            "model_name": "Galaxy Book4 Ultra", "device_name": "BREW-BOOK",
            "soc_manufacturer": "Intel", "soc_model": ""
        },
        "sources": {
            "manufacturer": "computer", "model": "computer", "device": "dsa",
            "model_name": "computer", "device_name": "computer",
            "soc_manufacturer": "computer", "soc_model": "dsa"
        }
    }));
    assert_eq!(identity.effective("model"), "NT960XGP-G72AG");
    assert_eq!(identity.origin("model"), "computer");
    assert_eq!(identity.origin("device"), "dsa");
    assert_eq!(
        identity.origin("fingerprint.type"),
        "dsa",
        "not named: DSA's"
    );
    assert_eq!(identity.computer("model_name"), "Galaxy Book4 Ultra");
    assert_eq!(identity.computer("device_name"), "BREW-BOOK");
    assert_eq!(identity.origin("device_name"), "computer");
    assert_eq!(
        view::computer_value(identity.computer("soc_manufacturer")),
        "Intel"
    );
    assert_ne!(view::computer_value(identity.computer("soc_model")), "");
    assert_ne!(view::origin_label("computer"), view::origin_label("dsa"));
    assert_ne!(
        view::identity_label("device_name"),
        view::identity_label("fingerprint")
    );

    // A service from before the computer's identity: every default is DSA's.
    let old = dsa_settings_linux::model::Identity::read(&json!({
        "chosen": {}, "effective": { "model": "Desktop Subsystem for Android" }
    }));
    assert_eq!(old.origin("model"), "dsa");
    assert_eq!(old.computer("device_name"), "");
}

#[test]
fn fr17_settings_are_read_as_the_service_reports_them() {
    let settings = Settings::read(&settings_json());
    assert!(settings.image_updates);
    assert!(settings.clipboard);
    assert_eq!(settings.shared_folders, vec!["Documents", "Pictures"]);
    assert_eq!(settings.adb_connect(), "adb connect 127.0.0.1:58526");
    assert_eq!(
        settings.network.as_ref().unwrap().addresses,
        vec!["10.0.2.15"]
    );

    // A service from before a switch existed leaves it at its default.
    let old = Settings::read(&json!({ "render_gpu": "display" }));
    assert!(old.clipboard);
    assert!(old.file_sharing);
    assert_eq!(old.shared_folders.len(), 6);
    assert_eq!(old.adb_port, 58526);
}

#[test]
fn fr17_the_general_page_offers_what_the_state_allows() {
    let status = |state: &str| Status::read(&json!({ "state": state }));
    let stopped = view::controls(Some(&status("Stopped")));
    assert!(stopped.can_start && !stopped.can_stop && !stopped.can_freeze);
    let active = view::controls(Some(&status("Active")));
    assert!(!active.can_start && active.can_stop && active.can_freeze);
    let frozen = view::controls(Some(&status("Frozen")));
    assert!(frozen.can_stop && !frozen.can_freeze);
    assert_eq!(status("Dozing").state, State::Dozing);
    assert_eq!(status("nonsense").state, State::Unknown);
    assert_eq!(view::controls(None), view::controls(Some(&status("?"))));
}

#[test]
fn nfr2_the_memory_page_counts_zram_compressed_and_google_play_services_apart() {
    let memory = Memory::read(&json!({
        "host": [], "host_total_kb": 0,
        "guest": {
            "mem_total_kb": 4194304, "mem_free_kb": 1048576, "total_pss_kb": 512000,
            "swap_pss_kb": 307200, "zram_original_kb": 307200, "zram_used_kb": 102400,
            "apps": { "resident_kb": 204800, "compressed_kb": 102400, "swapped_out_kb": 307200,
                      "total_kb": 307200 },
            "google_play": { "processes": 2, "pss_kb": 138240, "swap_pss_kb": 125952,
                             "resident_kb": 12288, "compressed_kb": 41984,
                             "swapped_out_kb": 125952, "total_kb": 54272 },
            "top_processes": [
                { "pid": 700, "name": "com.google.android.gms", "pss_kb": 82944,
                  "swap_pss_kb": 76800, "google_play": true, "resident_kb": 6144,
                  "compressed_kb": 25600 },
                { "pid": 42, "name": "system_server", "pss_kb": 153600, "swap_pss_kb": 0,
                  "google_play": false, "resident_kb": 153600, "compressed_kb": 0 }
            ],
            "age_seconds": 12
        }
    }));
    let guest = memory.guest.as_ref().unwrap();
    let rows = view::guest_rows(guest);
    assert_eq!(rows[0].size, "300 MB");
    assert_eq!(rows[0].detail, "200 MB in memory, 100 MB compressed");
    assert_eq!(rows[1].label, "Google Play services");
    assert_eq!(rows[1].size, "53 MB");
    assert_eq!(
        rows[1].detail,
        "The Google apps you installed, 2 processes: 12 MB in memory, 41 MB compressed"
    );
    let processes = view::process_rows(guest);
    assert_eq!(processes[0].size, "31 MB");
    assert_eq!(processes[0].detail, "Process 700, Google Play services");
    assert_eq!(processes[1].detail, "Process 42");

    let unmeasured = Memory::read(&json!({
        "host": [], "host_total_kb": 0,
        "guest": {
            "mem_total_kb": 4194304, "total_pss_kb": 204800, "swap_pss_kb": 102400,
            "apps": { "resident_kb": 102400, "compressed_kb": null, "swapped_out_kb": 102400,
                      "total_kb": 102400 },
            "google_play": null, "top_processes": [], "age_seconds": 1
        }
    }));
    let rows = view::guest_rows(unmeasured.guest.as_ref().unwrap());
    assert_eq!(rows[0].size, "100 MB");
    assert_eq!(rows[0].detail, "100 MB in memory, unknown compressed");
    assert!(rows.iter().all(|row| row.label != "Google Play services"));
}

#[test]
fn fr17_the_memory_page_shows_the_vm_and_the_guest_breakdown() {
    let memory = Memory::read(&json!({
        "host": [
            { "role": "vm", "pid": 10, "working_set_kb": 1048576, "private_kb": 1000 },
            { "role": "window manager", "pid": 11, "working_set_kb": 2048, "private_kb": 1024 },
            { "role": "graphics renderer", "pid": 12, "working_set_kb": 4096, "private_kb": 2048 }
        ],
        "host_total_kb": 1050624,
        "vm_beyond_guest_kb": 524288,
        "guest": {
            "mem_total_kb": 4194304, "mem_available_kb": 3145728, "mem_free_kb": 1048576,
            "total_pss_kb": 819200, "cached_kb": 100, "buffers_kb": 24,
            "kernel": { "slab_unreclaimable_kb": 10, "reserved_at_boot_kb": 2048,
                        "mem_online_kb": 4194304, "cma_total_kb": 0 },
            "top_processes": [ { "pid": 500, "name": "system_server", "pss_kb": 204800 } ],
            "age_seconds": 30
        }
    }));
    assert_eq!(view::vm_size_kb(&memory), Some(1048576));
    let host = view::host_rows(&memory);
    assert_eq!(host.len(), 3);
    assert_eq!(host[0].size, "1.0 GB");
    // The graphics renderer runs in a process of its own and shows as one.
    assert_eq!(host[2].label, "Graphics renderer");
    let guest = memory.guest.as_ref().unwrap();
    let rows = view::guest_rows(guest);
    assert_eq!(rows[0].size, "800 MB");
    assert_eq!(rows[1].size, "124 KB");
    assert_eq!(view::kernel_rows(guest).len(), 7);
    assert_eq!(view::process_rows(guest)[0].label, "system_server");
    assert!((view::guest_used_fraction(guest) - 0.25).abs() < 1e-9);

    let limits = view::memory_limit_choices(5000);
    assert_eq!(limits[0].1, 0);
    assert!(limits.iter().any(|(_, mb)| *mb == 5000));
    assert!(limits.windows(2).skip(1).all(|w| w[0].1 < w[1].1));
}

#[test]
fn fr17_the_android_page_warns_before_moving_to_an_older_version() {
    let info = dsa_settings_linux::model::AndroidVersion::read(&json!({
        "version": "17.0.0-1", "android_major": 17, "channel": "latest",
        "supported_majors": [16, 17], "target_major": 17
    }));
    let choices = view::android_choices(&info);
    let channels: Vec<&str> = choices.iter().map(|c| c.channel.as_str()).collect();
    assert_eq!(channels, vec!["latest", "17", "16"]);
    assert_eq!(view::android_choice_index(&info), 0);
    assert_eq!(
        view::android_move(&info, &choices[1]),
        view::AndroidMove::Same
    );
    assert_eq!(
        view::android_move(&info, &choices[2]),
        view::AndroidMove::Down
    );
}

#[test]
fn fr17_the_apps_page_searches_and_reads_refusals() {
    let apps: Vec<_> = [("com.b", "Beta"), ("com.a", "alpha"), ("org.c", "Gamma")]
        .iter()
        .map(|(package, label)| {
            dsa_settings_linux::model::App::read(&json!({ "package": package, "label": label }))
        })
        .collect();
    let all: Vec<&str> = view::filter_apps(&apps, "")
        .iter()
        .map(|a| a.label.as_str())
        .collect();
    assert_eq!(all, vec!["alpha", "Beta", "Gamma"]);
    assert_eq!(view::filter_apps(&apps, "COM.").len(), 2);
    assert_eq!(view::apps_count(3, 2, true), "2 of 3 apps");

    let refused = Outcome::refusal(&Failure::new(
        Problem::Refused,
        "not uninstalled (system-app): it is part of Android",
    ))
    .unwrap();
    assert_eq!(refused.status, "system-app");
    assert!(Outcome::refusal(&Failure::new(Problem::Refused, "something else")).is_none());

    let fake = Fake::failing(Failure::new(
        Problem::Refused,
        "not installed (downgrade): newer version installed",
    ));
    let outcome = client(&fake, &fake).install("/tmp/a.apk").unwrap();
    assert!(!outcome.done);
    assert_eq!(outcome.status, "downgrade");

    assert_eq!(view::start_size_index(None), Some(0));
    let phone = dsa_settings_linux::model::AppWindow::read(
        &json!({ "package": "a", "start_size": [412, 915] }),
    );
    assert_eq!(view::start_size_index(Some(&phone)), Some(1));
}

#[test]
fn fr17_problems_are_told_apart() {
    let unreachable =
        Failure::from_client("dsa: cannot reach the DSA service at /run/dsa/host.sock");
    assert_eq!(unreachable.problem, Problem::ServiceUnreachable);
    assert!(!unreachable.detail.starts_with("dsa: "));
    assert_eq!(Failure::from_client("no").problem, Problem::Refused);
    let fake = Fake::new(json!(null));
    let fake2 = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        answer: Mutex::new(Ok("not json".into())),
    });
    assert!(client(&fake, &fake).status().is_ok());
    assert_eq!(
        client(&fake2, &fake2).status().unwrap_err().problem,
        Problem::Unreadable
    );
}

#[test]
fn fr17_shared_folders_keep_the_service_order() {
    let now = vec!["Pictures".to_string(), "Documents".to_string()];
    assert_eq!(
        view::toggle_folder(&now, "Downloads", true),
        vec!["Documents", "Downloads", "Pictures"]
    );
    assert_eq!(
        view::toggle_folder(&now, "Pictures", false),
        vec!["Documents"]
    );
}

#[test]
fn fr21_the_log_page_uses_the_app_font() {
    let page = std::fs::read_to_string(crate_dir().join("src/ui/logs.rs")).unwrap();
    assert!(
        !page.contains("monospace"),
        "the log page sets a monospace font instead of the app's own"
    );
}

#[test]
fn fr21_a_log_line_splits_into_the_page_columns_and_a_dropped_record_has_only_its_body() {
    let record = json!({ "time": "2026-11-04 10:00:00.000", "priority": "I",
                         "tag": "ActivityManager", "pid": 12, "tid": 13, "message": "시작" });
    let columns = LogLine::parse(&record.to_string()).unwrap().columns();
    assert_eq!(columns.time, "2026-11-04 10:00:00.000");
    assert_eq!(columns.pid, "12");
    assert_eq!(columns.tid, "13");
    assert_eq!(columns.priority, "I");
    assert_eq!(columns.body, "ActivityManager: 시작");

    let record = json!({ "time": "2026-11-04 10:00:00.000", "priority": "W", "dropped": 3 });
    let dropped = LogLine::parse(&record.to_string()).unwrap().columns();
    assert_eq!(
        dropped.time.clone() + &dropped.pid + &dropped.tid + &dropped.priority,
        ""
    );
    assert_eq!(dropped.body, "--------- 3 records dropped");
}

#[test]
fn fr21_copying_selected_log_lines_gives_each_whole_line_in_page_order() {
    let record = |tag: &str, message: &str| {
        let json = json!({ "time": "2026-11-04 10:00:00.000", "priority": "I", "tag": tag,
                           "pid": 12, "tid": 13, "message": message });
        LogLine::parse(&json.to_string()).unwrap()
    };
    let dropped = LogLine::parse(&json!({ "dropped": 3 }).to_string()).unwrap();
    let shown = vec![
        record("A", "one"),
        record("B", "two"),
        record("C", "three"),
        dropped,
    ];
    assert_eq!(
        view::log_copy_text(&shown, &[3, 0, 2, 0, 9]),
        "2026-11-04 10:00:00.000    12    13 I A: one\n\
         2026-11-04 10:00:00.000    12    13 I C: three\n\
         --------- 3 records dropped"
    );
    assert_eq!(view::log_copy_text(&shown, &[]), "");

    let page = std::fs::read_to_string(crate_dir().join("src/ui/logs.rs")).unwrap();
    assert!(
        page.contains("SelectionMode::Multiple")
            && page.contains("<Control>c")
            && page.contains("\"log.copy\""),
        "the log page cannot select several lines or copy them with Ctrl+C and its menu"
    );
}

#[test]
fn fr21_the_log_page_shows_lines_in_columns() {
    let page = std::fs::read_to_string(crate_dir().join("src/ui/logs.rs")).unwrap();
    assert!(
        page.contains("line.columns()") && !page.contains("TextView"),
        "the log page shows each line as one text instead of in columns"
    );
}

#[test]
fn fr17_the_log_page_filters_pauses_and_keeps_the_newest_lines() {
    let record = |message: &str| {
        json!({ "time": "10:00:00.000", "priority": "I", "tag": "Tag", "pid": 1, "tid": 2,
                "message": message })
        .to_string()
    };
    let parsed = LogLine::parse(&record("hello")).unwrap();
    assert_eq!(parsed.text(), "10:00:00.000     1     2 I Tag: hello");
    let mut log = view::LogView::default();
    assert!(log.add(&record("one")));
    assert!(!log.add("not a record"));
    log.set_paused(true);
    assert!(!log.add(&record("two")));
    assert_eq!(log.shown().len(), 1);
    assert!(log.paused_text().contains('1'));
    log.set_paused(false);
    assert_eq!(log.shown().len(), 2);
    log.filter = "TWO".into();
    assert_eq!(log.shown().len(), 1);
    log.filter.clear();
    for i in 0..view::LogView::LIMIT + 10 {
        log.add(&record(&i.to_string()));
    }
    assert_eq!(log.shown().len(), view::LogView::LIMIT);
    assert_eq!(
        view::logcat_args("W"),
        vec!["logcat".to_string(), "--json".into(), "*:W".into()]
    );
}

/// Every string literal the pages show: one with a letter that has a space or starts with a
/// capital, and is not a format string or a name in capitals. A `\n` in a literal is the line
/// break it stands for; a literal with any other escape is not read.
fn shown_strings() -> Vec<String> {
    let src = crate_dir().join("src");
    let mut files = vec![src.join("view.rs"), src.join("pages.rs")];
    let mut ui: Vec<PathBuf> = std::fs::read_dir(src.join("ui"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    ui.sort();
    files.extend(ui);
    let mut found = Vec::new();
    for file in files {
        for code in std::fs::read_to_string(&file).unwrap().lines() {
            let trimmed = code.trim_start();
            if trimmed.starts_with("//") || code.contains("// not translated") {
                continue;
            }
            let mut parts = code.split('"');
            parts.next();
            while let Some(literal) = parts.next() {
                parts.next();
                let literal = literal.replace("\\n", "\n");
                let literal = literal.as_str();
                let shown = literal.chars().any(|c| c.is_ascii_alphabetic())
                    && literal.chars().count() >= 2
                    && !literal.starts_with('-')
                    && !literal.contains('\\')
                    && !literal
                        .split('{')
                        .skip(1)
                        .any(|after| !after.starts_with(|c: char| c.is_ascii_digit()))
                    && !literal
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    && (literal.contains(' ') || literal.starts_with(|c: char| c.is_uppercase()));
                if shown && !found.iter().any(|f| f == literal) {
                    found.push(literal.to_string());
                }
            }
        }
    }
    found
}

/// The translations of a `.po` file of single-line entries.
fn catalogue(path: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap();
    let unquote = |s: &str| -> String {
        let s = s.trim();
        serde_json::from_str::<String>(s).unwrap_or_else(|e| panic!("{s} in {path:?}: {e}"))
    };
    let mut entries = BTreeMap::new();
    let mut id: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("msgid ") {
            id = Some(unquote(rest));
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            let id = id.take().expect("msgstr without msgid");
            assert!(
                entries.insert(id.clone(), unquote(rest)).is_none(),
                "{id:?} is in {path:?} twice"
            );
        }
    }
    entries
}

fn placeholders(text: &str) -> Vec<String> {
    let mut found: Vec<String> = (0..10)
        .map(|i| format!("{{{i}}}"))
        .filter(|p| text.contains(p.as_str()))
        .collect();
    found.sort();
    found
}

/// Whether `text` names a size, such as "3 GB" or "16GiB".
fn names_a_size(text: &str) -> bool {
    ["GB", "GiB"].iter().any(|unit| {
        text.match_indices(unit).any(|(at, _)| {
            text[..at]
                .trim_end()
                .ends_with(|c: char| c.is_ascii_digit())
        })
    })
}

#[test]
fn fr17_automatic_memory_limit_names_no_fixed_size() {
    let shown = shown_strings();
    let english = shown
        .iter()
        .find(|text| text.starts_with("The most memory Android may grow to."))
        .expect("the memory limit's description is on a page");
    let ko = catalogue(&crate_dir().join("po/ko.po"));
    let korean = ko.get(english).expect("the description has a Korean text");
    for text in [english, korean] {
        assert!(
            !names_a_size(text),
            "this names a size, which reads as a limit on Automatic: {text}"
        );
        assert!(
            text.contains("Linux"),
            "this should say that Automatic stops at the computer's memory less what Linux keeps: {text}"
        );
    }
    assert!(english.contains("no fixed limit"), "{english}");
    assert!(korean.contains("고정된 한도가 없"), "{korean}");
}

#[test]
fn fr17_every_string_is_translated_into_korean() {
    let ko = catalogue(&crate_dir().join("po/ko.po"));
    let mut wanted = shown_strings();
    wanted.extend(Page::ALL.iter().map(|p| p.title().to_string()));
    wanted.extend(
        dsa_settings_linux::model::ALL_FOLDERS
            .iter()
            .map(|f| f.to_string()),
    );
    wanted.push("OK".into());
    let mut missing = Vec::new();
    for text in &wanted {
        match ko.get(text) {
            Some(korean) if !korean.is_empty() => assert_eq!(
                placeholders(text),
                placeholders(korean),
                "the Korean of {text:?} has other placeholders"
            ),
            _ => missing.push(text.clone()),
        }
    }
    assert!(
        missing.is_empty(),
        "po/ko.po has no Korean for {missing:#?}"
    );
    let linguas = std::fs::read_to_string(crate_dir().join("po/LINGUAS")).unwrap();
    assert!(linguas.lines().any(|l| l.trim() == "ko"));
}

/// The most columns one paragraph (the text between two line breaks) of a shown string may
/// take, in English and in Korean alike. A longer one is a wall of text in a row or a dialog.
const PARAGRAPH_COLUMNS: usize = 140;

/// How many columns `text` takes on screen: an East Asian wide character, such as a Hangul
/// syllable, takes two and every other character one. A placeholder counts as written.
fn columns(text: &str) -> usize {
    text.chars()
        .map(|c| match u32::from(c) {
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFF60 => 2,
            _ => 1,
        })
        .sum()
}

/// The paragraphs of `text` that take more than [`PARAGRAPH_COLUMNS`].
fn long_paragraphs(text: &str) -> Vec<(usize, String)> {
    text.split('\n')
        .map(|paragraph| (columns(paragraph), paragraph.to_string()))
        .filter(|(width, _)| *width > PARAGRAPH_COLUMNS)
        .collect()
}

#[test]
fn fr17_no_paragraph_of_a_shown_string_is_too_long() {
    let ko = catalogue(&crate_dir().join("po/ko.po"));
    let mut english: Vec<String> = ko.keys().filter(|id| !id.is_empty()).cloned().collect();
    english.extend(shown_strings());
    let mut too_long = Vec::new();
    for text in &english {
        for (width, paragraph) in long_paragraphs(text) {
            too_long.push(format!("English, {width} columns: {paragraph}"));
        }
    }
    for (id, korean) in &ko {
        if id.is_empty() {
            continue;
        }
        for (width, paragraph) in long_paragraphs(korean) {
            too_long.push(format!("Korean, {width} columns: {paragraph}"));
        }
    }
    assert!(
        too_long.is_empty(),
        "these paragraphs take more than {PARAGRAPH_COLUMNS} columns; break them into short \
         paragraphs or lines:\n{}",
        too_long.join("\n")
    );
}

#[test]
fn fr17_korean_has_the_paragraphs_of_the_english() {
    let ko = catalogue(&crate_dir().join("po/ko.po"));
    let mut differ = Vec::new();
    for (english, korean) in &ko {
        if english.is_empty() {
            continue;
        }
        let breaks = |text: &str| text.matches('\n').count();
        let blank_lines = |text: &str| text.matches("\n\n").count();
        let bullets = |text: &str| text.lines().filter(|l| l.starts_with("• ")).count();
        if korean.is_empty()
            || breaks(english) != breaks(korean)
            || blank_lines(english) != blank_lines(korean)
            || bullets(english) != bullets(korean)
        {
            differ.push(format!("{english:?}\n  {korean:?}"));
        }
    }
    assert!(
        differ.is_empty(),
        "the Korean of these does not break into the same paragraphs, lines and list items:\n{}",
        differ.join("\n")
    );
}

/// The page files: the ones that build a settings page, or a part of one, from groups and
/// rows. The log page is a list of log lines, not of settings rows, and lays itself out.
fn page_sources() -> Vec<(String, String)> {
    let ui = crate_dir().join("src/ui");
    let mut pages: Vec<(String, String)> = std::fs::read_dir(&ui)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "common.rs"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .filter(|(_, code)| code.contains("Page::new()") || code.contains("&Rc<Page>"))
        .collect();
    pages.sort();
    pages
}

#[test]
fn fr17_pages_take_their_rows_and_spacing_from_the_shared_helpers() {
    let pages = page_sources();
    assert!(
        ["apps.rs", "backup.rs", "status.rs", "system.rs"]
            .iter()
            .all(|name| pages.iter().any(|(page, _)| page == name)),
        "the settings pages were not found: {:?}",
        pages.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    // The rows a page may not build itself: common.rs makes each of them, so every row has the
    // same title handling and the spacing libadwaita gives it.
    let rows = [
        "adw::ActionRow::",
        "adw::SwitchRow::",
        "adw::ComboRow::",
        "adw::EntryRow::",
        "adw::ExpanderRow::",
        "adw::SpinRow::",
        "adw::PasswordEntryRow::",
        "adw::ButtonRow::",
    ];
    // What sets a widget's own spacing around it or inside it.
    let spacing = [
        "set_margin_",
        ".margin_top(",
        ".margin_bottom(",
        ".margin_start(",
        ".margin_end(",
        "padding",
    ];
    let mut found = Vec::new();
    for (name, code) in &pages {
        for (number, line) in code.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for mark in rows.iter().chain(spacing.iter()) {
                if line.contains(mark) {
                    found.push(format!("{name}:{}: {}", number + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "a page builds a row or sets spacing itself; use the helpers in src/ui/common.rs:\n{}",
        found.join("\n")
    );
    let common = std::fs::read_to_string(crate_dir().join("src/ui/common.rs")).unwrap();
    for helper in [
        "pub fn action_row(",
        "pub fn property_row(",
        "pub fn value_row(",
        "pub fn button_row(",
        "pub fn link_row(",
        "pub fn command_row(",
        "pub fn entry_row(",
        "pub fn expander_row(",
        "pub fn heading_block(",
    ] {
        assert!(common.contains(helper), "src/ui/common.rs has no {helper}");
    }
}

#[test]
fn fr17_text_never_names_another_platform() {
    for text in shown_strings() {
        for mark in ["Windows", "File Explorer", "Start menu", "taskbar", "vmmem"] {
            assert!(!text.contains(mark), "{text:?} names {mark}");
        }
    }
}

#[test]
fn fr17_the_desktop_entry_appstream_and_icons_are_there() {
    let data = crate_dir().join("data");
    let id = dsa_settings_linux::APP_ID;
    let desktop = std::fs::read_to_string(data.join(format!("{id}.desktop"))).unwrap();
    for key in [
        "[Desktop Entry]",
        "Type=Application",
        "Name=DSA Settings",
        "Name[ko]=",
        &format!("Icon={id}"),
        "Exec=/usr/lib/dsa/bin/dsa-settings",
        "Categories=",
    ] {
        assert!(desktop.contains(key), "the desktop entry has no {key}");
    }
    let metainfo = std::fs::read_to_string(data.join(format!("{id}.metainfo.xml"))).unwrap();
    assert!(metainfo.contains(&format!("<id>{id}</id>")));
    assert!(metainfo.contains(&format!(
        "<launchable type=\"desktop-id\">{id}.desktop</launchable>"
    )));
    assert!(metainfo.contains(&format!("<icon type=\"stock\">{id}</icon>")));
    assert!(metainfo.contains("Android is a trademark of Google LLC."));
    for icon in [
        format!("icons/hicolor/scalable/apps/{id}.svg"),
        format!("icons/hicolor/symbolic/apps/{id}-symbolic.svg"),
        "icons/hicolor/scalable/apps/io.github.thisisthepy.DSA.svg".into(),
        "icons/hicolor/symbolic/apps/io.github.thisisthepy.DSA-symbolic.svg".into(),
    ] {
        let svg =
            std::fs::read_to_string(data.join(&icon)).unwrap_or_else(|e| panic!("{icon}: {e}"));
        assert!(svg.contains("<svg"), "{icon} is not an SVG");
    }
}

#[test]
fn fr11_a_bundle_refused_before_android_sees_it_says_its_reason() {
    let refused = Outcome::refusal(&Failure {
        problem: Problem::Refused,
        detail: "not installed (app-bundle): notes.aab: this is an Android App Bundle".into(),
    })
    .expect("a refusal");
    assert_eq!(refused.status, "app-bundle");
    assert!(view::install_reason(&refused.status).contains("App Bundle"));
    let fallback = view::install_reason("other");
    for code in [
        "app-bundle",
        "encrypted-bundle",
        "split-signature-mismatch",
        "invalid-bundle",
        "unsupported-abi",
        "translation-off",
    ] {
        assert_ne!(view::install_reason(code), fallback, "{code}");
    }
}

#[test]
fn fr15_the_release_channel_is_read_and_chosen_as_an_administrator() {
    let fake = Fake::new(settings_json());
    let elevated = Fake::new(settings_json());
    let c = client(&fake, &elevated);
    let settings = c.settings().unwrap();
    assert!(settings.prerelease_updates);
    assert!(settings.update_channel_available);
    c.set_machine("prerelease-updates", false).unwrap();
    assert_eq!(
        elevated.calls(),
        vec![line(&[
            "settings",
            "set",
            "prerelease-updates",
            "off",
            "--json"
        ])]
    );
    // A service from before the channels reports neither: stable, and nothing to choose.
    let older = Settings::read(&json!({ "version": "0.1.0" }));
    assert!(!older.prerelease_updates);
    assert!(!older.update_channel_available);
}

#[test]
fn fr14_the_updates_page_shows_the_version_followed_and_the_download_with_its_progress() {
    let read = |value: serde_json::Value| dsa_settings_linux::model::AndroidVersion::read(&value);
    let downloading = read(json!({
        "version": "17.0.0-1", "android_major": 17, "channel": "latest",
        "supported_majors": [16, 17], "target_major": 17,
        "notice": "downloading Android 17.0.0-2: 45% (0.81 of 1.8 GB)"
    }));
    assert!(view::android_channel_text(&downloading).contains("17"));
    assert_eq!(
        view::android_download_percent(downloading.notice.as_deref()),
        Some(45)
    );
    assert!(view::android_update_text(&downloading).starts_with("downloading Android"));
    assert_eq!(view::android_check_channel(&downloading), "latest");

    let pinned = read(json!({
        "version": "16.0.0-1", "android_major": 16, "channel": "16",
        "supported_majors": [16, 17], "target_major": 16
    }));
    assert!(view::android_channel_text(&pinned).contains("16"));
    assert_eq!(view::android_download_percent(None), None);
    assert_eq!(
        view::android_download_percent(Some("Android 17.0.0-2 is installed")),
        None
    );
    assert!(view::android_update_text(&pinned).contains("No Android update"));
    assert_eq!(view::android_check_channel(&pinned), "16");
}

#[test]
fn fr14_a_downloaded_image_is_applied_without_the_user_restarting() {
    let read = |value: serde_json::Value| dsa_settings_linux::model::AndroidVersion::read(&value);
    let waiting = read(json!({
        "version": "17.0.0-1", "android_major": 17, "channel": "latest",
        "supported_majors": [16, 17], "target_major": 17,
        "notice": "Android 17.0.0-2 is downloaded and will be used the next time Android starts",
        "restart_pending": "17.0.0-2"
    }));
    assert_eq!(waiting.restart_pending.as_deref(), Some("17.0.0-2"));
    let text = view::android_update_text(&waiting);
    assert!(
        text.contains("17.0.0-2") && text.contains("Restart Android"),
        "{text}"
    );
    let idle = read(json!({
        "version": "17.0.0-2", "android_major": 17, "channel": "latest",
        "supported_majors": [16, 17], "target_major": 17, "restart_pending": null
    }));
    assert_eq!(idle.restart_pending, None);

    // The setting is on unless the service says it is off.
    let settings = |value: serde_json::Value| dsa_settings_linux::model::Settings::read(&value);
    assert!(settings(json!({"render_gpu": "display"})).image_auto_restart);
    assert!(!settings(json!({"image_auto_restart": false})).image_auto_restart);
}

#[test]
fn fr14_check_now_chooses_the_version_already_followed_as_an_administrator() {
    let fake = Fake::new(json!({}));
    let elevated = Fake::new(json!({
        "version": "17.0.0-1", "android_major": 17, "channel": "latest",
        "supported_majors": [16, 17], "target_major": 17
    }));
    let c = client(&fake, &elevated);
    c.use_android("latest", false).unwrap();
    assert_eq!(
        elevated.calls(),
        vec![line(&["android", "use", "latest", "--json"])]
    );
}

#[test]
fn fr17_the_identity_page_opens_with_this_computer_read_only_and_copies_its_specifications() {
    // The computer reports its maker and model as Android's defaults; the model chosen for
    // Android is not the computer's.
    let identity = dsa_settings_linux::model::Identity::read(&json!({
        "chosen": { "model": "My tablet" },
        "effective": {
            "manufacturer": "LENOVO", "model": "My tablet", "model_name": "ThinkPad X1",
            "device_name": "DESK-42", "soc_manufacturer": "Intel", "soc_model": ""
        },
        "defaults": { "manufacturer": "LENOVO", "model": "21KC" },
        "sources": {
            "manufacturer": "computer", "model": "computer", "model_name": "computer",
            "device_name": "computer", "soc_manufacturer": "computer", "soc_model": "dsa"
        }
    }));
    assert_eq!(identity.of_computer("device_name"), "DESK-42");
    assert_eq!(identity.of_computer("model"), "21KC");
    assert_eq!(identity.computer_make_model(), "LENOVO 21KC");
    assert_eq!(identity.of_computer("soc_model"), "");
    let text = identity.about_text(|key| key.to_string(), "?");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), dsa_settings_linux::model::ABOUT_KEYS.len());
    assert_eq!(lines[0], "device_name	DESK-42");
    assert_eq!(lines[5], "soc_model	?");
    // A default that is DSA's own is not this computer's.
    let dsa = dsa_settings_linux::model::Identity::read(&json!({
        "chosen": {}, "effective": { "manufacturer": "thisisthepy" },
        "defaults": { "manufacturer": "thisisthepy" }, "sources": { "manufacturer": "dsa" }
    }));
    assert_eq!(dsa.of_computer("manufacturer"), "");
}

#[test]
fn fr17_the_command_line_group_offers_one_copy_button_per_command() {
    // The group once showed a single command, and the other settings apps showed
    // "dsa status | dsa settings | dsa memory" on one line, which reads as a shell pipeline and
    // could not be copied. Each command is its own row with a button that copies only it.
    assert_eq!(
        view::COMMAND_LINE_COMMANDS,
        ["dsa status", "dsa settings", "dsa memory"]
    );
    for command in view::COMMAND_LINE_COMMANDS {
        assert!(!command.contains(" | "), "{command:?} reads as a pipeline");
    }
    let src = crate_dir().join("src/ui");
    let system = std::fs::read_to_string(src.join("system.rs")).unwrap();
    assert!(system.contains("for cli_command in view::COMMAND_LINE_COMMANDS {"));
    assert!(system.contains("command_row(cli_command)"));
    assert!(system.contains("adb_row.add_suffix(&copy_button(&adb_command));"));
    let common = std::fs::read_to_string(src.join("common.rs")).unwrap();
    assert!(common.contains("row.add_suffix(&copy_button(&label));"));
    assert!(common.contains(".tooltip_text(tr(\"Copy\"))"));
    assert!(common.contains("button.set_icon_name(\"object-select-symbolic\");"));
}

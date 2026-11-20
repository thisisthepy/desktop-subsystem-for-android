//! The agents DSA.app registers through `SMAppService`: their bundled property lists, the move
//! off the legacy agents, and when the legacy agents are still installed.

use std::path::Path;

use dsa_host_macos::app_service::{
    agent_log_path, bundled_plist, choose_path, legacy_migration, registration_succeeded,
    runs_from_app, InstallPath, LegacyReason, MigrationStep, Situation, Status, AGENTS, DEVICES,
    HOSTD, LOG_ENV, WINDOW_MANAGER,
};
use dsa_host_macos::launchd::{APP_BUNDLE_ID, DEVICES_LABEL, LABEL, WM_LABEL};

fn packaging() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/launch-agents")
}

#[test]
fn fr2_the_bundled_agents_keep_their_labels_and_run_programs_inside_dsa_app() {
    assert_eq!(HOSTD.label, LABEL);
    assert_eq!(WINDOW_MANAGER.label, WM_LABEL);
    assert_eq!(DEVICES.label, DEVICES_LABEL);
    let hostd = bundled_plist(&HOSTD, "1.2.3");
    assert!(hostd.contains("<key>BundleProgram</key>\n\t<string>Contents/MacOS/dsa-hostd</string>"));
    assert!(hostd.contains("<string>dsa-hostd</string>\n\t\t<string>serve</string>\n\t</array>"));
    let wm = bundled_plist(&WINDOW_MANAGER, "1.2.3");
    assert!(wm.contains(
        "<string>Contents/Helpers/DSA Window Manager.app/Contents/MacOS/dsa-wm</string>"
    ));
    let devices = bundled_plist(&DEVICES, "1.2.3");
    assert!(devices
        .contains("<string>Contents/Helpers/DSA Devices.app/Contents/MacOS/dsa-devices</string>"));
    for (agent, text) in [(HOSTD, &hostd), (WINDOW_MANAGER, &wm), (DEVICES, &devices)] {
        assert!(text.contains(&format!(
            "<key>Label</key>\n\t<string>{}</string>",
            agent.label
        )));
        assert!(text.contains(&format!("<string>{APP_BUNDLE_ID}</string>")));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(text.contains("<!-- DSA 1.2.3 -->"));
        // Nothing below a home: the log is opened by the program, named by file only.
        assert!(!text.contains("/Users/") && !text.contains('~'));
        assert!(!text.contains("StandardOutPath"));
        assert!(text.contains(&format!(
            "<key>{LOG_ENV}</key>\n\t\t<string>{}</string>",
            agent.log
        )));
    }
    // Only the window manager has a Mach service, named by its label.
    assert!(wm.contains(&format!(
        "<key>MachServices</key>\n\t<dict>\n\t\t<key>{WM_LABEL}</key>\n\t\t<true/>"
    )));
    assert!(!hostd.contains("MachServices") && !devices.contains("MachServices"));
}

#[test]
fn fr2_the_packaged_agent_property_lists_match_the_bundled_agents() {
    for agent in AGENTS {
        let path = packaging().join(agent.plist_name());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            bundled_plist(&agent, "@VERSION@"),
            "{}",
            path.display()
        );
    }
}

#[test]
fn fr2_an_agent_log_is_a_file_in_the_users_logs_folder() {
    let home = Path::new("/Users/ann");
    assert_eq!(
        agent_log_path(home, "dsa-hostd.log").unwrap(),
        Path::new("/Users/ann/Library/Application Support/DSA/logs/dsa-hostd.log")
    );
    assert_eq!(agent_log_path(home, "../x.log"), None);
    assert_eq!(agent_log_path(home, "/tmp/x.log"), None);
    assert_eq!(agent_log_path(home, ""), None);
}

#[test]
fn fr2_moving_off_the_legacy_agents_boots_each_out_before_removing_it() {
    let steps = legacy_migration(Path::new("/Users/ann"), 501);
    let agents = Path::new("/Users/ann/Library/LaunchAgents");
    let mut expected = Vec::new();
    for label in [LABEL, WM_LABEL, DEVICES_LABEL] {
        expected.push(MigrationStep::Launchctl(vec![
            "bootout".into(),
            format!("gui/501/{label}"),
        ]));
        expected.push(MigrationStep::Remove(agents.join(format!("{label}.plist"))));
    }
    assert_eq!(steps, expected);
}

#[test]
fn fr2_the_legacy_agents_are_the_fallback_only_when_the_app_cannot_register() {
    let all = Situation {
        service_management: true,
        bundled_plists: true,
        config_argument: false,
    };
    assert_eq!(choose_path(all), InstallPath::AppService);
    assert_eq!(
        choose_path(Situation {
            service_management: false,
            ..all
        }),
        InstallPath::Legacy(LegacyReason::NoServiceManagement)
    );
    assert_eq!(
        choose_path(Situation {
            bundled_plists: false,
            ..all
        }),
        InstallPath::Legacy(LegacyReason::NotInApp)
    );
    assert_eq!(
        choose_path(Situation {
            config_argument: true,
            ..all
        }),
        InstallPath::Legacy(LegacyReason::ConfigArgument)
    );
    // Waiting for the user's approval is registered: no second, legacy copy.
    assert!(registration_succeeded(&[
        Status::Enabled,
        Status::RequiresApproval,
        Status::Enabled
    ]));
    assert!(!registration_succeeded(&[
        Status::Enabled,
        Status::NotFound
    ]));
    assert!(!registration_succeeded(&[]));
    assert_eq!(Status::from_raw(1), Status::Enabled);
    assert_eq!(Status::from_raw(2), Status::RequiresApproval);
    assert_eq!(Status::from_raw(3), Status::NotFound);
    assert_eq!(Status::from_raw(0), Status::NotRegistered);
}

#[test]
fn fr2_only_a_program_in_dsa_apps_macos_folder_with_the_plists_registers() {
    let root = std::env::temp_dir().join(format!("dsa-app-service-{}", std::process::id()));
    let contents = root.join("DSA.app/Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    let hostd = contents.join("MacOS/dsa-hostd");
    assert!(!runs_from_app(&hostd));
    std::fs::create_dir_all(contents.join("Library/LaunchAgents")).unwrap();
    for agent in AGENTS {
        std::fs::write(
            contents
                .join("Library/LaunchAgents")
                .join(agent.plist_name()),
            "",
        )
        .unwrap();
    }
    assert!(runs_from_app(&hostd));
    assert!(!runs_from_app(&root.join("target/release/dsa-hostd")));
    std::fs::remove_dir_all(&root).unwrap();
}

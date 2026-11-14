//! Running a downloaded setup file to update DSA.

use std::path::Path;

use dsa_host_core::update::program_service::{after_setup, AfterSetup, SetupOutcome};
use dsa_host_windows::setup::{
    parse_process_key, process_key, quote_argument, restart_command_line, setup_command_line,
    setup_outcome,
};

#[test]
fn fr15_setup_that_installed_and_asks_for_a_restart_counts_as_installed() {
    assert_eq!(setup_outcome(0), SetupOutcome::Installed);
    // ERROR_SUCCESS_REBOOT_REQUIRED (0xbc2), as setup ended when a file was in use.
    assert_eq!(setup_outcome(0xbc2), SetupOutcome::InstalledRestartPending);
    assert_eq!(setup_outcome(1641), SetupOutcome::InstalledRestartPending);
    assert_eq!(setup_outcome(3011), SetupOutcome::InstalledRestartPending);
    for failed in [1, 1602, 1603, 1618, 0x8007_0005] {
        assert_eq!(setup_outcome(failed), SetupOutcome::Failed, "{failed}");
    }
    // 3010 from a setup that put the new files in place restarts the old service into them,
    // where it was reported as a failure before.
    assert_eq!(
        after_setup(setup_outcome(3010), 3010, "0.1.10", Some("0.1.10.0")),
        AfterSetup::RestartIntoInstalled
    );
}

#[test]
fn fr15_a_setup_process_is_named_by_its_id_and_creation_time() {
    let key = process_key(18652, 134_070_000_000_000_000);
    assert_eq!(
        parse_process_key(&key),
        Some((18652, 134_070_000_000_000_000))
    );
    assert_eq!(parse_process_key("18652"), None);
    assert_eq!(parse_process_key("x:1"), None);
}

#[test]
fn fr15_the_service_restarts_through_the_installed_program() {
    assert_eq!(
        restart_command_line(Path::new(r"C:\Program Files\DSA\dsa-hostd.exe")),
        r#""C:\Program Files\DSA\dsa-hostd.exe" restart-service"#
    );
}

#[test]
fn fr15_setup_runs_quietly_without_restarting_and_logs_where_the_service_says() {
    let line = setup_command_line(
        Path::new(r"C:\Data Folder\updates\0.1.1\setup-0.1.1.exe"),
        Path::new(r"C:\Data Folder\logs\setup-0.1.1.log"),
    );
    assert_eq!(
        line,
        r#""C:\Data Folder\updates\0.1.1\setup-0.1.1.exe" /quiet /norestart /log "C:\Data Folder\logs\setup-0.1.1.log""#
    );
}

#[test]
fn fr15_arguments_are_quoted_the_way_windows_programs_read_them() {
    assert_eq!(quote_argument(r"C:\logs\setup.log"), r"C:\logs\setup.log");
    assert_eq!(quote_argument(""), r#""""#);
    assert_eq!(quote_argument(r"C:\a b\"), r#""C:\a b\\""#);
    assert_eq!(quote_argument(r#"say "hi""#), r#""say \"hi\"""#);
    assert_eq!(quote_argument(r#"a\"b c"#), r#""a\\\"b c""#);
}

#[cfg(windows)]
mod on_windows {
    use std::path::PathBuf;

    use dsa_host_core::update::program_service::ProgramInstaller;
    use dsa_host_windows::setup::{spawn_detached, SetupInstaller};

    #[test]
    fn fr15_a_detached_process_runs_and_its_exit_code_is_read() {
        let cmd =
            std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        let process = spawn_detached(
            PathBuf::from(&cmd).as_path(),
            &format!("\"{cmd}\" /c exit 7"),
        )
        .unwrap();
        assert_eq!(process.wait().unwrap(), 7);
        assert!(
            dsa_host_windows::setup::reattach("4294967295:1").is_none(),
            "no such process"
        );
        let missing = spawn_detached(
            PathBuf::from(r"C:\no such folder\setup.exe").as_path(),
            r#""C:\no such folder\setup.exe""#,
        );
        match missing {
            Err(why) => assert!(why.contains("no such folder"), "{why}"),
            Ok(_) => panic!("a setup that does not exist started"),
        }
    }

    #[test]
    fn fr15_setup_in_a_folder_with_spaces_is_started_and_awaited() {
        // dsa-hostd stands in for setup: it rejects setup's options with exit code 1, which
        // shows that it started, received them and ended.
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("setup test {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("0.1.1")).unwrap();
        let setup = dir.join("0.1.1").join("setup 0.1.1.exe");
        std::fs::copy(env!("CARGO_BIN_EXE_dsa-hostd"), &setup).unwrap();
        let running = SetupInstaller::default()
            .install(&setup, &dir.join("setup-0.1.1.log"))
            .unwrap();
        assert_eq!(running.wait().unwrap(), 1);
    }

    #[test]
    fn fr15_a_running_setup_is_found_again_and_an_ended_one_is_not() {
        use dsa_host_core::update::program_service::RunningSetup;
        let cmd =
            std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        // A process that runs for a few seconds, standing in for setup.
        let process = spawn_detached(
            PathBuf::from(&cmd).as_path(),
            &format!("\"{cmd}\" /c ping -n 4 127.0.0.1 >nul"),
        )
        .unwrap();
        let key = RunningSetup::id(&process).expect("a started process has an id");
        let found = SetupInstaller::default()
            .reattach(&key)
            .expect("the setup still runs");
        assert_eq!(found.wait().unwrap(), 0);
        assert!(
            SetupInstaller::default().reattach(&key).is_none(),
            "an ended setup is not found again"
        );
        drop(process);
    }

    #[test]
    fn fr15_the_installed_program_version_is_read_from_its_file() {
        let version = dsa_host_windows::setup::file_version(std::path::Path::new(env!(
            "CARGO_BIN_EXE_dsa-hostd"
        )))
        .expect("dsa-hostd carries version information");
        // The release's own text, so a development build's -dev.<n> is kept.
        assert_eq!(version, dsa_host_windows::VERSION);
    }

    #[test]
    fn fr15_a_development_build_on_disk_is_not_taken_for_a_newer_one() {
        use dsa_host_core::update::program_service::restart_into_files;
        // The files' text version is compared, never the fixed numbers (0.2.4.0 here), which
        // would read as newer than the service itself and restart it again and again.
        assert!(!restart_into_files(
            "0.2.4-dev.7",
            Some("0.2.4-dev.7"),
            false
        ));
        assert!(restart_into_files(
            "0.2.4-dev.7",
            Some("0.2.4-dev.8"),
            false
        ));
    }
}

//! Running a downloaded setup file to update DSA.

use std::path::Path;

use dsa_host_windows::setup::{quote_argument, setup_command_line};

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
        let running = SetupInstaller
            .install(&setup, &dir.join("setup-0.1.1.log"))
            .unwrap();
        assert_eq!(running.wait().unwrap(), 1);
    }
}

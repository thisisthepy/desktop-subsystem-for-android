//! The host configuration file.

use std::path::PathBuf;
use std::time::Duration;

use dsa_host_windows::host_config::HostConfig;

const MINIMAL: &str = r#"{
  "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
  "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx",
  "data_disk": "C:\\ProgramData\\DSA\\data.vhdx"
}"#;

#[test]
fn fr2_a_minimal_configuration_gets_working_defaults() {
    let config = HostConfig::parse(MINIMAL).unwrap();
    assert_eq!(config.vm_name, "dsa-android");
    assert_eq!(config.control_port, 5000);
    let session = config.session_config();
    assert_eq!(session.connect_timeout, Duration::from_secs(120));
    assert_eq!(session.stop_timeout, Duration::from_secs(30));
    let vm = config.vm_config(Some("S-1-5-21-1-2-3-1001"));
    assert_eq!(vm.kernel, PathBuf::from(r"C:\Program Files\DSA\vm\bzImage"));
    assert_eq!(vm.memory_mb, 4096);
    assert_eq!(vm.page_reporting_order, 9);
    assert_eq!(vm.socket_sids, ["S-1-5-21-1-2-3-1001"]);
    vm.to_hcs_document().expect("the defaults make a valid VM");
}

#[test]
fn fr2_a_user_named_in_the_file_wins_over_the_running_user() {
    let json = MINIMAL.replace("\n}", ",\n  \"user_sid\": \"S-1-5-21-9-9-9-1002\"\n}");
    let config = HostConfig::parse(&json).unwrap();
    assert_eq!(
        config.vm_config(Some("S-1-5-21-1-2-3-1001")).socket_sids,
        ["S-1-5-21-9-9-9-1002"]
    );
}

#[test]
fn fr2_a_misspelt_setting_is_reported_not_ignored() {
    let json = MINIMAL.replace("\n}", ",\n  \"memroy_mb\": 2048\n}");
    let error = HostConfig::parse(&json).unwrap_err().to_string();
    assert!(error.contains("memroy_mb"), "{error}");
}

#[test]
fn fr2_a_missing_required_path_is_reported() {
    let error = HostConfig::parse(r#"{"kernel": "C:\\k"}"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("system_image"), "{error}");
}

#[test]
fn fr2_the_example_configuration_is_valid() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dsa-hostd.example.json");
    let config = HostConfig::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    config
        .vm_config(Some("S-1-5-21-1-2-3-1001"))
        .to_hcs_document()
        .expect("the example describes a valid VM");
}

const SERVICE: &str = r#"{
  "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
  "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx",
  "data_dir": "C:\\ProgramData\\DSA\\users"
}"#;

#[test]
fn pr5_each_user_gets_their_own_vm_data_disk_and_socket_access() {
    let config = HostConfig::parse(SERVICE).unwrap();
    let alice = config.user_vm("S-1-5-21-1-2-3-1001").unwrap();
    let bob = config.user_vm("S-1-5-21-1-2-3-1002").unwrap();
    assert_eq!(alice.vm_name, "dsa-android-S-1-5-21-1-2-3-1001");
    assert_eq!(
        alice.config.data_disk,
        PathBuf::from(r"C:\ProgramData\DSA\users\S-1-5-21-1-2-3-1001\data.vhdx")
    );
    assert_eq!(alice.config.socket_sids, ["S-1-5-21-1-2-3-1001"]);
    assert_ne!(alice.vm_name, bob.vm_name);
    assert_ne!(alice.config.data_disk, bob.config.data_disk);
    assert_eq!(bob.config.socket_sids, ["S-1-5-21-1-2-3-1002"]);
    assert_eq!(alice.data_disk_size_gb, 64);
    alice.config.to_hcs_document().expect("a valid VM");
}

#[test]
fn pr5_a_caller_that_is_not_a_sid_gets_no_vm() {
    let config = HostConfig::parse(SERVICE).unwrap();
    for bad in ["", "..", r"S-1-5\..\x", "uid:1000"] {
        assert!(config.user_vm(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn pr5_serving_users_needs_a_data_folder() {
    let config = HostConfig::parse(MINIMAL).unwrap();
    let error = config
        .user_vm("S-1-5-21-1-2-3-1001")
        .unwrap_err()
        .to_string();
    assert!(error.contains("data_dir"), "{error}");
}

#[test]
fn fr2_a_configuration_without_any_data_disk_is_reported() {
    let json = r#"{"kernel": "C:\\k", "system_image": "C:\\s.vhdx"}"#;
    let error = HostConfig::parse(json).unwrap_err().to_string();
    assert!(
        error.contains("data_disk") && error.contains("data_dir"),
        "{error}"
    );
}

#[test]
fn pr5_the_pipe_defaults_to_dsa_host() {
    let config = HostConfig::parse(SERVICE).unwrap();
    assert_eq!(config.pipe_name, "dsa-host");
    assert_eq!(config.pipe_sddl, None);
}

const UPDATING: &str = r#"{
  "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
  "images_dir": "C:\\ProgramData\\DSA\\images",
  "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
  "data_dir": "C:\\ProgramData\\DSA\\users",
  "update": {
    "list_url": "https://example.invalid/dsa/images.json",
    "trusted_keys": ["17cb79fb2b4120f2b1ec65e4198d6e08b28e813feb01e4a400839b85e18080ce"]
  }
}"#;

#[test]
fn fr14_an_updating_configuration_gets_working_defaults() {
    let config = HostConfig::parse(UPDATING).unwrap();
    let update = config.update.as_ref().unwrap();
    assert_eq!(update.check_interval_hours, 24);
    assert_eq!(update.boot_timeout_secs, 300);
    assert_eq!(update.keys().unwrap().len(), 1);
    assert_eq!(config.first_image.as_ref().unwrap().version, "16.0.1");
}

#[test]
fn fr14_the_vm_boots_the_image_folder_it_is_given() {
    let config = HostConfig::parse(UPDATING).unwrap();
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (kernel, system) = config.image_files(Some(&dir));
    assert_eq!(system, dir.join("system.vhdx"));
    assert_eq!(
        kernel,
        PathBuf::from(r"C:\Program Files\DSA\vm\bzImage"),
        "no kernel in the image"
    );
    std::fs::write(dir.join("bzImage"), b"kernel").unwrap();
    assert_eq!(
        config.image_files(Some(&dir)).0,
        dir.join("bzImage"),
        "the image's own kernel"
    );
}

#[test]
fn fr14_an_untrusted_key_or_a_missing_first_image_is_reported() {
    let bad_key = UPDATING.replace("17cb79fb", "zzcb79fb");
    let error = HostConfig::parse(&bad_key).unwrap_err().to_string();
    assert!(error.contains("key"), "{error}");
    let no_first = UPDATING.replace(
        r#"  "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
"#,
        "",
    );
    let error = HostConfig::parse(&no_first).unwrap_err().to_string();
    assert!(error.contains("first_image"), "{error}");
    let neither = r#"{"kernel": "C:\\k", "data_dir": "C:\\d"}"#;
    let error = HostConfig::parse(neither).unwrap_err().to_string();
    assert!(
        error.contains("system_image") && error.contains("images_dir"),
        "{error}"
    );
}

//! The host configuration file.

use std::path::PathBuf;
use std::time::Duration;

use dsa_host_windows::host_config::{
    expand_locations, page_reporting_order_for_build, profile_disk, HostConfig,
};

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
    let vm = config.vm_config(Some("S-1-5-21-1-2-3-1001")).unwrap();
    assert_eq!(vm.kernel, PathBuf::from(r"C:\Program Files\DSA\vm\bzImage"));
    assert_eq!(vm.memory_mb, 3072);
    assert!([5, 9].contains(&vm.page_reporting_order));
    assert_eq!(vm.socket_sids, ["S-1-5-21-1-2-3-1001"]);
    vm.to_hcs_document().expect("the defaults make a valid VM");
}

#[test]
fn fr10_android_dozes_after_half_a_minute_without_a_window_and_never_freezes_by_default() {
    let session = HostConfig::parse(MINIMAL).unwrap().session_config();
    assert_eq!(session.doze_delay, Duration::from_secs(30));
    assert_eq!(session.freeze_after, None);
}

#[test]
fn fr10_freezing_after_a_time_in_doze_is_a_setting() {
    let json = MINIMAL.replace(
        "\n}",
        ",\n  \"doze_delay_secs\": 5,\n  \"freeze_after_minutes\": 60\n}",
    );
    let session = HostConfig::parse(&json).unwrap().session_config();
    assert_eq!(session.doze_delay, Duration::from_secs(5));
    assert_eq!(session.freeze_after, Some(Duration::from_secs(3600)));
}

#[test]
fn nfr2_free_pages_are_reported_in_the_smallest_pieces_the_host_takes() {
    assert_eq!(page_reporting_order_for_build(None), 9);
    assert_eq!(page_reporting_order_for_build(Some((19045, 3392))), 9);
    assert_eq!(page_reporting_order_for_build(Some((19045, 3393))), 5);
    assert_eq!(page_reporting_order_for_build(Some((22000, 2359))), 9);
    assert_eq!(page_reporting_order_for_build(Some((22000, 2360))), 5);
    assert_eq!(page_reporting_order_for_build(Some((26300, 0))), 5);
    let pinned = MINIMAL.replace("\n}", ",\n  \"page_reporting_order\": 9\n}");
    let config = HostConfig::parse(&pinned).unwrap();
    let vm = config.vm_config(Some("S-1-5-21-1-2-3-1001")).unwrap();
    assert_eq!(vm.page_reporting_order, 9);
}

#[test]
fn fr2_a_user_named_in_the_file_wins_over_the_running_user() {
    let json = MINIMAL.replace("\n}", ",\n  \"user_sid\": \"S-1-5-21-9-9-9-1002\"\n}");
    let config = HostConfig::parse(&json).unwrap();
    assert_eq!(
        config
            .vm_config(Some("S-1-5-21-1-2-3-1001"))
            .unwrap()
            .socket_sids,
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
        .unwrap()
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
    assert!(!alice.disk_in_profile);
    alice.config.to_hcs_document().expect("a valid VM");
}

#[test]
fn pr5_a_caller_that_is_not_a_sid_gets_no_vm() {
    let config = HostConfig::parse(SERVICE).unwrap();
    for bad in ["", "..", r"S-1-5\..\x", "uid:1000"] {
        assert!(config.user_vm(bad).is_err(), "{bad:?} must be refused");
    }
}

/// Profiles as Windows would report them, for the tests.
fn profiles(sid: &str) -> Result<PathBuf, String> {
    match sid {
        "S-1-5-21-1-2-3-1001" => Ok(PathBuf::from(r"C:\Users\alice\AppData\Local")),
        "S-1-5-21-1-2-3-1002" => Ok(PathBuf::from(r"D:\Profiles\bob\Local")),
        _ => Err(format!("Windows has no profile folder for {sid}")),
    }
}

const PROFILES: &str = r#"{
  "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
  "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx"
}"#;

#[test]
fn pr5_each_users_disk_is_in_their_own_profile_without_a_data_folder() {
    let config = HostConfig::parse(PROFILES).unwrap();
    let alice = config
        .user_vm_with("S-1-5-21-1-2-3-1001", &profiles)
        .unwrap();
    assert_eq!(
        alice.config.data_disk,
        PathBuf::from(r"C:\Users\alice\AppData\Local\DSA\android.vhdx")
    );
    assert!(alice.disk_in_profile);
    let bob = config
        .user_vm_with("S-1-5-21-1-2-3-1002", &profiles)
        .unwrap();
    assert_eq!(
        bob.config.data_disk,
        PathBuf::from(r"D:\Profiles\bob\Local\DSA\android.vhdx")
    );
    // A user Windows has no profile for gets no VM, and is told why.
    let error = config
        .user_vm_with("S-1-5-21-1-2-3-1003", &profiles)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no profile folder"), "{error}");
}

#[test]
fn fr2_one_vm_by_hand_keeps_its_disk_in_the_users_profile_without_a_data_disk() {
    let config = HostConfig::parse(PROFILES).unwrap();
    let vm = config
        .vm_config_with(Some("S-1-5-21-1-2-3-1001"), &profiles)
        .unwrap();
    assert_eq!(
        vm.data_disk,
        PathBuf::from(r"C:\Users\alice\AppData\Local\DSA\android.vhdx")
    );
    let error = config
        .vm_config_with(None, &profiles)
        .unwrap_err()
        .to_string();
    assert!(error.contains("user_sid"), "{error}");
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
    let files = config.image_files(Some(&dir));
    assert_eq!(files.system_image, Some(dir.join("system.vhdx")));
    assert_eq!(files.disk_template, None);
    assert_eq!(
        files.kernel,
        PathBuf::from(r"C:\Program Files\DSA\vm\bzImage"),
        "no kernel in the image"
    );
    std::fs::write(dir.join("bzImage"), b"kernel").unwrap();
    assert_eq!(
        config.image_files(Some(&dir)).kernel,
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

#[test]
fn fr14_an_image_made_from_aosp_boots_from_one_disk_made_from_its_template() {
    let config = HostConfig::parse(UPDATING).unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aosp-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("image.json"),
        r#"{"version": "17.0.0-16373615", "kernel_cmdline": "androidboot.boot_part_uuid=6b1ad5ad androidboot.fstab_suffix=dsa"}"#,
    )
    .unwrap();
    let files = config.image_files(Some(&dir));
    assert_eq!(files.system_image, None, "no separate system disk");
    assert_eq!(files.disk_template, Some(dir.join("disk.vhdx")));
    assert_eq!(files.initrd, Some(dir.join("initrd.img")));
    assert_eq!(
        files.kernel_cmdline,
        "androidboot.boot_part_uuid=6b1ad5ad androidboot.fstab_suffix=dsa"
    );

    let user = config.user_vm("S-1-5-21-1-2-3-1001").unwrap();
    let mut vm = user.config.clone();
    vm.system_image = files.system_image;
    vm.initrd = files.initrd;
    vm.kernel_cmdline = files.kernel_cmdline;
    let document = vm.to_hcs_document().unwrap();
    let attachments = &document["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(
        attachments.as_object().unwrap().len(),
        1,
        "only the user's disk"
    );
    assert_eq!(attachments["0"]["ReadOnly"], false);
    let kernel = &document["VirtualMachine"]["Chipset"]["LinuxKernelDirect"];
    assert!(kernel["KernelCmdLine"]
        .as_str()
        .unwrap()
        .contains("androidboot.fstab_suffix=dsa"));
    assert!(kernel["InitRdPath"]
        .as_str()
        .unwrap()
        .ends_with("initrd.img"));
}

#[test]
fn fr1_the_folders_chosen_at_installation_fill_in_the_configuration() {
    let text = r#"{
  "kernel": "{program}\\vm\\bzImage",
  "images_dir": "{data}\\images",
  "first_image": {"version": "none", "android_major": 0, "build": 0},
  "log_file": "{data}\\logs\\dsa-hostd.log"
}"#;
    let expanded = expand_locations(
        text,
        &PathBuf::from(r"D:\Android Data\"),
        &PathBuf::from(r"E:\Apps\DSA"),
    );
    let config = HostConfig::parse(&expanded).unwrap();
    assert_eq!(config.kernel, PathBuf::from(r"E:\Apps\DSA\vm\bzImage"));
    assert_eq!(
        config.images_dir,
        Some(PathBuf::from(r"D:\Android Data\images"))
    );
    assert_eq!(
        config.log_file,
        Some(PathBuf::from(r"D:\Android Data\logs\dsa-hostd.log"))
    );
}

const SELF_UPDATING: &str = r#"{
  "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
  "images_dir": "C:\\ProgramData\\DSA\\images",
  "first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
  "log_file": "C:\\ProgramData\\DSA\\logs\\dsa-hostd.log",
  "update": {
    "list_url": "https://example.invalid/dsa/images.json",
    "program_list_url": "https://example.invalid/dsa/program.json",
    "program_dir": "C:\\ProgramData\\DSA\\updates",
    "trusted_keys": ["17cb79fb2b4120f2b1ec65e4198d6e08b28e813feb01e4a400839b85e18080ce"]
  }
}"#;

#[test]
fn fr15_the_program_list_and_the_folder_for_setup_files_come_from_the_configuration() {
    let config = HostConfig::parse(SELF_UPDATING).unwrap();
    let settings = config.program_settings("0.1.0").unwrap().unwrap();
    assert_eq!(
        settings.list_url.as_deref(),
        Some("https://example.invalid/dsa/program.json")
    );
    assert!(settings.enabled, "on unless a setting turns it off");
    assert_eq!(settings.keys.len(), 1);
    assert_eq!(settings.dir, PathBuf::from(r"C:\ProgramData\DSA\updates"));
    assert_eq!(
        settings.log_dir,
        PathBuf::from(r"C:\ProgramData\DSA\logs"),
        "setup logs go beside the service's"
    );
    assert_eq!(settings.running, "0.1.0");
    assert_eq!(settings.stop_timeout, Duration::from_secs(30 + 15));
}

#[test]
fn fr15_a_setting_turns_updating_dsa_off() {
    let off = SELF_UPDATING.replace(
        r#""program_dir""#,
        r#""program": false,
    "program_dir""#,
    );
    let settings = HostConfig::parse(&off)
        .unwrap()
        .program_settings("0.1.0")
        .unwrap()
        .unwrap();
    assert!(!settings.enabled);
}

#[test]
fn fr15_without_a_program_folder_dsa_does_not_update_itself() {
    // An older configuration names no program list: nothing changes for it.
    let config = HostConfig::parse(UPDATING).unwrap();
    assert!(config.program_settings("0.1.0").unwrap().is_none());
    // A program list needs the folder the setup files are downloaded to.
    let no_dir = SELF_UPDATING.replace(
        r#"    "program_dir": "C:\\ProgramData\\DSA\\updates",
"#,
        "",
    );
    let error = HostConfig::parse(&no_dir).unwrap_err().to_string();
    assert!(error.contains("program_dir"), "{error}");
}

#[test]
fn fr2_each_users_console_is_kept_in_the_service_log_folder() {
    let config = HostConfig::parse(SELF_UPDATING).unwrap();
    let alice = config
        .user_vm_with("S-1-5-21-1-2-3-1001", &profiles)
        .unwrap();
    assert_eq!(
        alice.console_log,
        PathBuf::from(r"C:\ProgramData\DSA\logs\android-S-1-5-21-1-2-3-1001.log")
    );
    assert_eq!(
        alice.config.console_pipe.as_deref(),
        Some(r"\\.\pipe\dsa-console-dsa-android-S-1-5-21-1-2-3-1001")
    );
    assert_eq!(alice.config.image_version.as_deref(), Some("16.0.1"));
    let bob = config
        .user_vm_with("S-1-5-21-1-2-3-1002", &profiles)
        .unwrap();
    assert_ne!(alice.console_log, bob.console_log);
    assert_ne!(alice.config.console_pipe, bob.config.console_pipe);
}

#[test]
fn fr2_without_a_service_log_the_console_is_kept_beside_the_users_disk() {
    let config = HostConfig::parse(PROFILES).unwrap();
    let alice = config
        .user_vm_with("S-1-5-21-1-2-3-1001", &profiles)
        .unwrap();
    assert_eq!(
        alice.console_log,
        PathBuf::from(r"C:\Users\alice\AppData\Local\DSA\android-S-1-5-21-1-2-3-1001.log")
    );
    assert_eq!(alice.config.image_version, None, "no versioned image");
}

#[test]
fn fr1_user_profile_resolution_and_vm_config_leave_wslconfig_untouched() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fr1-wslconfig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let wslconfig = dir.join(".wslconfig");
    let original = b"[wsl2]
kernel=C:\\WSL\\bzImage
memory=8GB
processors=4
";
    std::fs::write(&wslconfig, original).unwrap();

    let local_dir = dir.join("AppData").join("Local");
    let profile_dir = local_dir.clone();
    let lookup_profile = move |sid: &str| -> Result<PathBuf, String> {
        if sid == "S-1-5-21-1-2-3-1001" {
            Ok(profile_dir.clone())
        } else {
            Err("no profile".into())
        }
    };

    let config = HostConfig::parse(PROFILES).unwrap();
    let alice = config
        .user_vm_with("S-1-5-21-1-2-3-1001", &lookup_profile)
        .unwrap();

    assert_eq!(alice.config.data_disk, profile_disk(&local_dir));
    assert!(alice.disk_in_profile);

    let vm = config
        .vm_config_with(Some("S-1-5-21-1-2-3-1001"), &lookup_profile)
        .unwrap();
    assert_eq!(vm.data_disk, profile_disk(&local_dir));

    let current = std::fs::read(&wslconfig).unwrap();
    assert_eq!(
        current, original,
        "the user's .wslconfig must remain byte-identical"
    );
}

#[test]
fn fr14_an_image_with_two_disks_attaches_system_image_and_data_disk() {
    let config = HostConfig::parse(UPDATING).unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aosp-two-disk-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("image.json"),
        r#"{"version": "17.0.0-16373615", "kernel_cmdline": "androidboot.boot_devices=vmbus androidboot.fstab_suffix=dsa", "files": {"system.vhdx": {}, "data.vhdx": {}}}"#,
    )
    .unwrap();
    let files = config.image_files(Some(&dir));
    assert_eq!(files.system_image, Some(dir.join("system.vhdx")));
    assert_eq!(files.disk_template, Some(dir.join("data.vhdx")));
    assert_eq!(files.initrd, Some(dir.join("initrd.img")));
    assert_eq!(
        files.kernel_cmdline,
        "androidboot.boot_devices=vmbus androidboot.fstab_suffix=dsa"
    );

    let user = config.user_vm("S-1-5-21-1-2-3-1001").unwrap();
    let mut vm = user.config.clone();
    vm.system_image = files.system_image;
    vm.initrd = files.initrd;
    vm.kernel_cmdline = files.kernel_cmdline;
    let document = vm.to_hcs_document().unwrap();
    let attachments = &document["VirtualMachine"]["Devices"]["Scsi"]["0"]["Attachments"];
    assert_eq!(
        attachments.as_object().unwrap().len(),
        2,
        "both system and data disks"
    );
    assert_eq!(attachments["0"]["ReadOnly"], true);
    assert_eq!(attachments["1"]["ReadOnly"], false);
    let kernel = &document["VirtualMachine"]["Chipset"]["LinuxKernelDirect"];
    assert!(kernel["KernelCmdLine"]
        .as_str()
        .unwrap()
        .contains("androidboot.boot_devices=vmbus"));
}

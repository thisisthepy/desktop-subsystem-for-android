//! The configuration rules every host shares.

use std::path::Path;

use dsa_host_core::host_config::{
    check_image_sources, expand_locations, sized_memory_mb, UpdateSettings, DEFAULT_MEMORY_MB,
};
use dsa_host_core::update::program::ReleaseChannel;

fn update(program_list_url: Option<&str>) -> UpdateSettings {
    UpdateSettings {
        list_url: "https://example.invalid/images.json".into(),
        trusted_keys: vec![],
        check_interval_hours: 24,
        boot_timeout_secs: 300,
        program_list_url: program_list_url.map(str::to_string),
        prerelease_program_list_url: None,
        channel: None,
        program: true,
        program_dir: None,
    }
}

#[test]
fn fr19_a_configuration_names_an_image_or_an_update_store() {
    assert!(check_image_sources(None, None, None, None).is_err());
    assert!(check_image_sources(Some(Path::new("system.vhdx")), None, None, None).is_ok());
    assert!(check_image_sources(None, Some(Path::new("store")), None, None).is_err());
}

#[test]
fn fr19_updates_need_a_store_and_a_folder_for_setup_files() {
    let system = Some(Path::new("system.vhdx"));
    assert!(check_image_sources(system, None, None, Some(&update(None))).is_err());
    let program = update(Some("https://example.invalid/program.json"));
    assert!(check_image_sources(system, None, None, Some(&program)).is_err());
}

#[test]
fn fr19_automatic_memory_is_the_default_size() {
    assert_eq!(sized_memory_mb(0, Some(65536)), DEFAULT_MEMORY_MB);
    assert_eq!(sized_memory_mb(2048, None), 2048);
}

#[test]
fn nfr2_the_vm_boots_at_two_gib_and_grows_from_there() {
    // The page records the guest kernel keeps for its boot memory stay resident while the VM
    // runs, so the VM boots small and grows instead.
    assert_eq!(DEFAULT_MEMORY_MB, 2048);
    assert_eq!(sized_memory_mb(0, Some(32768)), 2048);
    assert_eq!(sized_memory_mb(0, None), 2048);
}

#[test]
fn fr19_installer_locations_are_filled_in() {
    let text = expand_locations(
        r#"{"a":"{data}\\images","b":"{program}"}"#,
        Path::new(r"D:\Data\"),
        Path::new("/opt/dsa"),
    );
    assert_eq!(text, r#"{"a":"D:\\Data\\images","b":"/opt/dsa"}"#);
}

#[test]
fn fr15_the_installers_channel_is_the_configured_one_or_follows_the_version() {
    let mut update = update(Some("https://example.invalid/program-stable.json"));
    assert_eq!(
        update.installer_channel("0.1.10"),
        ReleaseChannel::Prerelease
    );
    assert_eq!(update.installer_channel("1.0.0"), ReleaseChannel::Stable);
    update.channel = Some(ReleaseChannel::Prerelease);
    assert_eq!(
        update.installer_channel("1.2.0"),
        ReleaseChannel::Prerelease
    );
    let read: UpdateSettings = serde_json::from_str(
        r#"{"list_url": "https://example.invalid/images.json", "trusted_keys": [],
            "program_list_url": "https://example.invalid/program-stable.json",
            "prerelease_program_list_url": "https://example.invalid/program-prerelease.json",
            "channel": "prerelease"}"#,
    )
    .unwrap();
    assert_eq!(read.channel, Some(ReleaseChannel::Prerelease));
    assert!(read.has_program_list());
}

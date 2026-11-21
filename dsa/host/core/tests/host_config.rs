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

#[test]
fn fr1_a_preset_installer_names_one_android_major() {
    use dsa_host_core::host_config::InstallerPreset;
    assert_eq!(
        InstallerPreset::parse(r#"{"android_major": 16}"#).unwrap(),
        InstallerPreset { android_major: 16 }
    );
    assert!(InstallerPreset::parse(r#"{"android_major": 0}"#).is_err());
    assert!(InstallerPreset::parse(r#"{"android_major": 16, "other": 1}"#).is_err());
}

#[test]
fn fr1_a_default_installation_has_no_preset() {
    use dsa_host_core::host_config::InstallerPreset;
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("preset-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(InstallerPreset::read_in(&dir).unwrap(), None);
    std::fs::write(
        dir.join("installer-preset.json"),
        r#"{"android_major": 17}"#,
    )
    .unwrap();
    assert_eq!(
        InstallerPreset::read_in(&dir).unwrap(),
        Some(InstallerPreset { android_major: 17 })
    );
}

mod shared_file {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use dsa_host_core::host_config::{
        service_settings_store, settings_store, updates_config, HostConfigFile, UpdateSettings,
    };
    use dsa_host_core::update::policy::InstalledImage;
    use dsa_host_core::update::program::ReleaseChannel;

    /// A host's file reduced to the keys every host's file has, with the defaults every host's
    /// file gives them.
    struct Shared {
        images_dir: Option<PathBuf>,
        first_image: Option<InstalledImage>,
        update: Option<UpdateSettings>,
        freeze_after_minutes: Option<u64>,
    }

    impl HostConfigFile for Shared {
        fn images_dir(&self) -> Option<&Path> {
            self.images_dir.as_deref()
        }
        fn first_image(&self) -> Option<&InstalledImage> {
            self.first_image.as_ref()
        }
        fn update(&self) -> Option<&UpdateSettings> {
            self.update.as_ref()
        }
        fn connect_timeout_secs(&self) -> u64 {
            120
        }
        fn ready_timeout_secs(&self) -> u64 {
            dsa_host_core::host_config::default_ready_timeout_secs()
        }
        fn stop_timeout_secs(&self) -> u64 {
            30
        }
        fn doze_delay_secs(&self) -> u64 {
            30
        }
        fn freeze_after_minutes(&self) -> Option<u64> {
            self.freeze_after_minutes
        }
    }

    fn shared(update: Option<UpdateSettings>) -> Shared {
        Shared {
            images_dir: Some(PathBuf::from("store")),
            first_image: Some(InstalledImage {
                version: "16.0.0-1".into(),
                android_major: 16,
                build: 1,
            }),
            update,
            freeze_after_minutes: None,
        }
    }

    fn settings(channel: Option<ReleaseChannel>) -> UpdateSettings {
        UpdateSettings {
            list_url: "https://example.invalid/images.json".into(),
            trusted_keys: vec![],
            check_interval_hours: 24,
            boot_timeout_secs: 77,
            program_list_url: None,
            prerelease_program_list_url: None,
            channel,
            program: true,
            program_dir: None,
        }
    }

    #[test]
    fn fr10_the_session_is_timed_from_the_files_keys_and_never_freezes_by_default() {
        let mut config = shared(None);
        let session = config.session_config();
        assert_eq!(session.connect_timeout, Duration::from_secs(120));
        assert_eq!(session.stop_timeout, Duration::from_secs(30));
        assert_eq!(session.doze_delay, Duration::from_secs(30));
        assert_eq!(session.freeze_after, None);
        config.freeze_after_minutes = Some(60);
        assert_eq!(
            config.session_config().freeze_after,
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn fr15_the_update_store_comes_from_the_files_keys() {
        let config = shared(Some(settings(None)));
        let store = updates_config(&config);
        assert_eq!(store.images_dir, Some(Path::new("store")));
        assert_eq!(store.first_image.map(|i| i.build), Some(1));
        assert!(store.bundled_image.is_none());
        assert_eq!(store.boot_timeout_secs, Some(77));
        assert!(config.image_updates_available());
        assert_eq!(updates_config(&shared(None)).boot_timeout_secs, None);
        assert!(!shared(None).image_updates_available());
    }

    #[test]
    fn fr15_a_computer_starts_on_the_installers_channel_in_every_host() {
        assert_eq!(
            shared(None).installer_channel("0.1.10"),
            ReleaseChannel::Prerelease
        );
        assert_eq!(
            shared(Some(settings(None))).installer_channel("1.0.0"),
            ReleaseChannel::Stable
        );
        assert_eq!(
            shared(Some(settings(Some(ReleaseChannel::Prerelease)))).installer_channel("1.0.0"),
            ReleaseChannel::Prerelease
        );
    }

    #[test]
    fn fr15_the_service_records_the_first_channel_once_and_keeps_it() {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("host-config-channel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Reading settings records nothing; the service's store records the channel.
        let plain = settings_store(dir.clone(), None, None);
        assert!(!plain.machine().prerelease_updates);
        assert!(!dir.join("machine.json").exists());
        let first = service_settings_store(dir.clone(), ReleaseChannel::Prerelease, None, "test");
        assert!(first.machine().prerelease_updates);
        assert!(dir.join("machine.json").exists());
        // A later start with another installer channel keeps what is recorded.
        let again = service_settings_store(dir.clone(), ReleaseChannel::Stable, None, "test");
        assert!(again.machine().prerelease_updates);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

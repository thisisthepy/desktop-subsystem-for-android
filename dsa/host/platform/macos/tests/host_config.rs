//! The macOS configuration file, read through the rules every host shares.

use std::time::Duration;

use dsa_host_core::host_config::{updates_config, HostConfigFile};
use dsa_host_macos::host_config::MacHostConfig;

const STORE: &str = r#"{
  "images_dir": "/Library/Application Support/DSA",
  "first_image": { "version": "16.0.0-1", "android_major": 16, "build": 1 },
  "update": { "list_url": "https://example.invalid/images.json", "trusted_keys": [],
              "boot_timeout_secs": 90 }
}"#;

#[test]
fn fr10_the_file_gets_the_shared_defaults() {
    let config = MacHostConfig::parse(STORE).unwrap();
    assert_eq!(config.control_port, 5000);
    assert_eq!(config.log_port, 5001);
    let session = config.session_config();
    assert_eq!(session.connect_timeout, Duration::from_secs(120));
    assert_eq!(session.stop_timeout, Duration::from_secs(30));
    assert_eq!(session.doze_delay, Duration::from_secs(30));
    assert_eq!(session.freeze_after, None);
}

#[test]
fn fr15_the_update_store_is_the_files_own() {
    let config = MacHostConfig::parse(STORE).unwrap();
    let store = updates_config(&config);
    assert_eq!(
        store.images_dir,
        Some(std::path::Path::new("/Library/Application Support/DSA"))
    );
    assert_eq!(store.first_image.map(|i| i.build), Some(1));
    assert!(store.bundled_image.is_none());
    assert_eq!(store.boot_timeout_secs, Some(90));
    assert!(config.image_updates_available());
}

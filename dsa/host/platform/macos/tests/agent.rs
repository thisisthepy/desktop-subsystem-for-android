//! The per-user agent: its configuration and its launchd property list.
#![cfg(unix)]

use std::path::Path;

use dsa_host_macos::host_config::{memory_range_mb, MacHostConfig};
use dsa_host_macos::launchd::{
    domain, install_commands, plist, plist_path, service_target, start_command, wm_plist,
    wm_plist_path, wm_service_target, LABEL, WM_LABEL,
};

#[test]
fn fr2_the_configuration_names_an_image_to_boot() {
    assert!(MacHostConfig::parse("{}").is_err());
    assert!(MacHostConfig::parse(r#"{"images_dir": "/Library/Application Support/DSA"}"#).is_err());
    let config = MacHostConfig::parse(r#"{"image_dir": "/images/16"}"#).unwrap();
    assert_eq!(config.control_port, 5000);
    assert_eq!(config.log_port, 5001);
    assert!(config.network && config.pstore);
    assert!(MacHostConfig::parse(r#"{"image_dir": "/i", "unknown": 1}"#).is_err());
}

#[test]
fn pr5_the_users_files_and_socket_are_in_their_own_folder() {
    let config = MacHostConfig::parse(r#"{"image_dir": "/images/16"}"#).unwrap();
    let user = config.user(Path::new("/Users/ann"));
    let root = Path::new("/Users/ann/Library/Application Support/DSA");
    assert_eq!(user.root, root);
    // Where dsa looks for the local API.
    assert_eq!(config.socket(&user), root.join("host.sock"));
    assert_eq!(user.disk(), root.join("android.img"));
    assert!(user.console_log().starts_with(root.join("logs")));
    assert_eq!(config.settings_dir(&user), root.join("settings"));
}

#[test]
fn nfr2_memory_starts_small_and_grows_to_what_the_mac_can_spare() {
    // No limit: the default start, growing to half the Mac's memory, with no fixed cap.
    assert_eq!(memory_range_mb(0, Some(16384)), (2048, 8192));
    assert_eq!(memory_range_mb(0, Some(65536)), (2048, 32768));
    assert_eq!(memory_range_mb(0, Some(8192)), (2048, 4096));
    // A limit is how far it grows; a low one is also where it starts.
    assert_eq!(memory_range_mb(6144, Some(32768)), (2048, 6144));
    // A limit above half the Mac is honoured, within what the Mac has.
    assert_eq!(memory_range_mb(24576, Some(32768)), (2048, 24576));
    assert_eq!(memory_range_mb(2048, Some(32768)), (2048, 2048));
    // Unknown memory: the start, fixed.
    assert_eq!(memory_range_mb(0, None), (2048, 2048));
    let fixed = MacHostConfig::parse(r#"{"image_dir": "/i", "memory_mb": 4096}"#).unwrap();
    assert_eq!(fixed.memory_mb(0, Some(65536)), (4096, 4096));
}

#[test]
fn fr2_launchd_starts_the_agent_at_login_and_again_after_a_failure() {
    let text = plist(
        Path::new("/Applications/DSA.app/Contents/MacOS/dsa-hostd"),
        Some(Path::new("/Library/Application Support/DSA/dsa-hostd.json")),
        Path::new("/Users/ann/Library/Application Support/DSA/logs/dsa-hostd.log"),
    );
    assert!(text.contains(&format!("<string>{LABEL}</string>")));
    assert!(text.contains("<string>/Applications/DSA.app/Contents/MacOS/dsa-hostd</string>"));
    assert!(text.contains("<string>serve</string>"));
    assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
    assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
    assert!(text.contains("<string>Aqua</string>"));
    let escaped = plist(Path::new("/a&b/dsa-hostd"), None, Path::new("/l"));
    assert!(escaped.contains("/a&amp;b/dsa-hostd"));
    assert_eq!(
        plist_path(Path::new("/Users/ann")),
        Path::new("/Users/ann/Library/LaunchAgents").join(format!("{LABEL}.plist"))
    );
}

#[test]
fn fr2_the_agent_is_loaded_into_the_users_login_session() {
    assert_eq!(domain(501), "gui/501");
    assert_eq!(service_target(501), format!("gui/501/{LABEL}"));
    let commands = install_commands(501, Path::new("/p.plist"));
    assert_eq!(
        commands[0],
        vec!["bootout".to_string(), service_target(501)]
    );
    assert_eq!(
        commands[1],
        vec!["bootstrap".to_string(), "gui/501".into(), "/p.plist".into()]
    );
    assert_eq!(start_command(501)[0], "kickstart");
}

#[test]
fn fr3_the_window_manager_runs_from_its_own_agent_with_its_mach_service() {
    let text = wm_plist(
        Path::new("/Applications/DSA.app/Contents/MacOS/dsa-wm"),
        Path::new("/Users/ann/Library/Application Support/DSA/logs/dsa-wm.log"),
    );
    assert!(text.contains(&format!("<string>{WM_LABEL}</string>")));
    assert!(text.contains("<string>/Applications/DSA.app/Contents/MacOS/dsa-wm</string>"));
    // The Mach service the frame surfaces go through, named as the window hosts look it up.
    let service = format!("<key>MachServices</key>\n\t<dict>\n\t\t<key>{WM_LABEL}</key>");
    assert!(text.contains(&service));
    assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
    assert!(text.contains("<string>Aqua</string>"));
    // The service's own agent has no Mach service.
    let hostd = plist(Path::new("/d/dsa-hostd"), None, Path::new("/l"));
    assert!(!hostd.contains("MachServices"));
    assert_eq!(wm_service_target(501), format!("gui/501/{WM_LABEL}"));
    let path = wm_plist_path(Path::new("/Users/ann"));
    assert!(path.ends_with(format!("{WM_LABEL}.plist")));
}

#[test]
fn fr26_the_device_lender_runs_from_its_own_agent_at_login() {
    use dsa_host_macos::launchd::{
        devices_plist, devices_plist_path, devices_service_target, DEVICES_LABEL,
    };
    let text = devices_plist(
        Path::new("/Applications/DSA.app/Contents/MacOS/dsa-devices"),
        Path::new("/Users/ann/Library/Application Support/DSA/logs/dsa-devices.log"),
    );
    assert!(text.contains(&format!("<string>{DEVICES_LABEL}</string>")));
    assert!(text.contains("<string>/Applications/DSA.app/Contents/MacOS/dsa-devices</string>"));
    assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
    assert!(text.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
    // In the login session, where macOS asks the user for the camera, microphone and location.
    assert!(text.contains("<string>Aqua</string>"));
    assert!(!text.contains("MachServices"));
    assert_ne!(DEVICES_LABEL, LABEL);
    assert_ne!(DEVICES_LABEL, WM_LABEL);
    assert_eq!(
        devices_service_target(501),
        format!("gui/501/{DEVICES_LABEL}")
    );
    assert_eq!(
        devices_plist_path(Path::new("/Users/ann")),
        Path::new("/Users/ann/Library/LaunchAgents").join(format!("{DEVICES_LABEL}.plist"))
    );
}

#[test]
fn fr26_every_device_port_of_the_guest_is_relayed_to_the_users_own_folder() {
    use dsa_host_macos::host_config::{relay_socket, DEVICE_PORTS, GRAPHICS_PORTS, RELAYED_PORTS};
    for port in [
        dsa_host_core::audio::AUDIO_PORT,
        dsa_host_core::video::VIDEO_PORT,
        dsa_host_core::sensors::SENSOR_PORT,
        dsa_host_core::camera::CAMERA_PORT,
    ] {
        assert!(DEVICE_PORTS.contains(&port));
        assert!(RELAYED_PORTS.contains(&port));
    }
    for port in GRAPHICS_PORTS {
        assert!(RELAYED_PORTS.contains(&port));
        assert!(!DEVICE_PORTS.contains(&port));
    }
    let config = MacHostConfig::parse(r#"{"image_dir": "/images/16"}"#).unwrap();
    let ann = config.user(Path::new("/Users/ann"));
    let bob = config.user(Path::new("/Users/bob"));
    let camera = dsa_host_core::camera::CAMERA_PORT;
    assert_eq!(
        relay_socket(&ann, camera),
        Path::new("/Users/ann/Library/Application Support/DSA/vsock/5572.sock")
    );
    assert_ne!(relay_socket(&ann, camera), relay_socket(&bob, camera));
}

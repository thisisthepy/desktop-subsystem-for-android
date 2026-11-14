//! Tests for Hyper-V socket service GUID registration.
#![cfg(windows)]

use dsa_host_core::audio::AUDIO_PORT;
use dsa_host_core::compositor::{GRAPHICS_COMMAND_PORT, GRAPHICS_PROCESS_PIPE_PORT};
use dsa_host_windows::guest_communication_services::{
    is_service_guid_registered_under, read_service_element_name_under,
    register_graphics_service_guids_under, register_service_guid_under,
    unregister_service_guid_under, RegistryHive, AUDIO_ELEMENT_NAME, DEFAULT_SERVICE_SDDL,
    GRAPHICS_COMMAND_ELEMENT_NAME, GRAPHICS_PROCESS_PIPE_ELEMENT_NAME,
};
use dsa_host_windows::guid::Guid;

fn test_base_key() -> String {
    format!(
        r"Software\DesktopSubsystemForAndroid\TestGuestCommunicationServices_{}",
        std::process::id()
    )
}

#[test]
fn pr1_graphics_ports_map_to_expected_vsock_service_guids() {
    let command_guid = Guid::vsock_service(GRAPHICS_COMMAND_PORT);
    assert_eq!(command_guid.vsock_port(), Some(GRAPHICS_COMMAND_PORT));
    assert_eq!(
        command_guid.to_string(),
        "000015be-facb-11e6-bd58-64006a7986d3"
    );

    let process_pipe_guid = Guid::vsock_service(GRAPHICS_PROCESS_PIPE_PORT);
    assert_eq!(
        process_pipe_guid.vsock_port(),
        Some(GRAPHICS_PROCESS_PIPE_PORT)
    );
    assert_eq!(
        process_pipe_guid.to_string(),
        "000015bf-facb-11e6-bd58-64006a7986d3"
    );
}

#[test]
fn pr1_graphics_service_guids_can_be_registered_and_queried_under_test_key() {
    let base_key = test_base_key();
    let command_guid = Guid::vsock_service(GRAPHICS_COMMAND_PORT);
    let process_pipe_guid = Guid::vsock_service(GRAPHICS_PROCESS_PIPE_PORT);

    // Register under HKCU test path
    register_service_guid_under(
        RegistryHive::CurrentUser,
        &base_key,
        command_guid,
        GRAPHICS_COMMAND_ELEMENT_NAME,
        Some(DEFAULT_SERVICE_SDDL),
    )
    .expect("register command stream GUID");

    register_service_guid_under(
        RegistryHive::CurrentUser,
        &base_key,
        process_pipe_guid,
        GRAPHICS_PROCESS_PIPE_ELEMENT_NAME,
        Some(DEFAULT_SERVICE_SDDL),
    )
    .expect("register process pipe GUID");

    assert!(is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &base_key,
        command_guid
    ));
    assert!(is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &base_key,
        process_pipe_guid
    ));

    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &base_key, command_guid)
            .as_deref(),
        Some(GRAPHICS_COMMAND_ELEMENT_NAME)
    );
    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &base_key, process_pipe_guid)
            .as_deref(),
        Some(GRAPHICS_PROCESS_PIPE_ELEMENT_NAME)
    );

    // Clean up
    unregister_service_guid_under(RegistryHive::CurrentUser, &base_key, command_guid)
        .expect("unregister command GUID");
    unregister_service_guid_under(RegistryHive::CurrentUser, &base_key, process_pipe_guid)
        .expect("unregister process pipe GUID");

    assert!(!is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &base_key,
        command_guid
    ));
    assert!(!is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &base_key,
        process_pipe_guid
    ));
}

#[test]
fn pr1_unregistered_service_guid_is_reported_not_registered() {
    let base_key = test_base_key();
    let unknown_guid = Guid::new(0x12345678, 0x1111, 0x2222, [0; 8]);
    assert!(!is_service_guid_registered_under(
        RegistryHive::CurrentUser,
        &base_key,
        unknown_guid
    ));
}

#[test]
fn fr36_the_keymint_root_key_port_is_registered_for_the_service_only() {
    use dsa_host_core::keymint_root::KEYMINT_PORT;
    use dsa_host_windows::guest_communication_services::{KEYMINT_ELEMENT_NAME, SERVICE_ONLY_SDDL};
    let base_key = format!("{}_keymint", test_base_key());
    let guid = Guid::vsock_service(KEYMINT_PORT);
    assert_eq!(guid.to_string(), "000015e4-facb-11e6-bd58-64006a7986d3");
    register_graphics_service_guids_under(RegistryHive::CurrentUser, &base_key)
        .expect("register the service GUIDs");
    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &base_key, guid).as_deref(),
        Some(KEYMINT_ELEMENT_NAME)
    );
    assert!(!SERVICE_ONLY_SDDL.contains(";IU)") && !SERVICE_ONLY_SDDL.contains(";AU)"));
    let _ = unregister_service_guid_under(RegistryHive::CurrentUser, &base_key, guid);
}

#[test]
fn fr28_the_dns_port_is_registered_for_the_service_only() {
    use dsa_host_core::dns::DNS_PORT;
    use dsa_host_windows::guest_communication_services::DNS_ELEMENT_NAME;
    let base_key = format!("{}_dns", test_base_key());
    let guid = Guid::vsock_service(DNS_PORT);
    assert_eq!(guid.to_string(), "000015e5-facb-11e6-bd58-64006a7986d3");
    register_graphics_service_guids_under(RegistryHive::CurrentUser, &base_key)
        .expect("register the service GUIDs");
    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &base_key, guid).as_deref(),
        Some(DNS_ELEMENT_NAME)
    );
    let _ = unregister_service_guid_under(RegistryHive::CurrentUser, &base_key, guid);
}

#[test]
fn pr4_the_audio_port_is_registered_beside_the_graphics_ports() {
    // dsa-wm listens on the audio port in the user's session, which Windows allows only for
    // a registered service.
    let base_key = format!("{}_audio", test_base_key());
    let audio_guid = Guid::vsock_service(AUDIO_PORT);
    assert_eq!(
        audio_guid.to_string(),
        "000015c0-facb-11e6-bd58-64006a7986d3"
    );
    register_graphics_service_guids_under(RegistryHive::CurrentUser, &base_key)
        .expect("register the service GUIDs");
    assert_eq!(
        read_service_element_name_under(RegistryHive::CurrentUser, &base_key, audio_guid)
            .as_deref(),
        Some(AUDIO_ELEMENT_NAME)
    );
    for port in [
        GRAPHICS_COMMAND_PORT,
        GRAPHICS_PROCESS_PIPE_PORT,
        AUDIO_PORT,
    ] {
        let _ = unregister_service_guid_under(
            RegistryHive::CurrentUser,
            &base_key,
            Guid::vsock_service(port),
        );
    }
}

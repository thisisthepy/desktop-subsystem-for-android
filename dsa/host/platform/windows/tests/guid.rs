use dsa_host_windows::guid::Guid;

#[test]
fn pr1_a_vsock_port_maps_to_the_hyper_v_service_id() {
    let service = Guid::vsock_service(5000);
    assert_eq!(service.to_string(), "00001388-facb-11e6-bd58-64006a7986d3");
    assert_eq!(service.vsock_port(), Some(5000));
    assert_eq!(Guid::LOOPBACK.vsock_port(), None);
}

#[test]
fn pr1_well_known_vm_ids_round_trip_through_text() {
    for (guid, text) in [
        (Guid::LOOPBACK, "e0e16197-dd56-4a10-9195-5ee7a155a838"),
        (Guid::CHILDREN, "90db8b89-0d35-4f79-8ce9-49ea0ac8b7cd"),
        (Guid::PARENT, "a42e7cda-d03f-480c-9cc2-a4de20abb878"),
        (Guid::WILDCARD, "00000000-0000-0000-0000-000000000000"),
    ] {
        assert_eq!(guid.to_string(), text);
        assert_eq!(text.parse::<Guid>(), Ok(guid));
    }
}

#[test]
fn pr1_guids_parse_with_braces_and_any_case() {
    let parsed: Guid = "{E0E16197-DD56-4A10-9195-5EE7A155A838}".parse().unwrap();
    assert_eq!(parsed, Guid::LOOPBACK);
}

#[test]
fn pr1_malformed_guids_are_rejected() {
    for text in [
        "",
        "e0e16197",
        "e0e16197-dd56-4a10-9195-5ee7a155a83",
        "g0e16197-dd56-4a10-9195-5ee7a155a838",
        "e0e16197dd564a1091955ee7a155a838",
    ] {
        assert!(text.parse::<Guid>().is_err(), "{text:?} should not parse");
    }
}

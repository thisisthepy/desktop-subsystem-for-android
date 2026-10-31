//! The hand-written binder service agrees with the AIDL files the clients are generated
//! from: the same interface tokens, and the same transaction codes, which `aidl` assigns
//! in declaration order from FIRST_CALL_TRANSACTION.

use std::path::PathBuf;

use dsa_guest_channeld::aidl;

/// The package, the interface name and the method names, in order, of one AIDL file.
fn interface(name: &str) -> (String, String, Vec<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aidl/io/github/thisisthepy/dsa/channel")
        .join(format!("{name}.aidl"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut package = String::new();
    let mut interface = String::new();
    let mut methods = Vec::new();
    for line in text.lines() {
        let line = line.split("//").next().unwrap().trim();
        if let Some(rest) = line.strip_prefix("package ") {
            package = rest.trim_end_matches(';').trim().to_string();
        } else if let Some(rest) = line.strip_prefix("interface ") {
            interface = rest.split_whitespace().next().unwrap().to_string();
        } else if let Some(open) = line.find('(') {
            let head = line[..open].trim();
            methods.push(head.split_whitespace().last().unwrap().to_string());
        }
    }
    (package, interface, methods)
}

#[test]
fn fr3_the_channel_service_matches_its_aidl_definition() {
    let (package, name, methods) = interface("IChannel");
    assert_eq!(format!("{package}.{name}"), aidl::CHANNEL_DESCRIPTOR);
    assert_eq!(format!("{package}.{name}/default"), aidl::INSTANCE);
    assert_eq!(methods, ["send", "subscribe"]);
    let code = |method: &str| {
        aidl::FIRST_CALL_TRANSACTION + methods.iter().position(|m| m == method).unwrap() as u32
    };
    assert_eq!(aidl::SEND, code("send"));
    assert_eq!(aidl::SUBSCRIBE, code("subscribe"));
    // What aidl gives every versioned interface: FIRST_CALL_TRANSACTION + 16777214 and
    // + 16777213, the top of the range a user transaction may use.
    assert_eq!(aidl::GET_INTERFACE_VERSION, 0x00ff_ffff);
    assert_eq!(aidl::GET_INTERFACE_HASH, 0x00ff_fffe);
}

#[test]
fn fr3_the_listener_matches_its_aidl_definition() {
    let (package, name, methods) = interface("IChannelListener");
    assert_eq!(format!("{package}.{name}"), aidl::LISTENER_DESCRIPTOR);
    assert_eq!(methods, ["onFrame"]);
    assert_eq!(aidl::ON_FRAME, aidl::FIRST_CALL_TRANSACTION);
}

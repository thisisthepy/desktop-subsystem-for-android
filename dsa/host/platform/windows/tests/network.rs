//! The Android VM's HCN network documents and what the host reads back from HCN.

use dsa_host_windows::guid::Guid;
use dsa_host_windows::network::{
    add_adapter_request, advanced_adapter, advanced_problem, all_query, choose_nat_subnet,
    endpoint_document, endpoint_facts, endpoint_name, guid_from, host_has_ipv6, host_side_adapter,
    ids_from_enumeration, mac_bytes, moved_leftover, name_query, nat_ipv6_subnet,
    nat_network_document, nat_path_problem, network_facts, network_name, object_name,
    remove_adapter_request, reusable, subnets_in_use, transparent_network_document, EndpointFacts,
    HostAdapter, Ipv4Subnet, NatPath, NetworkMode, NetworkStatusFacts,
};
use dsa_host_windows::vm::NetworkAdapter;
use serde_json::Value;

const VM: &str = "dsa-android-S-1-5-21-1-2-3-1001";

fn subnet(text: &str) -> Ipv4Subnet {
    let (prefix, length) = text.split_once('/').unwrap();
    Ipv4Subnet {
        prefix: prefix.parse().unwrap(),
        length: length.parse().unwrap(),
    }
}

fn wifi() -> HostAdapter {
    HostAdapter {
        name: "Wi-Fi".into(),
        description: "Intel(R) Wi-Fi 6E AX211 160MHz".into(),
        up: true,
        has_gateway: true,
        ipv4: vec![subnet("192.168.1.0/24")],
        global_ipv6: true,
        virtual_switch: false,
    }
}

#[test]
fn fr28_the_nat_network_is_named_after_the_vm_and_hands_out_addresses_and_dns() {
    let net = choose_nat_subnet(VM, &[]).unwrap();
    assert_eq!(net.length, 24);
    assert!(subnet("172.28.0.0/14").overlaps(&net));
    // The same VM gets the same subnet while it is free.
    assert_eq!(choose_nat_subnet(VM, &[]), Some(net));
    let doc = nat_network_document(&network_name(VM), net, None);
    assert_eq!(doc["Name"], format!("DSA {VM}"));
    assert_eq!(doc["Type"], "ICS");
    let flags = doc["Flags"].as_u64().unwrap();
    assert_eq!(flags & 1, 1, "the DNS proxy follows the host's DNS");
    assert_eq!(flags & 2, 2, "DHCP gives the guest its address");
    assert_eq!(flags & 8, 8, "Windows removes the network at restart");
    let ipv4 = &doc["Ipams"][0]["Subnets"][0];
    assert_eq!(ipv4["IpAddressPrefix"], net.to_string());
    assert_eq!(ipv4["Routes"][0]["NextHop"], net.gateway().to_string());
    assert_eq!(ipv4["Routes"][0]["DestinationPrefix"], "0.0.0.0/0");
    assert_eq!(doc["Ipams"][0]["Subnets"].as_array().unwrap().len(), 1);
}

#[test]
fn fr28_the_nat_subnet_keeps_clear_of_the_hosts_own_networks() {
    let first = choose_nat_subnet(VM, &[]).unwrap();
    let other = choose_nat_subnet(VM, &[first]).unwrap();
    assert_ne!(first, other);
    assert!(!other.overlaps(&first));
    // A host network that covers all of 172.28.0.0/14 leaves none.
    assert_eq!(choose_nat_subnet(VM, &[subnet("172.16.0.0/12")]), None);
    let adapters = vec![wifi()];
    assert_eq!(subnets_in_use(&adapters), vec![subnet("192.168.1.0/24")]);
}

#[test]
fn fr28_a_host_with_ipv6_gives_the_nat_network_an_ipv6_subnet() {
    assert!(host_has_ipv6(&[wifi()]));
    assert!(!host_has_ipv6(&[HostAdapter {
        global_ipv6: false,
        ..wifi()
    }]));
    let (prefix, length) = nat_ipv6_subnet(VM);
    assert_eq!(length, 64);
    assert_eq!(prefix.segments()[0], 0xfd5a, "a unique local prefix");
    let doc = nat_network_document(
        "n",
        choose_nat_subnet(VM, &[]).unwrap(),
        Some((prefix, length)),
    );
    let ipv6 = &doc["Ipams"][0]["Subnets"][1];
    assert_eq!(ipv6["IpAddressPrefix"], format!("{prefix}/64"));
    assert_eq!(ipv6["Routes"][0]["DestinationPrefix"], "::/0");
}

#[test]
fn fr28_advanced_networking_binds_a_transparent_network_to_the_connected_adapter() {
    let switch = HostAdapter {
        name: "vEthernet (External)".into(),
        description: "Hyper-V Virtual Ethernet Adapter".into(),
        virtual_switch: true,
        ..wifi()
    };
    let down = HostAdapter {
        name: "Ethernet".into(),
        up: false,
        ..wifi()
    };
    let adapters = vec![down.clone(), switch.clone(), wifi()];
    assert_eq!(advanced_adapter(&adapters).unwrap().name, "Wi-Fi");
    assert_eq!(advanced_problem(&adapters), None);
    // The user's own virtual switch is never taken over.
    let problem = advanced_problem(&[switch, down]).unwrap();
    assert!(problem.contains("virtual switch"), "{problem}");
    assert!(advanced_problem(&[]).unwrap().contains("NAT instead"));

    let doc = transparent_network_document(&network_name(VM), "Wi-Fi");
    assert_eq!(doc["Type"], "Transparent");
    assert_eq!(doc["Policies"][0]["Type"], "NetAdapterName");
    assert_eq!(
        doc["Policies"][0]["Settings"]["NetworkAdapterName"],
        "Wi-Fi"
    );
    assert!(doc.get("Ipams").is_none(), "the LAN gives the addresses");
}

#[test]
fn fr28_a_network_left_with_the_same_name_is_used_again_when_it_is_of_the_kind_wanted() {
    let query: Value = serde_json::from_str(&name_query(&network_name(VM))).unwrap();
    let filter: Value = serde_json::from_str(query["Filter"].as_str().unwrap()).unwrap();
    assert_eq!(filter["Name"], format!("DSA {VM}"));
    let id = "{6D5A0D5A-0000-4D5A-8D5A-00000000E0E0}";
    let ids = ids_from_enumeration(&format!("[\"{id}\"]"));
    assert_eq!(
        ids,
        vec!["6d5a0d5a-0000-4d5a-8d5a-00000000e0e0"
            .parse::<Guid>()
            .unwrap()]
    );
    assert!(ids_from_enumeration("not json").is_empty());

    let nat = network_facts(
        r#"{"Name":"DSA x","Type":"ICS","Ipams":[{"Type":"Static","Subnets":[{"IpAddressPrefix":"172.28.5.0/24"}]}]}"#,
    )
    .unwrap();
    assert_eq!(nat.subnets, vec![subnet("172.28.5.0/24")]);
    assert!(reusable(&nat, NetworkMode::Nat, None));
    assert!(!reusable(&nat, NetworkMode::Advanced, Some("Wi-Fi")));
    let lan = network_facts(
        r#"{"Type":"Transparent","Policies":[{"Type":"NetAdapterName","Settings":{"NetworkAdapterName":"Wi-Fi"}}]}"#,
    )
    .unwrap();
    assert!(reusable(&lan, NetworkMode::Advanced, Some("Wi-Fi")));
    // After the host moved to another adapter, the old network is not used again.
    assert!(!reusable(&lan, NetworkMode::Advanced, Some("Ethernet")));
}

#[test]
fn fr28_the_endpoint_gives_the_adapter_its_mac_and_the_status_its_addresses() {
    let network: Guid = "6d5a0d5a-0000-4d5a-8d5a-00000000e0e0".parse().unwrap();
    let doc = endpoint_document("DSA vm eth0", network);
    assert_eq!(doc["HostComputeNetwork"], network.to_string());
    let facts = endpoint_facts(
        r#"{"ID":"x","MacAddress":"00-15-5D-12-34-56",
            "IpConfigurations":[{"IpAddress":"172.28.5.2","PrefixLength":24},{"IpAddress":"fd5a:d5a:1:2::2","PrefixLength":64}],
            "Routes":[{"NextHop":"172.28.5.1","DestinationPrefix":"0.0.0.0/0"}],
            "Dns":{"ServerList":["172.28.5.1"]}}"#,
    )
    .unwrap();
    assert_eq!(facts.mac_address, "00-15-5D-12-34-56");
    assert_eq!(
        mac_bytes(&facts.mac_address),
        Some([0x00, 0x15, 0x5d, 0x12, 0x34, 0x56])
    );
    assert_eq!(mac_bytes("00:15:5d:12:34"), None);
    let status = NetworkStatusFacts {
        mode: NetworkMode::Nat,
        endpoint: facts.clone(),
        seen: vec![],
        problem: None,
    }
    .to_status();
    assert_eq!(status.mode, "nat");
    assert_eq!(status.addresses, vec!["172.28.5.2", "fd5a:d5a:1:2::2"]);
    assert_eq!(status.dns_servers, vec!["172.28.5.1"]);
    assert_eq!(status.gateway, "172.28.5.1");

    // A NAT endpoint that names no DNS server resolves through the gateway's DNS proxy.
    let quiet = EndpointFacts {
        dns_servers: vec![],
        ..facts.clone()
    };
    let status = NetworkStatusFacts {
        mode: NetworkMode::Nat,
        endpoint: quiet.clone(),
        seen: vec![],
        problem: None,
    }
    .to_status();
    assert_eq!(status.dns_servers, vec!["172.28.5.1"]);
    // On the LAN the addresses are the ones the guest was seen using.
    let status = NetworkStatusFacts {
        mode: NetworkMode::Advanced,
        endpoint: EndpointFacts {
            addresses: vec![],
            gateway: None,
            ..quiet
        },
        seen: vec![
            "2001:db8::5".parse().unwrap(),
            "192.168.1.50".parse().unwrap(),
        ],
        problem: None,
    }
    .to_status();
    assert_eq!(status.mode, "advanced");
    assert_eq!(status.addresses, vec!["192.168.1.50", "2001:db8::5"]);
}

#[test]
fn fr28_the_adapter_moves_to_a_new_endpoint_while_the_vm_runs() {
    let adapter = NetworkAdapter {
        endpoint_id: guid_from("endpoint"),
        mac_address: "00-15-5D-00-00-01".into(),
    };
    let add: Value = serde_json::from_str(&add_adapter_request(&adapter)).unwrap();
    assert_eq!(
        add["ResourcePath"],
        "VirtualMachine/Devices/NetworkAdapters/eth0"
    );
    assert_eq!(add["RequestType"], "Add");
    assert_eq!(
        add["Settings"]["EndpointId"],
        adapter.endpoint_id.to_string()
    );
    let remove: Value = serde_json::from_str(&remove_adapter_request()).unwrap();
    assert_eq!(remove["RequestType"], "Remove");
    assert_eq!(remove["ResourcePath"], add["ResourcePath"]);
    // IDs are fresh for each seed and look like version 4 GUIDs.
    assert_ne!(guid_from("a"), guid_from("b"));
    assert_eq!(guid_from("a").data3 >> 12, 4);
}

#[test]
fn fr28_networks_left_from_moving_the_vm_are_found_by_name_for_that_vm_only() {
    let query: Value = serde_json::from_str(&all_query()).unwrap();
    assert!(
        query.get("Filter").is_none(),
        "every network, whatever its name"
    );
    assert_eq!(
        object_name(r#"{"Name":"DSA vm 3","Type":"Transparent"}"#).as_deref(),
        Some("DSA vm 3")
    );
    assert_eq!(object_name("{}"), None);
    // A move names its network and endpoint after a count.
    assert!(moved_leftover(VM, &network_name(&format!("{VM} 1"))));
    assert!(moved_leftover(VM, &endpoint_name(&format!("{VM} 12"))));
    // The VM's own network is used again rather than removed; another VM's is not touched.
    assert!(!moved_leftover(VM, &network_name(VM)));
    assert!(!moved_leftover(VM, &endpoint_name(VM)));
    assert!(!moved_leftover(VM, &network_name(&format!("{VM}0 1"))));
    assert!(!moved_leftover(
        VM,
        &network_name("dsa-android-S-1-5-21-1-2-3-1002 1")
    ));
    assert!(!moved_leftover(VM, "Default Switch"));
    assert!(!moved_leftover(VM, &format!("DSA {VM} x")));
    assert!(!moved_leftover(VM, &format!("DSA {VM} ")));
}

fn nat_endpoint() -> EndpointFacts {
    endpoint_facts(
        r#"{"MacAddress":"00-15-5D-12-34-56",
            "IpConfigurations":[{"IpAddress":"172.28.5.2","PrefixLength":24}],
            "Routes":[{"NextHop":"172.28.5.1","DestinationPrefix":"0.0.0.0/0"}]}"#,
    )
    .unwrap()
}

#[test]
fn fr28_the_guests_default_route_is_the_hosts_nat_so_the_hosts_routes_and_vpn_apply() {
    let name = network_name(VM);
    let good = NatPath {
        gateway: "172.28.5.1".parse().unwrap(),
        host_has_gateway: true,
        host_route: Some(host_side_adapter(&name)),
    };
    assert_eq!(nat_path_problem(&name, &nat_endpoint(), &good), None);
    // The NAT document gives the guest the gateway as its default route.
    let subnet = subnet("172.28.5.0/24");
    let doc = nat_network_document(&name, subnet, None);
    assert_eq!(
        doc["Ipams"][0]["Subnets"][0]["Routes"][0]["NextHop"],
        good.gateway.to_string()
    );
    // A guest routed anywhere but the host does not go where the host's routes (a VPN's) go.
    let elsewhere = EndpointFacts {
        gateway: Some("172.28.5.254".parse().unwrap()),
        ..nat_endpoint()
    };
    let problem = nat_path_problem(&name, &elsewhere, &good).unwrap();
    assert!(problem.contains("VPN"), "{problem}");
    // Nor does one whose gateway the host no longer holds.
    let gone = NatPath {
        host_has_gateway: false,
        ..good
    };
    let problem = nat_path_problem(&name, &nat_endpoint(), &gone).unwrap();
    assert!(problem.contains("172.28.5.1"), "{problem}");
}

#[test]
fn fr28_the_host_reaches_the_guests_address_through_its_network_for_android() {
    let name = network_name(VM);
    assert_eq!(host_side_adapter(&name), format!("vEthernet (DSA {VM})"));
    let path = |route: Option<&str>| NatPath {
        gateway: "172.28.5.1".parse().unwrap(),
        host_has_gateway: true,
        host_route: route.map(str::to_string),
    };
    let own = host_side_adapter(&name);
    assert_eq!(
        nat_path_problem(&name, &nat_endpoint(), &path(Some(&own))),
        None
    );
    // Windows' adapter names are not case sensitive.
    assert_eq!(
        nat_path_problem(&name, &nat_endpoint(), &path(Some(&own.to_uppercase()))),
        None
    );
    // A VPN that takes the guest's subnet sends the host's traffic for it into the tunnel.
    let problem = nat_path_problem(&name, &nat_endpoint(), &path(Some("Corporate VPN"))).unwrap();
    assert!(problem.contains("Corporate VPN"), "{problem}");
    assert!(problem.contains("172.28.5.2"), "{problem}");
    let problem = nat_path_problem(&name, &nat_endpoint(), &path(None)).unwrap();
    assert!(problem.contains("no route"), "{problem}");
    // An endpoint without an IPv4 address has nothing for the host to reach.
    let no_address = EndpointFacts {
        addresses: vec![],
        ..nat_endpoint()
    };
    assert_eq!(nat_path_problem(&name, &no_address, &path(None)), None);
}

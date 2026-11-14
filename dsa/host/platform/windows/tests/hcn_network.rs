//! The Android VM's network made, used and removed through the real Host Compute Network.
//!
//! They make HCN networks on the machine they run on and need an administrator, so they are
//! ignored on a developer's machine and run on the Installer test's runners. No guest boots
//! there, so they check the host's side of the network: what is made, what is removed, and
//! the routes between the host and the guest's address.
#![cfg(windows)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::session::GuestLauncher;
use dsa_host_windows::android_vm::AndroidVmLauncher;
use dsa_host_windows::hcn::{
    endpoints_named, host_holds, host_interface_holding, host_route_to, networks_named,
    shared_status, SharedNetwork, VmNetwork, VmNetworking,
};
use dsa_host_windows::network::{endpoint_name, nat_path_problem, network_name};
use dsa_host_windows::vm::AndroidVmConfig;
use dsa_protocol::auth::BootSecret;

/// NAT networking for one VM, with what the settings read of it.
fn nat() -> (VmNetworking, SharedNetwork) {
    let shared = SharedNetwork::default();
    let networking = VmNetworking::new(Box::new(|| false), Arc::clone(&shared));
    (networking, shared)
}

fn assert_gone(vm: &str) {
    assert_eq!(
        networks_named(&network_name(vm)).unwrap(),
        vec![],
        "the network of {vm} is left"
    );
    assert_eq!(
        endpoints_named(&endpoint_name(vm)).unwrap(),
        vec![],
        "the endpoint of {vm} is left"
    );
}

#[test]
#[ignore = "makes HCN networks on this machine; run on the Installer test's runners"]
fn fr28_the_nat_network_and_its_endpoint_are_removed_when_the_vm_stops() {
    let vm = "dsa-test-fr28-stop";
    let (mut networking, shared) = nat();
    let adapter = networking.attach(vm).expect("HCN makes the VM's network");
    assert_eq!(networks_named(&network_name(vm)).unwrap().len(), 1);
    assert_eq!(
        endpoints_named(&endpoint_name(vm)).unwrap(),
        vec![adapter.endpoint_id]
    );
    assert!(shared_status(&shared).is_some());
    networking.detach();
    assert_gone(vm);
    assert!(
        shared_status(&shared).is_none(),
        "the settings show no network"
    );
}

#[test]
#[ignore = "makes HCN networks on this machine; run on the Installer test's runners"]
fn fr28_the_network_goes_when_the_service_lets_go_of_the_vm() {
    let vm = "dsa-test-fr28-drop";
    let (mut networking, _shared) = nat();
    networking.attach(vm).expect("HCN makes the VM's network");
    drop(networking);
    assert_gone(vm);
}

/// A VM whose kernel and disks are not real: HCS cannot start it, on a runner without
/// virtualization or with it.
fn unbootable(dir: &std::path::Path) -> AndroidVmConfig {
    std::fs::create_dir_all(dir).unwrap();
    let (kernel, initrd) = (dir.join("kernel"), dir.join("initrd.img"));
    std::fs::write(&kernel, b"not a kernel").unwrap();
    std::fs::write(&initrd, b"ramdisk").unwrap();
    AndroidVmConfig {
        owner: "DSA test".into(),
        kernel,
        initrd: Some(initrd),
        kernel_cmdline: String::new(),
        memory_mb: 1024,
        processors: 1,
        system_image: Some(dir.join("system.vhdx")),
        data_disk: dir.join("data.vhdx"),
        pstore_disk: None,
        addon_disk: None,
        socket_sids: Vec::new(),
        page_reporting_order: 9,
        console_pipe: None,
        image_version: None,
        hosting_process_name_suffix: None,
        fine_grained_memory: false,
        network_adapter: None,
        direct_mapped: None,
    }
}

#[test]
#[ignore = "makes HCN networks on this machine; run on the Installer test's runners"]
fn fr28_a_vm_that_fails_or_dies_while_it_starts_leaves_no_network() {
    let vm = "dsa-test-fr28-failed-start";
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fr28-failed-start");
    let attached = Arc::new(AtomicBool::new(false));
    let asked = Arc::clone(&attached);
    let networking = VmNetworking::new(
        Box::new(move || {
            // Asked once the network is about to be made, so the start got that far.
            asked.store(true, Ordering::SeqCst);
            false
        }),
        SharedNetwork::default(),
    );
    let mut launcher = AndroidVmLauncher::new(vm, unbootable(&dir))
        .with_boot_dir(dir.join("boot"))
        .with_networking(networking);
    launcher
        .start(&BootSecret::from_bytes([0x28; 32]))
        .expect_err("there is nothing to boot");
    assert!(
        attached.load(Ordering::SeqCst),
        "the start ended before the VM's network was made, so this checks nothing"
    );
    // Gone before the session stops the VM, and after.
    assert_gone(vm);
    launcher
        .stop(Duration::from_secs(5))
        .expect("stopping a VM that is not there succeeds");
    assert_gone(vm);
}

#[test]
#[ignore = "makes HCN networks on this machine; run on the Installer test's runners"]
fn fr28_a_network_left_by_a_service_that_ended_uncleanly_is_used_again_and_moves_are_removed() {
    let vm = "dsa-test-fr28-leftovers";
    let moved = format!("{vm} 1");
    // A service that ended with the VM on its network, after moving it once: nothing removed
    // either network.
    let (mut first, _) = nat();
    first.attach(vm).expect("HCN makes the VM's network");
    let left = networks_named(&network_name(vm)).unwrap();
    assert_eq!(left.len(), 1);
    std::mem::forget(first);
    let _on_the_move = VmNetwork::connect_as(vm, &moved, false).expect("HCN makes a second one");
    assert_eq!(networks_named(&network_name(&moved)).unwrap().len(), 1);

    // The next service starts the VM.
    let (mut second, shared) = nat();
    second
        .attach(vm)
        .expect("HCN gives the VM its network again");
    assert_eq!(
        networks_named(&network_name(vm)).unwrap(),
        left,
        "the network left with the same name is used again"
    );
    assert_eq!(
        endpoints_named(&endpoint_name(vm)).unwrap().len(),
        1,
        "the endpoint left from before is replaced, not kept beside the new one"
    );
    assert_eq!(networks_named(&network_name(&moved)).unwrap(), vec![]);
    assert_eq!(endpoints_named(&endpoint_name(&moved)).unwrap(), vec![]);
    assert!(shared_status(&shared).is_some());
    second.detach();
    assert_gone(vm);
}

#[test]
#[ignore = "makes HCN networks on this machine; run on the Installer test's runners"]
fn fr28_the_host_reaches_the_guests_address_and_the_guest_routes_through_the_host() {
    let vm = "dsa-test-fr28-routes";
    let (mut networking, shared) = nat();
    networking.attach(vm).expect("HCN makes the VM's network");
    let status = shared_status(&shared).expect("the settings show the network");
    eprintln!("the network's status: {status:?}");
    assert_eq!(status.mode, "nat");
    {
        let current = shared.lock().unwrap();
        let network = current.as_ref().expect("the VM has its network");
        let path = network
            .nat_path()
            .expect("a NAT network has a way through the host");
        eprintln!("the way between the host and the guest: {path:?}");
        let endpoint = network.status().endpoint;
        assert_eq!(nat_path_problem(&endpoint, &path), None);
        // HCN names no address for the guest: the endpoint asks for DHCP, and the guest takes
        // its address from the network's DHCP server when it brings eth0 up. No guest boots
        // here, so there is none yet.
        assert!(
            endpoint.dhcp,
            "the endpoint on a NAT network takes its address by DHCP"
        );
    }
    assert!(
        !status
            .addresses
            .iter()
            .filter_map(|a| a.parse::<IpAddr>().ok())
            .any(|a| a.is_ipv4()),
        "no guest has taken an address, yet the status shows {:?}",
        status.addresses
    );
    let gateway: Ipv4Addr = status
        .gateway
        .parse()
        .expect("the guest has a default route");
    // The address after the gateway, on the network's /24: one the DHCP server can give the
    // guest, and so routed as the guest's address is.
    let guest = Ipv4Addr::from(u32::from(gateway) + 1);
    assert_eq!(
        status.dns_servers,
        vec![gateway.to_string()],
        "the guest resolves through the gateway's DNS proxy"
    );

    // The guest's default route is the host itself, on its adapter for the network: the host
    // forwards what the guest sends, through the routes it has.
    assert!(
        host_holds(gateway),
        "the gateway {gateway} is not this computer's"
    );
    let own =
        host_interface_holding(gateway).expect("an adapter of this computer holds the gateway");
    // Windows names that adapter after the network, cut short ("vEthernet (DSA dsa-test-fr)"
    // here), so it is known by its interface, never by that name.
    eprintln!("this computer's adapter for Android's network: {own:?}");
    // The host sends to the guest's addresses through that adapter, not its default route.
    let route = host_route_to(guest);
    assert_eq!(
        route.as_ref().map(|r| r.luid),
        Some(own.luid),
        "this computer would not reach Android at {guest} through {own:?}, but {route:?}"
    );
    // And the host's own way out is not that adapter, so what the guest sends leaves where
    // this computer's traffic leaves: through a VPN when one is connected.
    let out = host_route_to(Ipv4Addr::new(8, 8, 8, 8));
    eprintln!("this computer's traffic leaves through {out:?}");
    assert_ne!(out.map(|o| o.luid), Some(own.luid));
    networking.detach();
    assert_gone(vm);
}

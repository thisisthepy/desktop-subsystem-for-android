//! The Android VM's network, as the Host Compute Network (HCN) documents that make it.
//!
//! Each VM gets a network of its own and one endpoint on it, which the VM's synthetic network
//! adapter is connected to (`vm::NetworkAdapter`). The guest's hv_netvsc drives that adapter
//! as eth0, and Android takes its address, route and DNS servers by DHCP.
//!
//! - By default the network is NAT: an HCN `ICS` network, the kind Windows' own "Default
//!   Switch" is. The host's connection sharing hands out the address by DHCP, and its DNS
//!   proxy on the gateway resolves through the host's own DNS servers, so the guest follows
//!   the host when its network changes (another Wi-Fi network, a VPN, a resume from sleep):
//!   the guest's address and gateway stay the same, and its traffic leaves through whatever
//!   route the host has now, a VPN included. The subnet is one of `172.28.0.0/14` that no
//!   host adapter uses. When the host has IPv6, the network gets a unique local IPv6 subnet
//!   too, so apps can use IPv6 to the host; a Windows that refuses an IPv6 subnet on such a
//!   network is reported (see [`NetworkStatusFacts::problem`]) and the VM keeps IPv4.
//! - With advanced networking the network is `Transparent`, bound to the host adapter that has
//!   the default route: the guest is a device of its own on the host's LAN, takes its
//!   addresses (IPv4 and IPv6) from the LAN, and other devices there reach it. When the host
//!   moves to another adapter, the host makes a network on that one and moves the VM's adapter
//!   to it while the VM runs.
//!
//! Behind NAT, the guest's default route is the gateway, which is the host's own address on
//! its adapter for the network (`vEthernet (DSA <vm>)`): the host forwards the guest's traffic
//! and translates it, so it leaves through whatever route the host takes, a VPN's included.
//! The host reaches the guest's address the other way, through the same adapter, whose subnet
//! route is more specific than any default route a VPN adds. Both are checked while the VM
//! runs ([`nat_path_problem`]) and reported in the status when they do not hold.
//!
//! The user's WSL network and the Hyper-V switches the user made are never touched: the
//! networks here are named after the VM, are not persistent (Windows removes them at restart),
//! and are deleted when the VM stops, when it fails to start and when the service lets go of
//! it. A network of the same name left by a service that did not stop cleanly is used again
//! when it is of the kind wanted, and replaced otherwise; one left from moving the VM to
//! another host adapter is removed at the VM's next start.
//!
//! This module only builds and reads the documents, so it is tested on every platform. The
//! calls into `computenetwork.dll` are in `hcn` (Windows only).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::guid::Guid;

/// How the VM reaches the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// Behind a NAT network of its own.
    Nat,
    /// On the host's LAN, through the host adapter named in the network.
    Advanced,
}

impl NetworkMode {
    /// The mode as the local API names it.
    pub fn name(self) -> &'static str {
        match self {
            NetworkMode::Nat => "nat",
            NetworkMode::Advanced => "advanced",
        }
    }

    /// The HCN network type of the mode.
    pub fn hcn_type(self) -> &'static str {
        match self {
            NetworkMode::Nat => "ICS",
            NetworkMode::Advanced => "Transparent",
        }
    }
}

/// HCN network flags: the DNS proxy and the DHCP server of connection sharing, and a network
/// Windows removes when it restarts, so none outlives the service by more than one session.
const FLAG_ENABLE_DNS_PROXY: u32 = 1;
const FLAG_ENABLE_DHCP_SERVER: u32 = 2;
const FLAG_NON_PERSISTENT: u32 = 8;

/// The HCN schema the documents are written in.
fn schema() -> Value {
    json!({ "Major": 2, "Minor": 0 })
}

/// The name of the network made for VM `vm_name`.
pub fn network_name(vm_name: &str) -> String {
    format!("DSA {vm_name}")
}

/// The name of the VM's endpoint.
pub fn endpoint_name(vm_name: &str) -> String {
    format!("DSA {vm_name} eth0")
}

/// An IPv4 subnet, as a prefix and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Subnet {
    pub prefix: Ipv4Addr,
    pub length: u8,
}

impl Ipv4Subnet {
    fn mask(length: u8) -> u32 {
        if length == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(length.min(32)))
        }
    }

    /// Whether this subnet and `other` share any address.
    pub fn overlaps(&self, other: &Ipv4Subnet) -> bool {
        let length = self.length.min(other.length);
        let mask = Ipv4Subnet::mask(length);
        u32::from(self.prefix) & mask == u32::from(other.prefix) & mask
    }

    /// The gateway: the first address of the subnet.
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.prefix) + 1)
    }
}

impl std::fmt::Display for Ipv4Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.prefix, self.length)
    }
}

/// How many /24 subnets `172.28.0.0/14` holds.
const NAT_SUBNETS: u32 = 1024;

fn hash_of(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// The NAT subnet for VM `vm_name`: a /24 in `172.28.0.0/14`, the same for the same VM while
/// it is free, and never one that `in_use` (the host adapters' subnets) overlaps. `None` when
/// every one is taken.
pub fn choose_nat_subnet(vm_name: &str, in_use: &[Ipv4Subnet]) -> Option<Ipv4Subnet> {
    let hash = hash_of(vm_name);
    let start = u32::from_be_bytes([0, 0, hash[0], hash[1]]) % NAT_SUBNETS;
    (0..NAT_SUBNETS).find_map(|step| {
        let index = (start + step) % NAT_SUBNETS;
        let candidate = Ipv4Subnet {
            prefix: Ipv4Addr::from(u32::from(Ipv4Addr::new(172, 28, 0, 0)) + (index << 8)),
            length: 24,
        };
        (!in_use.iter().any(|used| used.overlaps(&candidate))).then_some(candidate)
    })
}

/// The unique local IPv6 /64 for VM `vm_name`, `fd5a:d5a:<n>:<m>::/64`.
pub fn nat_ipv6_subnet(vm_name: &str) -> (Ipv6Addr, u8) {
    let hash = hash_of(vm_name);
    let prefix = Ipv6Addr::new(
        0xfd5a,
        0x0d5a,
        u16::from_be_bytes([hash[2], hash[3]]),
        u16::from_be_bytes([hash[4], hash[5]]),
        0,
        0,
        0,
        0,
    );
    (prefix, 64)
}

/// A GUID made from `seed`, for the IDs HCN asks the caller for.
pub fn guid_from(seed: &str) -> Guid {
    let h = hash_of(seed);
    let mut data4 = [0u8; 8];
    data4.copy_from_slice(&h[8..16]);
    // Version 4 and the RFC 4122 variant, so the ID reads as an ordinary random GUID.
    data4[0] = (data4[0] & 0x3f) | 0x80;
    Guid::new(
        u32::from_be_bytes([h[0], h[1], h[2], h[3]]),
        u16::from_be_bytes([h[4], h[5]]),
        (u16::from_be_bytes([h[6], h[7]]) & 0x0fff) | 0x4000,
        data4,
    )
}

/// The document that creates the NAT network `name` on `subnet`, with the unique local IPv6
/// subnet `ipv6` as well when it is given.
pub fn nat_network_document(name: &str, subnet: Ipv4Subnet, ipv6: Option<(Ipv6Addr, u8)>) -> Value {
    let mut subnets = vec![json!({
        "IpAddressPrefix": subnet.to_string(),
        "Routes": [{ "NextHop": subnet.gateway().to_string(), "DestinationPrefix": "0.0.0.0/0" }],
    })];
    if let Some((prefix, length)) = ipv6 {
        let gateway = Ipv6Addr::from(u128::from(prefix) + 1);
        subnets.push(json!({
            "IpAddressPrefix": format!("{prefix}/{length}"),
            "Routes": [{ "NextHop": gateway.to_string(), "DestinationPrefix": "::/0" }],
        }));
    }
    json!({
        "SchemaVersion": schema(),
        "Name": name,
        "Type": NetworkMode::Nat.hcn_type(),
        "Flags": FLAG_ENABLE_DNS_PROXY | FLAG_ENABLE_DHCP_SERVER | FLAG_NON_PERSISTENT,
        "Ipams": [{ "Type": "Static", "Subnets": subnets }],
    })
}

/// The document that creates the advanced (transparent) network `name` on the host adapter
/// `adapter` (its name as Windows shows it, such as "Wi-Fi"). The LAN gives the guest its
/// addresses, so the network has no subnet of its own.
pub fn transparent_network_document(name: &str, adapter: &str) -> Value {
    json!({
        "SchemaVersion": schema(),
        "Name": name,
        "Type": NetworkMode::Advanced.hcn_type(),
        "Flags": FLAG_NON_PERSISTENT,
        "Policies": [{
            "Type": "NetAdapterName",
            "Settings": { "NetworkAdapterName": adapter },
        }],
    })
}

/// The document that creates the VM's endpoint `name` on network `network`. HCN gives it its
/// MAC address and, on a NAT network, its addresses.
pub fn endpoint_document(name: &str, network: Guid) -> Value {
    json!({
        "SchemaVersion": schema(),
        "Name": name,
        "HostComputeNetwork": network.to_string(),
    })
}

/// The query that finds the HCN objects named `name`.
pub fn name_query(name: &str) -> String {
    json!({
        "SchemaVersion": schema(),
        "Filter": json!({ "Name": name }).to_string(),
    })
    .to_string()
}

/// The query that finds every HCN object of a kind, whatever its name.
pub fn all_query() -> String {
    json!({ "SchemaVersion": schema() }).to_string()
}

/// The name in an HCN network's or endpoint's properties, if it has one.
pub fn object_name(properties: &str) -> Option<String> {
    let value: Value = serde_json::from_str(properties).ok()?;
    Some(field(&value, "Name")?.as_str()?.to_string())
}

/// Whether `name` is a network or endpoint made to move VM `vm_name`'s adapter to another host
/// adapter (`DSA <vm> <n>` and `DSA <vm> <n> eth0`). The move names them after a count so they
/// do not take the place of the one the VM is on; a service that ends while such a network is
/// in use leaves it behind under that name, which the VM's own network name never matches.
pub fn moved_leftover(vm_name: &str, name: &str) -> bool {
    let Some(rest) = name.strip_prefix(&format!("{} ", network_name(vm_name))) else {
        return false;
    };
    let count = rest.strip_suffix(" eth0").unwrap_or(rest);
    !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit())
}

/// The name Windows gives the host's own adapter on the NAT network `network_name`: the
/// connection sharing gateway, through which the host reaches the guest.
pub fn host_side_adapter(network_name: &str) -> String {
    format!("vEthernet ({network_name})")
}

/// What the host found on the way between it and a guest behind NAT, for
/// [`nat_path_problem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatPath {
    /// The gateway the NAT network was made with: the host's address on that network.
    pub gateway: Ipv4Addr,
    /// Whether the gateway is one of the host's own addresses now.
    pub host_has_gateway: bool,
    /// The host adapter the host sends to the guest's IPv4 address through, by name; `None`
    /// when the host has no route to it.
    pub host_route: Option<String>,
}

/// Why traffic between the host and a guest behind NAT would not go as it must, if it would
/// not: the guest's default route must be the NAT gateway, an address of the host, so that the
/// guest's traffic is forwarded by the host and follows the host's routes (a VPN's among them);
/// and the host must send to the guest's address through its own adapter on that network, so
/// that programs on the host reach the guest's servers.
pub fn nat_path_problem(
    network_name: &str,
    endpoint: &EndpointFacts,
    path: &NatPath,
) -> Option<String> {
    let mut problems = Vec::new();
    match endpoint.gateway {
        Some(IpAddr::V4(gateway)) if gateway != path.gateway => problems.push(format!(
            "Android's default route goes to {gateway}, not to this computer ({}), so its traffic \
             does not follow this computer's routes or VPN",
            path.gateway
        )),
        _ => {}
    }
    if !path.host_has_gateway {
        problems.push(format!(
            "this computer does not hold the NAT gateway address {} any more, so Android's traffic \
             has nowhere to go",
            path.gateway
        ));
    }
    let guest = endpoint.addresses.iter().find_map(|a| match a {
        IpAddr::V4(v4) => Some(*v4),
        IpAddr::V6(_) => None,
    });
    if let Some(guest) = guest {
        let own = host_side_adapter(network_name);
        match path.host_route.as_deref() {
            Some(route) if route.eq_ignore_ascii_case(&own) => {}
            Some(route) => problems.push(format!(
                "this computer sends to Android's address {guest} through {route} rather than its \
                 network for Android, so programs here cannot reach Android's servers; a VPN that \
                 takes {guest}'s subnet does this"
            )),
            None => problems.push(format!(
                "this computer has no route to Android's address {guest}, so programs here cannot \
                 reach Android's servers"
            )),
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// The IDs in an HCN enumeration (a JSON array of GUID strings, with or without braces).
pub fn ids_from_enumeration(text: &str) -> Vec<Guid> {
    serde_json::from_str::<Vec<String>>(text)
        .unwrap_or_default()
        .iter()
        .filter_map(|id| id.trim_matches(['{', '}']).parse().ok())
        .collect()
}

/// What a network's properties say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkFacts {
    /// `ICS`, `NAT`, `Transparent`, ...
    pub kind: String,
    /// The IPv4 subnets of a NAT network.
    pub subnets: Vec<Ipv4Subnet>,
    /// The host adapter a transparent network is bound to.
    pub adapter: Option<String>,
}

fn parse_ipv4_subnet(text: &str) -> Option<Ipv4Subnet> {
    let (prefix, length) = text.split_once('/')?;
    Some(Ipv4Subnet {
        prefix: prefix.parse().ok()?,
        length: length.parse::<u8>().ok().filter(|l| *l <= 32)?,
    })
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// Reads a network's properties document.
pub fn network_facts(properties: &str) -> Option<NetworkFacts> {
    let value: Value = serde_json::from_str(properties).ok()?;
    let kind = field(&value, "Type")?.as_str()?.to_string();
    let subnets = field(&value, "Ipams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|ipam| field(ipam, "Subnets").and_then(Value::as_array))
        .flatten()
        .filter_map(|subnet| field(subnet, "IpAddressPrefix").and_then(Value::as_str))
        .filter_map(parse_ipv4_subnet)
        .collect();
    let adapter = field(&value, "Policies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|policy| field(policy, "Type").and_then(Value::as_str) == Some("NetAdapterName"))
        .filter_map(|policy| field(policy, "Settings"))
        .filter_map(|settings| field(settings, "NetworkAdapterName").and_then(Value::as_str))
        .map(str::to_string)
        .next();
    Some(NetworkFacts {
        kind,
        subnets,
        adapter,
    })
}

/// Whether an existing network of the right name can be used again for `mode` on `adapter`:
/// it must be of the same kind and, for advanced networking, bound to the same adapter.
pub fn reusable(facts: &NetworkFacts, mode: NetworkMode, adapter: Option<&str>) -> bool {
    facts.kind.eq_ignore_ascii_case(mode.hcn_type())
        && match mode {
            NetworkMode::Nat => !facts.subnets.is_empty(),
            NetworkMode::Advanced => facts.adapter.as_deref() == adapter,
        }
}

/// What an endpoint's properties say about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointFacts {
    /// `00-15-5D-...`, as the VM's adapter must carry it.
    pub mac_address: String,
    pub addresses: Vec<IpAddr>,
    pub gateway: Option<IpAddr>,
    pub dns_servers: Vec<IpAddr>,
}

/// Reads an endpoint's properties document.
pub fn endpoint_facts(properties: &str) -> Option<EndpointFacts> {
    let value: Value = serde_json::from_str(properties).ok()?;
    let mac_address = field(&value, "MacAddress")?.as_str()?.to_string();
    let addresses = field(&value, "IpConfigurations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|config| field(config, "IpAddress").and_then(Value::as_str))
        .filter_map(|ip| ip.parse().ok())
        .collect();
    let gateway = field(&value, "Routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|route| field(route, "NextHop").and_then(Value::as_str))
        .filter_map(|ip| ip.parse().ok())
        .find(|ip: &IpAddr| !ip.is_unspecified());
    let dns_servers = field(&value, "Dns")
        .and_then(|dns| field(dns, "ServerList"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|ip| ip.parse().ok())
        .collect();
    Some(EndpointFacts {
        mac_address,
        addresses,
        gateway,
        dns_servers,
    })
}

/// The MAC address as six bytes, from `00-15-5D-12-34-56` or `00:15:5d:12:34:56`.
pub fn mac_bytes(mac: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = mac
        .split(['-', ':'])
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<_>>()?;
    parts.try_into().ok()
}

/// A host adapter, as far as choosing a network needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAdapter {
    /// Its name as Windows shows it ("Wi-Fi", "Ethernet").
    pub name: String,
    pub description: String,
    pub up: bool,
    /// Whether it has a default gateway, so the host's traffic leaves through it.
    pub has_gateway: bool,
    pub ipv4: Vec<Ipv4Subnet>,
    /// Whether it has a global IPv6 address.
    pub global_ipv6: bool,
    /// A Hyper-V virtual adapter (`vEthernet (...)`), which a transparent network cannot be
    /// bound to.
    pub virtual_switch: bool,
}

/// The adapter advanced networking binds to: the first that is up, has a default gateway and
/// is a physical one. `None` when there is none.
pub fn advanced_adapter(adapters: &[HostAdapter]) -> Option<&HostAdapter> {
    adapters
        .iter()
        .find(|a| a.up && a.has_gateway && !a.virtual_switch)
}

/// Whether the host reaches the internet over IPv6, so the NAT network gets an IPv6 subnet.
pub fn host_has_ipv6(adapters: &[HostAdapter]) -> bool {
    adapters
        .iter()
        .any(|a| a.up && a.global_ipv6 && !a.virtual_switch)
}

/// The IPv4 subnets the host's adapters use, which the NAT subnet must stay clear of. The VM's
/// own network (its `vEthernet` adapter) is left out by the caller when it is being reused.
pub fn subnets_in_use(adapters: &[HostAdapter]) -> Vec<Ipv4Subnet> {
    adapters
        .iter()
        .flat_map(|a| a.ipv4.iter().copied())
        .collect()
}

/// Why advanced networking cannot be used now, if it cannot.
pub fn advanced_problem(adapters: &[HostAdapter]) -> Option<String> {
    if advanced_adapter(adapters).is_some() {
        return None;
    }
    if adapters
        .iter()
        .any(|a| a.up && a.has_gateway && a.virtual_switch)
    {
        Some(
            "advanced networking needs this computer's network adapter, which a Hyper-V virtual \
             switch already uses; Android uses NAT instead"
                .into(),
        )
    } else {
        Some(
            "advanced networking needs a connected network adapter, and this computer has none; \
             Android uses NAT instead"
                .into(),
        )
    }
}

/// The HCS modify request that removes the VM's network adapter while it runs.
pub fn remove_adapter_request() -> String {
    json!({
        "ResourcePath": format!("VirtualMachine/Devices/NetworkAdapters/{}", crate::vm::NETWORK_ADAPTER_NAME),
        "RequestType": "Remove",
    })
    .to_string()
}

/// The HCS modify request that adds `adapter` to the running VM.
pub fn add_adapter_request(adapter: &crate::vm::NetworkAdapter) -> String {
    json!({
        "ResourcePath": format!("VirtualMachine/Devices/NetworkAdapters/{}", crate::vm::NETWORK_ADAPTER_NAME),
        "RequestType": "Add",
        "Settings": adapter.to_hcs(),
    })
    .to_string()
}

/// What the host knows of the VM's network, for the status the settings show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatusFacts {
    pub mode: NetworkMode,
    pub endpoint: EndpointFacts,
    /// The guest's addresses the host has seen it use (its neighbour entries for the VM's MAC
    /// address), which are what the LAN gave it in advanced networking.
    pub seen: Vec<IpAddr>,
    /// Why the network is not what the settings ask for, if it is not.
    pub problem: Option<String>,
}

impl NetworkStatusFacts {
    /// The status as the local API reports it. The addresses are the ones seen in use, else
    /// those HCN gave the endpoint; the DNS servers are the endpoint's, else the gateway, whose
    /// DNS proxy a NAT network has.
    pub fn to_status(&self) -> dsa_host_core::host_api::NetworkStatus {
        let mut addresses: Vec<IpAddr> = self.seen.clone();
        for address in &self.endpoint.addresses {
            if !addresses.contains(address) {
                addresses.push(*address);
            }
        }
        // IPv4 first, then IPv6, each in the order found.
        addresses.sort_by_key(|a| a.is_ipv6());
        let dns: Vec<IpAddr> = if !self.endpoint.dns_servers.is_empty() {
            self.endpoint.dns_servers.clone()
        } else if self.mode == NetworkMode::Nat {
            self.endpoint.gateway.into_iter().collect()
        } else {
            Vec::new()
        };
        dsa_host_core::host_api::NetworkStatus {
            mode: self.mode.name().into(),
            addresses: addresses.iter().map(IpAddr::to_string).collect(),
            dns_servers: dns.iter().map(IpAddr::to_string).collect(),
            gateway: self
                .endpoint
                .gateway
                .map(|g| g.to_string())
                .unwrap_or_default(),
            problem: self.problem.clone(),
        }
    }
}

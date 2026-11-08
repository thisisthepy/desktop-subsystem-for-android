//! Making, reusing and removing the Android VM's network through the Host Compute Network
//! (HCN), and reading the host's own adapters.
//!
//! `computenetwork.dll` is loaded at run time rather than linked, as `computecore.dll` is in
//! `hcs`: a machine without it still runs the host process, and its absence is reported as
//! the reason the VM has no network, while Android starts without one.
//!
//! The documents are built and read by `network`, which is tested on every platform.

use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::core::{GUID, HRESULT, PCWSTR, PWSTR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetAdaptersAddresses, GetIpNetTable2, GAA_FLAG_INCLUDE_GATEWAYS,
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH, MIB_IPNET_TABLE2,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use crate::guid::Guid;
use crate::network::{
    advanced_adapter, advanced_problem, all_query, choose_nat_subnet, endpoint_document,
    endpoint_facts, endpoint_name, guid_from, host_has_ipv6, ids_from_enumeration, mac_bytes,
    moved_leftover, name_query, nat_ipv6_subnet, nat_network_document, nat_path_problem,
    network_facts, network_name, object_name, reusable, subnets_in_use,
    transparent_network_document, EndpointFacts, HostAdapter, Ipv4Subnet, NatPath, NetworkMode,
    NetworkStatusFacts,
};
use crate::vm::NetworkAdapter;

type Handle = *mut c_void;
type Enumerate = unsafe extern "system" fn(PCWSTR, *mut PWSTR, *mut PWSTR) -> HRESULT;
type CreateNetwork =
    unsafe extern "system" fn(*const GUID, PCWSTR, *mut Handle, *mut PWSTR) -> HRESULT;
type Open = unsafe extern "system" fn(*const GUID, *mut Handle, *mut PWSTR) -> HRESULT;
type Query = unsafe extern "system" fn(Handle, PCWSTR, *mut PWSTR, *mut PWSTR) -> HRESULT;
type Delete = unsafe extern "system" fn(*const GUID, *mut PWSTR) -> HRESULT;
type Close = unsafe extern "system" fn(Handle) -> HRESULT;
type CreateEndpoint =
    unsafe extern "system" fn(Handle, *const GUID, PCWSTR, *mut Handle, *mut PWSTR) -> HRESULT;

/// The `computenetwork.dll` functions this module uses.
struct Api {
    enumerate_networks: Enumerate,
    create_network: CreateNetwork,
    open_network: Open,
    query_network: Query,
    delete_network: Delete,
    close_network: Close,
    enumerate_endpoints: Enumerate,
    create_endpoint: CreateEndpoint,
    open_endpoint: Open,
    query_endpoint: Query,
    delete_endpoint: Delete,
    close_endpoint: Close,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    let name = wide("computenetwork.dll");
    // SAFETY: a NUL-terminated name; only System32 is searched.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(format!(
            "the Host Compute Network is not available (computenetwork.dll could not be loaded: \
             {}); turn on the Windows feature \"Virtual Machine Platform\" and restart",
            std::io::Error::last_os_error()
        ));
    }
    macro_rules! function {
        ($name:literal, $type:ty) => {{
            // SAFETY: a NUL-terminated ASCII name on a loaded module.
            match unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) } {
                // SAFETY: the export has this signature in ComputeNetwork.h.
                Some(f) => unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, $type>(f)
                },
                None => return Err(format!("computenetwork.dll has no {}", $name)),
            }
        }};
    }
    // The module stays loaded for the life of the process.
    Ok(Api {
        enumerate_networks: function!("HcnEnumerateNetworks", Enumerate),
        create_network: function!("HcnCreateNetwork", CreateNetwork),
        open_network: function!("HcnOpenNetwork", Open),
        query_network: function!("HcnQueryNetworkProperties", Query),
        delete_network: function!("HcnDeleteNetwork", Delete),
        close_network: function!("HcnCloseNetwork", Close),
        enumerate_endpoints: function!("HcnEnumerateEndpoints", Enumerate),
        create_endpoint: function!("HcnCreateEndpoint", CreateEndpoint),
        open_endpoint: function!("HcnOpenEndpoint", Open),
        query_endpoint: function!("HcnQueryEndpointProperties", Query),
        delete_endpoint: function!("HcnDeleteEndpoint", Delete),
        close_endpoint: function!("HcnCloseEndpoint", Close),
    })
}

/// Takes ownership of a string HCN allocated.
fn take(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: HCN returns a NUL-terminated wide string allocated with CoTaskMemAlloc.
    let result = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: freed once, as HCN documents.
    unsafe { CoTaskMemFree(text as *const c_void) };
    result
}

fn guid_ptr(id: &Guid) -> *const GUID {
    // Guid has the layout of the Windows GUID structure.
    id as *const Guid as *const GUID
}

/// Turns a failed HCN call into a sentence with its error record.
fn check(what: &str, hresult: HRESULT, record: PWSTR) -> Result<(), String> {
    let record = take(record);
    if hresult < 0 {
        let mut message = format!("{what} failed (HRESULT 0x{:08x})", hresult as u32);
        if !record.is_empty() {
            message.push_str(": ");
            message.push_str(record.trim());
        }
        Err(message)
    } else {
        Ok(())
    }
}

fn enumerate(function: Enumerate, what: &str, name: &str) -> Result<Vec<Guid>, String> {
    enumerate_matching(function, what, &name_query(name))
}

fn enumerate_matching(function: Enumerate, what: &str, query: &str) -> Result<Vec<Guid>, String> {
    let query = wide(query);
    let (mut found, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a NUL-terminated query and out pointers that outlive the call.
    let hresult = unsafe { function(query.as_ptr(), &mut found, &mut record) };
    let found = take(found);
    check(what, hresult, record)?;
    Ok(ids_from_enumeration(&found))
}

fn delete(function: Delete, what: &str, id: &Guid) -> Result<(), String> {
    let mut record = std::ptr::null_mut();
    // SAFETY: a live GUID and an out pointer.
    let hresult = unsafe { function(guid_ptr(id), &mut record) };
    check(what, hresult, record)
}

/// The properties of the network `id`, if it can be opened.
fn network_properties(api: &Api, id: &Guid) -> Result<String, String> {
    let (mut handle, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live GUID and out pointers.
    let hresult = unsafe { (api.open_network)(guid_ptr(id), &mut handle, &mut record) };
    check("HcnOpenNetwork", hresult, record)?;
    let query = wide("");
    let (mut properties, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live handle, a NUL-terminated query and out pointers.
    let hresult =
        unsafe { (api.query_network)(handle, query.as_ptr(), &mut properties, &mut record) };
    let properties = take(properties);
    // SAFETY: closed once.
    unsafe { (api.close_network)(handle) };
    check("HcnQueryNetworkProperties", hresult, record)?;
    Ok(properties)
}

/// The properties of the endpoint `id`, if it can be opened.
fn endpoint_properties(api: &Api, id: &Guid) -> Result<String, String> {
    let (mut handle, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live GUID and out pointers.
    let hresult = unsafe { (api.open_endpoint)(guid_ptr(id), &mut handle, &mut record) };
    check("HcnOpenEndpoint", hresult, record)?;
    let query = wide("");
    let (mut properties, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live handle, a NUL-terminated query and out pointers.
    let hresult =
        unsafe { (api.query_endpoint)(handle, query.as_ptr(), &mut properties, &mut record) };
    let properties = take(properties);
    // SAFETY: closed once.
    unsafe { (api.close_endpoint)(handle) };
    check("HcnQueryEndpointProperties", hresult, record)?;
    Ok(properties)
}

/// The IDs of the HCN networks named `name`.
pub fn networks_named(name: &str) -> Result<Vec<Guid>, String> {
    let api = api()?;
    enumerate(api.enumerate_networks, "HcnEnumerateNetworks", name)
}

/// The IDs of the HCN endpoints named `name`.
pub fn endpoints_named(name: &str) -> Result<Vec<Guid>, String> {
    let api = api()?;
    enumerate(api.enumerate_endpoints, "HcnEnumerateEndpoints", name)
}

/// Removes the networks and endpoints left from moving VM `vm_name`'s adapter to another host
/// adapter by a service that ended while the VM was on one: the endpoints first, since an
/// endpoint holds its network. Called when the VM starts, when none of them is in use.
fn remove_moved_leftovers(api: &Api, vm_name: &str) {
    let named = |ids: Vec<Guid>, properties: &dyn Fn(&Guid) -> Result<String, String>| {
        ids.into_iter()
            .filter(|id| {
                properties(id)
                    .ok()
                    .and_then(|p| object_name(&p))
                    .is_some_and(|name| moved_leftover(vm_name, &name))
            })
            .collect::<Vec<_>>()
    };
    let all = all_query();
    match enumerate_matching(api.enumerate_endpoints, "HcnEnumerateEndpoints", &all) {
        Ok(ids) => {
            for stale in named(ids, &|id| endpoint_properties(api, id)) {
                match delete(api.delete_endpoint, "HcnDeleteEndpoint", &stale) {
                    Ok(()) => eprintln!("network: removed an endpoint left from moving {vm_name}"),
                    Err(why) => eprintln!(
                        "network: an endpoint left from moving {vm_name} could not be removed: {why}"
                    ),
                }
            }
        }
        Err(why) => eprintln!("network: the endpoints left from before cannot be listed: {why}"),
    }
    match enumerate_matching(api.enumerate_networks, "HcnEnumerateNetworks", &all) {
        Ok(ids) => {
            for stale in named(ids, &|id| network_properties(api, id)) {
                match delete(api.delete_network, "HcnDeleteNetwork", &stale) {
                    Ok(()) => eprintln!("network: removed a network left from moving {vm_name}"),
                    Err(why) => eprintln!(
                        "network: a network left from moving {vm_name} could not be removed: {why}"
                    ),
                }
            }
        }
        Err(why) => eprintln!("network: the networks left from before cannot be listed: {why}"),
    }
}

fn create_network(api: &Api, id: &Guid, document: &str) -> Result<Handle, String> {
    let settings = wide(document);
    let (mut handle, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live GUID, a NUL-terminated document and out pointers.
    let hresult =
        unsafe { (api.create_network)(guid_ptr(id), settings.as_ptr(), &mut handle, &mut record) };
    check("HcnCreateNetwork", hresult, record)?;
    Ok(handle)
}

fn open_network(api: &Api, id: &Guid) -> Result<Handle, String> {
    let (mut handle, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live GUID and out pointers.
    let hresult = unsafe { (api.open_network)(guid_ptr(id), &mut handle, &mut record) };
    check("HcnOpenNetwork", hresult, record)?;
    Ok(handle)
}

/// Creates the endpoint `id` on the open network `network` and reads back its properties.
fn create_endpoint(
    api: &Api,
    network: Handle,
    id: &Guid,
    document: &str,
) -> Result<EndpointFacts, String> {
    let settings = wide(document);
    let (mut handle, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live network handle, GUID, NUL-terminated document and out pointers.
    let hresult = unsafe {
        (api.create_endpoint)(
            network,
            guid_ptr(id),
            settings.as_ptr(),
            &mut handle,
            &mut record,
        )
    };
    check("HcnCreateEndpoint", hresult, record)?;
    let query = wide("");
    let (mut properties, mut record) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: a live handle, a NUL-terminated query and out pointers.
    let hresult =
        unsafe { (api.query_endpoint)(handle, query.as_ptr(), &mut properties, &mut record) };
    let properties = take(properties);
    // SAFETY: closed once; the endpoint stays until it is deleted.
    unsafe { (api.close_endpoint)(handle) };
    check("HcnQueryEndpointProperties", hresult, record)?;
    endpoint_facts(&properties).ok_or_else(|| {
        format!("HCN described the new endpoint in a way DSA cannot read: {properties}")
    })
}

/// The network a VM's adapter is connected to, made for that VM.
pub struct VmNetwork {
    vm_name: String,
    /// The network's name.
    name: String,
    network_id: Guid,
    endpoint_id: Guid,
    mode: NetworkMode,
    /// The host adapter an advanced network is bound to.
    host_adapter: Option<String>,
    endpoint: EndpointFacts,
    /// The gateway of a NAT network: the host's address on it, the guest's default route.
    nat_gateway: Option<Ipv4Addr>,
    problem: Option<String>,
}

impl VmNetwork {
    /// Makes the network and the endpoint for VM `vm_name`: advanced networking when `advanced`
    /// asks for it and the host has an adapter it can use, NAT otherwise. A network of the
    /// same name left from before is used again when it is of the kind wanted; one left from
    /// moving the VM to another host adapter is removed.
    pub fn connect(vm_name: &str, advanced: bool) -> Result<VmNetwork, String> {
        remove_moved_leftovers(api()?, vm_name);
        VmNetwork::connect_as(vm_name, vm_name, advanced)
    }

    /// As [`connect`](Self::connect), with the network and endpoint named after `label`
    /// rather than the VM: a network made to move a running VM to another host adapter must
    /// not take the name, and so the place, of the one the VM is still on. A move names it
    /// `<vm> <n>`, which the VM's next [`connect`](Self::connect) removes if it is left.
    pub fn connect_as(vm_name: &str, label: &str, advanced: bool) -> Result<VmNetwork, String> {
        let api = api()?;
        let adapters = host_adapters().unwrap_or_else(|why| {
            eprintln!("network: {why}; choosing the network without the host's adapters");
            Vec::new()
        });
        let (mut mode, mut host_adapter, mut problem) = if advanced {
            match advanced_adapter(&adapters) {
                Some(adapter) => (NetworkMode::Advanced, Some(adapter.name.clone()), None),
                None => (NetworkMode::Nat, None, advanced_problem(&adapters)),
            }
        } else {
            (NetworkMode::Nat, None, None)
        };
        let name = network_name(label);
        // An endpoint left by a service that did not stop cleanly holds the network; it goes.
        let ep_name = endpoint_name(label);
        for stale in enumerate(api.enumerate_endpoints, "HcnEnumerateEndpoints", &ep_name)? {
            if let Err(why) = delete(api.delete_endpoint, "HcnDeleteEndpoint", &stale) {
                eprintln!("network: an endpoint left from before could not be removed: {why}");
            }
        }
        let mut network_id = None;
        let mut nat_gateway = None;
        for existing in enumerate(api.enumerate_networks, "HcnEnumerateNetworks", &name)? {
            let facts = network_properties(api, &existing)
                .ok()
                .and_then(|properties| network_facts(&properties));
            if network_id.is_none()
                && facts
                    .as_ref()
                    .is_some_and(|f| reusable(f, mode, host_adapter.as_deref()))
            {
                eprintln!("network: using the network {name} left from before");
                network_id = Some(existing);
                if mode == NetworkMode::Nat {
                    nat_gateway = facts
                        .as_ref()
                        .and_then(|f| f.subnets.first())
                        .map(Ipv4Subnet::gateway);
                }
            } else if let Err(why) = delete(api.delete_network, "HcnDeleteNetwork", &existing) {
                eprintln!(
                    "network: the network {name} left from before could not be removed: {why}"
                );
            }
        }
        let network_id = match network_id {
            Some(id) => id,
            None => {
                let id = guid_from(&format!(
                    "{name} network {:?}",
                    std::time::SystemTime::now()
                ));
                let made = match (mode, host_adapter.as_deref()) {
                    (NetworkMode::Advanced, Some(adapter)) => {
                        let document = transparent_network_document(&name, adapter).to_string();
                        create_network(api, &id, &document).map_err(|why| {
                            format!("advanced networking on {adapter} could not be set up ({why})")
                        })
                    }
                    _ => Err(String::new()),
                };
                match made {
                    Ok(handle) => {
                        // SAFETY: closed once.
                        unsafe { (api.close_network)(handle) };
                        id
                    }
                    Err(why) => {
                        if mode == NetworkMode::Advanced {
                            problem = Some(format!("{why}; Android uses NAT instead"));
                            mode = NetworkMode::Nat;
                            host_adapter = None;
                        }
                        let (handle, subnet, ipv6_problem) =
                            create_nat(api, &id, vm_name, &name, &adapters)?;
                        nat_gateway = Some(subnet.gateway());
                        // SAFETY: closed once.
                        unsafe { (api.close_network)(handle) };
                        if problem.is_none() {
                            problem = ipv6_problem;
                        }
                        id
                    }
                }
            }
        };
        let endpoint_id = guid_from(&format!("{ep_name} {:?}", std::time::SystemTime::now()));
        let network = open_network(api, &network_id)?;
        let endpoint = create_endpoint(
            api,
            network,
            &endpoint_id,
            &endpoint_document(&ep_name, network_id).to_string(),
        );
        // SAFETY: closed once.
        unsafe { (api.close_network)(network) };
        let endpoint = match endpoint {
            Ok(endpoint) => endpoint,
            Err(why) => {
                let _ = delete(api.delete_network, "HcnDeleteNetwork", &network_id);
                return Err(why);
            }
        };
        eprintln!(
            "network: {vm_name} is on {} ({}), adapter {}{}",
            name,
            mode.name(),
            endpoint.mac_address,
            problem
                .as_deref()
                .map(|p| format!("; {p}"))
                .unwrap_or_default()
        );
        Ok(VmNetwork {
            vm_name: vm_name.to_string(),
            name: name.clone(),
            network_id,
            endpoint_id,
            mode,
            host_adapter,
            endpoint,
            nat_gateway,
            problem,
        })
    }

    /// The VM's adapter on this network.
    pub fn adapter(&self) -> NetworkAdapter {
        NetworkAdapter {
            endpoint_id: self.endpoint_id,
            mac_address: self.endpoint.mac_address.clone(),
        }
    }

    pub fn mode(&self) -> NetworkMode {
        self.mode
    }

    /// The network's name, which its endpoint's and the host's own adapter's names carry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The way between the host and a guest behind NAT, as it stands now: whether the host
    /// still holds the gateway and which adapter it reaches the guest through. `None` on the
    /// LAN, where the host's NAT is not on the way.
    pub fn nat_path(&self) -> Option<NatPath> {
        let gateway = self.nat_gateway?;
        let guest = self.endpoint.addresses.iter().find_map(|a| match a {
            IpAddr::V4(v4) => Some(*v4),
            IpAddr::V6(_) => None,
        });
        Some(NatPath {
            gateway,
            host_has_gateway: host_holds(gateway),
            host_route: guest.and_then(host_route_to),
        })
    }

    /// The network as it stands, with the addresses the guest has been seen using. Behind
    /// NAT, a way between the host and the guest that no longer goes through the host's
    /// network for Android (a VPN that took its subnet, the gateway gone) is its problem.
    pub fn status(&self) -> NetworkStatusFacts {
        let path = self
            .nat_path()
            .and_then(|path| nat_path_problem(&self.name, &self.endpoint, &path));
        let problem = match (self.problem.clone(), path) {
            (Some(first), Some(second)) => Some(format!("{first}; {second}")),
            (first, second) => first.or(second),
        };
        NetworkStatusFacts {
            mode: self.mode,
            endpoint: self.endpoint.clone(),
            seen: mac_bytes(&self.endpoint.mac_address)
                .map(|mac| neighbour_addresses(&mac))
                .unwrap_or_default(),
            problem,
        }
    }

    /// The adapter an advanced network should be on now, when it is not the one it is on: the
    /// host moved to another network adapter. `None` for NAT, which follows the host by itself.
    pub fn moved_adapter(&self) -> Option<String> {
        if self.mode != NetworkMode::Advanced {
            return None;
        }
        let adapters = host_adapters().ok()?;
        let wanted = advanced_adapter(&adapters)?;
        (Some(wanted.name.as_str()) != self.host_adapter.as_deref()).then(|| wanted.name.clone())
    }

    /// Deletes the endpoint and the network. Each failure is logged; neither stops the other.
    pub fn remove(self) {
        let Ok(api) = api() else { return };
        if let Err(why) = delete(api.delete_endpoint, "HcnDeleteEndpoint", &self.endpoint_id) {
            eprintln!(
                "network: {}'s endpoint could not be removed: {why}",
                self.vm_name
            );
        }
        if let Err(why) = delete(api.delete_network, "HcnDeleteNetwork", &self.network_id) {
            eprintln!(
                "network: {}'s network could not be removed: {why}",
                self.vm_name
            );
        } else {
            eprintln!("network: removed {}", self.name);
        }
    }
}

/// Says at each start whether the user wants advanced networking.
pub type AdvancedWanted = Box<dyn FnMut() -> bool + Send>;

/// The running VM's network, which the settings read the status of.
pub type SharedNetwork = Arc<Mutex<Option<VmNetwork>>>;

/// How often a running VM's advanced network checks that it is on the adapter the host uses.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(5);

/// The network of one user's VM across its starts: made at each start, followed while the VM
/// runs, removed when it stops. Every failure is logged and leaves the VM without a network
/// (or on the one it has) rather than stopping it.
pub struct VmNetworking {
    advanced: AdvancedWanted,
    current: SharedNetwork,
    /// The network a move to another host adapter is being made on.
    moving: Option<Receiver<Result<VmNetwork, String>>>,
    checked: Option<Instant>,
    moves: u32,
}

impl VmNetworking {
    pub fn new(advanced: AdvancedWanted, current: SharedNetwork) -> VmNetworking {
        VmNetworking {
            advanced,
            current,
            moving: None,
            checked: None,
            moves: 0,
        }
    }

    /// Makes the network for the VM about to start, and returns the adapter to give it.
    pub fn attach(&mut self, vm_name: &str) -> Option<NetworkAdapter> {
        self.detach();
        let advanced = (self.advanced)();
        let made = std::panic::catch_unwind(|| VmNetwork::connect(vm_name, advanced))
            .unwrap_or_else(|_| Err("making the network panicked".into()));
        match made {
            Ok(network) => {
                let adapter = network.adapter();
                *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(network);
                Some(adapter)
            }
            Err(why) => {
                eprintln!("network: {vm_name} starts without a network: {why}");
                None
            }
        }
    }

    /// Keeps a running VM's advanced network on the adapter the host uses. `modify` sends an
    /// HCS modify request to the running VM. It returns at once; a new network is made on a
    /// thread of its own, and the adapter moves to it at a later call.
    pub fn follow(&mut self, vm_name: &str, modify: &dyn Fn(&str) -> Result<(), String>) {
        if let Some(moving) = &self.moving {
            match moving.try_recv() {
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => self.moving = None,
                Ok(Err(why)) => {
                    self.moving = None;
                    eprintln!("network: {vm_name} stays on its network: {why}");
                }
                Ok(Ok(next)) => {
                    self.moving = None;
                    self.move_to(vm_name, next, modify);
                }
            }
        }
        if self
            .checked
            .is_some_and(|at| at.elapsed() < FOLLOW_INTERVAL)
        {
            return;
        }
        self.checked = Some(Instant::now());
        let moved = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(VmNetwork::moved_adapter);
        let Some(adapter) = moved else { return };
        eprintln!("network: this computer now uses {adapter}; moving {vm_name}'s network to it");
        self.moves += 1;
        let (send, receive) = mpsc::channel();
        let (vm, label) = (vm_name.to_string(), format!("{vm_name} {}", self.moves));
        let spawned = std::thread::Builder::new()
            .name("dsa-network-move".into())
            .spawn(move || {
                let made = std::panic::catch_unwind(|| VmNetwork::connect_as(&vm, &label, true))
                    .unwrap_or_else(|_| Err("making the network panicked".into()));
                // The VM stopped while the network was made: nothing will use it.
                if let Err(mpsc::SendError(Ok(unused))) = send.send(made) {
                    unused.remove();
                }
            });
        match spawned {
            Ok(_) => self.moving = Some(receive),
            Err(why) => eprintln!("network: cannot move {vm_name}'s network: {why}"),
        }
    }

    fn move_to(
        &mut self,
        vm_name: &str,
        next: VmNetwork,
        modify: &dyn Fn(&str) -> Result<(), String>,
    ) {
        let removed = modify(&crate::network::remove_adapter_request());
        let added =
            removed.and_then(|()| modify(&crate::network::add_adapter_request(&next.adapter())));
        match added {
            Ok(()) => {
                let old = self
                    .current
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .replace(next);
                if let Some(old) = old {
                    old.remove();
                }
                eprintln!("network: {vm_name}'s adapter moved");
            }
            Err(why) => {
                eprintln!("network: {vm_name}'s adapter could not be moved: {why}");
                // The adapter may be gone from the VM; put the one it had back.
                let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(current) = current.as_ref() {
                    let _ = modify(&crate::network::add_adapter_request(&current.adapter()));
                }
                drop(current);
                next.remove();
            }
        }
    }

    /// Removes the stopped VM's network.
    pub fn detach(&mut self) {
        self.moving = None;
        self.checked = None;
        let current = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(network) = current {
            network.remove();
        }
    }
}

/// The service letting go of the VM (it is ending, or the user's Android is made again) takes
/// its network with it. The launcher holds the VM before its networking, so the VM is gone by
/// then.
impl Drop for VmNetworking {
    fn drop(&mut self) {
        self.detach();
    }
}

/// The status of a user's network, for the settings: `None` while the VM has none.
pub fn shared_status(shared: &SharedNetwork) -> Option<dsa_host_core::host_api::NetworkStatus> {
    let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|network| network.status().to_status())
}

/// Creates the NAT network `id`, with an IPv6 subnet when the host has IPv6. A Windows that
/// refuses the IPv6 subnet gets the network without it, and the reason is returned. The
/// network's IPv4 subnet is returned too.
fn create_nat(
    api: &Api,
    id: &Guid,
    vm_name: &str,
    name: &str,
    adapters: &[HostAdapter],
) -> Result<(Handle, Ipv4Subnet, Option<String>), String> {
    let in_use: Vec<Ipv4Subnet> = subnets_in_use(adapters);
    let subnet = choose_nat_subnet(vm_name, &in_use).ok_or_else(|| {
        "every NAT subnet DSA uses (172.28.0.0/14) is taken by this computer's networks".to_string()
    })?;
    if host_has_ipv6(adapters) {
        let document = nat_network_document(name, subnet, Some(nat_ipv6_subnet(vm_name)));
        match create_network(api, id, &document.to_string()) {
            Ok(handle) => return Ok((handle, subnet, None)),
            Err(why) => {
                let problem = format!(
                    "this computer has IPv6, but Windows refused an IPv6 subnet for the NAT \
                     network ({why}); Android uses IPv4, and advanced networking gives it IPv6"
                );
                eprintln!("network: {problem}");
                let document = nat_network_document(name, subnet, None);
                return create_network(api, id, &document.to_string())
                    .map(|h| (h, subnet, Some(problem)));
            }
        }
    }
    let document = nat_network_document(name, subnet, None);
    create_network(api, id, &document.to_string()).map(|handle| (handle, subnet, None))
}

/// Whether `address` is one of the host's own addresses now: a socket can be bound to it only
/// then.
pub fn host_holds(address: Ipv4Addr) -> bool {
    std::net::UdpSocket::bind((address, 0)).is_ok()
}

/// The host adapter, by the name Windows shows, that the host sends to `address` through;
/// `None` when it has no route there.
pub fn host_route_to(address: Ipv4Addr) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceIndexToLuid, ConvertInterfaceLuidToAlias, GetBestInterfaceEx,
    };
    // NDIS_IF_MAX_STRING_SIZE in ifdef.h: the longest adapter name, without its NUL.
    const IF_MAX_STRING_SIZE: usize = 256;
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    // SAFETY: an all-zero SOCKADDR_IN is valid; its family and address are set below.
    let mut destination: SOCKADDR_IN = unsafe { std::mem::zeroed() };
    destination.sin_family = AF_INET;
    destination.sin_addr.S_un.S_addr = u32::from(address).to_be();
    let mut index = 0u32;
    // SAFETY: a socket address of its family's size and an out pointer.
    let result = unsafe {
        GetBestInterfaceEx(
            &destination as *const SOCKADDR_IN as *const SOCKADDR,
            &mut index,
        )
    };
    if result != 0 {
        return None;
    }
    // SAFETY: a LUID is plain data, which all zeros is.
    let mut luid: NET_LUID_LH = unsafe { std::mem::zeroed() };
    // SAFETY: an interface index and an out pointer.
    if unsafe { ConvertInterfaceIndexToLuid(index, &mut luid) } != 0 {
        return None;
    }
    let mut alias = [0u16; IF_MAX_STRING_SIZE + 1];
    // SAFETY: a LUID and a buffer of the length given, in characters.
    if unsafe { ConvertInterfaceLuidToAlias(&luid, alias.as_mut_ptr(), alias.len()) } != 0 {
        return None;
    }
    Some(from_wide(alias.as_mut_ptr()))
}

/// An IP address from a socket address, if it is IPv4 or IPv6.
///
/// # Safety
/// `address` is null or points at a socket address of its family's size.
unsafe fn ip_of(address: *const SOCKADDR) -> Option<IpAddr> {
    if address.is_null() {
        return None;
    }
    match (*address).sa_family {
        AF_INET => {
            let v4 = &*(address as *const SOCKADDR_IN);
            Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                v4.sin_addr.S_un.S_addr,
            ))))
        }
        AF_INET6 => {
            let v6 = &*(address as *const SOCKADDR_IN6);
            Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.u.Byte)))
        }
        _ => None,
    }
}

fn from_wide(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string the adapter list owns.
    unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    }
}

fn subnet_of(address: Ipv4Addr, length: u8) -> Ipv4Subnet {
    let mask = if length == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(length.min(32)))
    };
    Ipv4Subnet {
        prefix: Ipv4Addr::from(u32::from(address) & mask),
        length,
    }
}

/// The host's network adapters.
pub fn host_adapters() -> Result<Vec<HostAdapter>, String> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
    let mut size: u32 = 16 * 1024;
    let mut buffer: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buffer.resize((size as usize).div_ceil(8), 0);
        // SAFETY: a buffer of `size` bytes, aligned for the structures written into it.
        let result = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                flags,
                std::ptr::null(),
                buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                &mut size,
            )
        };
        match result {
            0 => break,
            // ERROR_BUFFER_OVERFLOW: `size` now says how much is needed.
            111 => continue,
            // ERROR_NO_DATA: no adapters.
            232 => return Ok(Vec::new()),
            error => {
                return Err(format!(
                    "the host's network adapters could not be read (error {error})"
                ))
            }
        }
    }
    let mut adapters = Vec::new();
    let mut at = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !at.is_null() {
        // SAFETY: a list GetAdaptersAddresses wrote into `buffer`, which outlives the loop.
        let adapter = unsafe { &*at };
        let name = from_wide(adapter.FriendlyName);
        let description = from_wide(adapter.Description);
        let mut ipv4 = Vec::new();
        let mut global_ipv6 = false;
        let mut unicast = adapter.FirstUnicastAddress;
        while !unicast.is_null() {
            // SAFETY: as above.
            let entry = unsafe { &*unicast };
            // SAFETY: the address the entry points at, of its family's size.
            match unsafe { ip_of(entry.Address.lpSockaddr) } {
                Some(IpAddr::V4(v4)) => ipv4.push(subnet_of(v4, entry.OnLinkPrefixLength)),
                Some(IpAddr::V6(v6)) => {
                    // 2000::/3 is the global unicast range.
                    global_ipv6 |= (v6.segments()[0] & 0xe000) == 0x2000;
                }
                None => {}
            }
            unicast = entry.Next;
        }
        let mut has_gateway = false;
        let mut gateway = adapter.FirstGatewayAddress;
        while !gateway.is_null() {
            // SAFETY: as above.
            let entry = unsafe { &*gateway };
            // SAFETY: as above.
            if unsafe { ip_of(entry.Address.lpSockaddr) }.is_some() {
                has_gateway = true;
            }
            gateway = entry.Next;
        }
        let virtual_switch =
            name.starts_with("vEthernet") || description.contains("Hyper-V Virtual Ethernet");
        adapters.push(HostAdapter {
            up: adapter.OperStatus == IfOperStatusUp,
            name,
            description,
            has_gateway,
            ipv4,
            global_ipv6,
            virtual_switch,
        });
        at = adapter.Next;
    }
    Ok(adapters)
}

/// The addresses the host has seen the device with MAC address `mac` use: its neighbour
/// entries, IPv4 and IPv6.
pub fn neighbour_addresses(mac: &[u8; 6]) -> Vec<IpAddr> {
    let mut table: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
    // SAFETY: an out pointer; the table is freed below.
    if unsafe { GetIpNetTable2(AF_UNSPEC, &mut table) } != 0 || table.is_null() {
        return Vec::new();
    }
    let mut found = Vec::new();
    // SAFETY: the table GetIpNetTable2 returned, with NumEntries rows after its header.
    unsafe {
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        for row in rows {
            if row.PhysicalAddressLength as usize == 6 && row.PhysicalAddress[..6] == mac[..] {
                let address = &row.Address as *const _ as *const SOCKADDR;
                if let Some(ip) = ip_of(address) {
                    let link_local =
                        matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
                    if !link_local && !found.contains(&ip) {
                        found.push(ip);
                    }
                }
            }
        }
        FreeMibTable(table as *const c_void);
    }
    found
}

//! How adb on this computer finds a user's developer mode port, the Windows part: where the
//! user's adb server listens, and the port's mDNS advertisement through the DNS client
//! service (`DnsServiceRegister`).
//!
//! The service runs as SYSTEM, so its own environment says nothing of the user's adb. adb
//! reads `ANDROID_ADB_SERVER_PORT` from the environment of the user who runs it, which
//! Windows keeps in the user's registry hive (`HKEY_USERS\<SID>\Environment`, loaded while
//! they are signed in) and, for every user, under the machine's `Session Manager\Environment`.

use std::ffi::c_void;
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ,
};

use dsa_host_core::adb_discovery::{
    adb_server_port, mdns_instance_name, AdbAdvertise, AdbServerLink, ADB_SERVER_PORT_VARIABLE,
    MDNS_SERVICE_TYPE,
};

/// The machine's environment, which every user's programs start with.
const MACHINE_ENVIRONMENT: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// How long the DNS client service may take to say a registration is done.
const REGISTER_WAIT: Duration = Duration::from_secs(5);

/// `DNS_REQUEST_PENDING`: the registration goes on and its callback says how it ended.
const DNS_REQUEST_PENDING: u32 = 9506;

/// `DNS_QUERY_REQUEST_VERSION1`.
const DNS_QUERY_REQUEST_VERSION1: u32 = 1;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn text(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (path_w, name_w) = (wide(path), wide(name));
    let mut buffer = [0u16; 64];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: NUL-terminated names and a buffer of the size given.
    let code = unsafe {
        RegGetValueW(
            root,
            path_w.as_ptr(),
            name_w.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// The port the adb server of the user with `sid` listens on: theirs, else the machine's
/// `ANDROID_ADB_SERVER_PORT`, else 5037.
pub fn adb_server_port_of(sid: &str) -> u16 {
    let user = text(
        HKEY_USERS,
        &format!(r"{sid}\Environment"),
        ADB_SERVER_PORT_VARIABLE,
    );
    let value = user.or_else(|| {
        text(
            HKEY_LOCAL_MACHINE,
            MACHINE_ENVIRONMENT,
            ADB_SERVER_PORT_VARIABLE,
        )
    });
    adb_server_port(value.as_deref())
}

/// A link to the adb server of the user with `sid`, read again at each connection so a
/// changed `ANDROID_ADB_SERVER_PORT` is followed.
pub fn adb_server_of(sid: &str) -> AdbServerLink {
    let sid = sid.to_string();
    dsa_host_core::adb_discovery::loopback_link(move || adb_server_port_of(&sid))
}

type RegisterComplete =
    unsafe extern "system" fn(status: u32, context: *mut c_void, instance: *mut c_void);

/// `DNS_SERVICE_REGISTER_REQUEST`.
#[repr(C)]
struct RegisterRequest {
    version: u32,
    interface_index: u32,
    service_instance: *mut c_void,
    callback: Option<RegisterComplete>,
    context: *mut c_void,
    credentials: *mut c_void,
    unicast_enabled: i32,
}

#[link(name = "dnsapi", kind = "raw-dylib")]
extern "system" {
    fn DnsServiceConstructInstance(
        service_name: *const u16,
        host_name: *const u16,
        ip4: *const u32,
        ip6: *const c_void,
        port: u16,
        priority: u16,
        weight: u16,
        properties: u32,
        keys: *const *const u16,
        values: *const *const u16,
    ) -> *mut c_void;
    fn DnsServiceFreeInstance(instance: *mut c_void);
    fn DnsServiceRegister(request: *const RegisterRequest, cancel: *mut c_void) -> u32;
    fn DnsServiceDeRegister(request: *const RegisterRequest, cancel: *mut c_void) -> u32;
}

/// Called by the DNS client service when a registration or its end is done.
unsafe extern "system" fn completed(status: u32, context: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: the service hands over an instance the callee frees.
        unsafe { DnsServiceFreeInstance(instance) };
    }
    if !context.is_null() {
        // SAFETY: the context is the sender the registration boxed, alive until it is freed
        // after the callback has run.
        let done = unsafe { &*(context as *const SyncSender<u32>) };
        let _ = done.try_send(status);
    }
}

/// A registered `_adb._tcp` service, ended when dropped.
struct Registration {
    request: Box<RegisterRequest>,
    instance: *mut c_void,
    done: Box<SyncSender<u32>>,
    answers: mpsc::Receiver<u32>,
    port: u16,
}

// SAFETY: the pointers are the DNS client service's, used from one thread at a time: here
// when the registration ends, and in its callback.
unsafe impl Send for Registration {}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: the request registered this instance and is still alive.
        let code = unsafe { DnsServiceDeRegister(&*self.request, std::ptr::null_mut()) };
        let ended = code == DNS_REQUEST_PENDING && self.answers.recv_timeout(REGISTER_WAIT).is_ok();
        if ended || code != DNS_REQUEST_PENDING {
            // SAFETY: constructed by DnsServiceConstructInstance and no longer in use.
            unsafe { DnsServiceFreeInstance(self.instance) };
        } else {
            // The service may still call back with these, so they are left to it.
            eprintln!(
                "developer mode: the mDNS advertisement of port {} did not end in time",
                self.port
            );
            std::mem::forget(std::mem::replace(
                &mut self.done,
                Box::new(mpsc::sync_channel(1).0),
            ));
            let request = std::mem::replace(
                &mut self.request,
                Box::new(RegisterRequest {
                    version: 0,
                    interface_index: 0,
                    service_instance: std::ptr::null_mut(),
                    callback: None,
                    context: std::ptr::null_mut(),
                    credentials: std::ptr::null_mut(),
                    unicast_enabled: 0,
                }),
            );
            std::mem::forget(request);
        }
    }
}

/// Advertises developer mode's port as an `_adb._tcp` service whose only address is
/// `127.0.0.1`, so no other device is told an address it could reach.
pub fn advertise(port: u16) -> Option<Box<dyn Send>> {
    let service = wide(&format!(
        "{}.{MDNS_SERVICE_TYPE}.local",
        mdns_instance_name(port)
    ));
    let host = wide(&format!("dsa-adb-{port}.local"));
    // An IP4_ADDRESS is the address in network byte order.
    let loopback = u32::from_ne_bytes([127, 0, 0, 1]);
    // SAFETY: NUL-terminated names and an address that live through the call, which copies
    // them; no properties.
    let instance = unsafe {
        DnsServiceConstructInstance(
            service.as_ptr(),
            host.as_ptr(),
            &loopback,
            std::ptr::null(),
            port,
            0,
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if instance.is_null() {
        eprintln!("developer mode: Windows cannot describe the mDNS service of port {port}");
        return None;
    }
    let (sender, answers) = mpsc::sync_channel(2);
    let done = Box::new(sender);
    let request = Box::new(RegisterRequest {
        version: DNS_QUERY_REQUEST_VERSION1,
        // Every interface the DNS client service answers on; the address it gives is
        // 127.0.0.1 alone.
        interface_index: 0,
        service_instance: instance,
        callback: Some(completed),
        context: (&*done as *const SyncSender<u32>) as *mut c_void,
        credentials: std::ptr::null_mut(),
        unicast_enabled: 0,
    });
    // SAFETY: the request, its instance and its context stay alive in the registration.
    let code = unsafe { DnsServiceRegister(&*request, std::ptr::null_mut()) };
    if code != DNS_REQUEST_PENDING {
        // SAFETY: constructed above and not registered.
        unsafe { DnsServiceFreeInstance(instance) };
        eprintln!(
            "developer mode: Windows does not advertise port {port} over mDNS ({})",
            std::io::Error::from_raw_os_error(code as i32)
        );
        return None;
    }
    let registration = Registration {
        request,
        instance,
        done,
        answers,
        port,
    };
    match registration.answers.recv_timeout(REGISTER_WAIT) {
        Ok(0) => Some(Box::new(registration)),
        Ok(status) => {
            eprintln!(
                "developer mode: Windows refused to advertise port {port} over mDNS ({})",
                std::io::Error::from_raw_os_error(status as i32)
            );
            None
        }
        Err(_) => {
            eprintln!(
                "developer mode: Windows did not confirm the mDNS advertisement of port {port}; \
                 it may still appear"
            );
            Some(Box::new(registration))
        }
    }
}

/// The advertiser developer mode uses on Windows.
pub fn advertiser() -> AdbAdvertise {
    std::sync::Arc::new(advertise)
}

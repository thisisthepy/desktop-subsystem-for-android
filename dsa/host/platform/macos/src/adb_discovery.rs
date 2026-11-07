//! How adb on this computer finds the user's developer mode port, the macOS part: the user's
//! adb server, and the port's mDNS advertisement through `dns_sd` (mDNSResponder).
//!
//! The service is the user's own launchd agent, so `ANDROID_ADB_SERVER_PORT` in its
//! environment is the user's. The advertisement is local to this computer
//! (`kDNSServiceInterfaceIndexLocalOnly`) and its host record holds `127.0.0.1` alone, so no
//! other device sees it or is given an address it could reach.

use std::ffi::{c_char, c_void, CString};
use std::sync::Arc;

use dsa_host_core::adb_discovery::{
    adb_server_port, mdns_instance_name, AdbAdvertise, AdbServerLink, ADB_SERVER_PORT_VARIABLE,
    MDNS_SERVICE_TYPE,
};

type DnsServiceRef = *mut c_void;
type DnsRecordRef = *mut c_void;
type RegisterRecordReply = unsafe extern "C" fn(
    sd_ref: DnsServiceRef,
    record_ref: DnsRecordRef,
    flags: u32,
    error: i32,
    context: *mut c_void,
);

/// `kDNSServiceFlagsUnique`: the host record is this computer's alone.
const FLAGS_UNIQUE: u32 = 0x20;
/// `kDNSServiceFlagsShareConnection`: the service rides on the connection of the record.
const FLAGS_SHARE_CONNECTION: u32 = 0x4000;
/// `kDNSServiceInterfaceIndexLocalOnly`: seen by programs on this computer only.
const INTERFACE_LOCAL_ONLY: u32 = u32::MAX;
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
const TTL: u32 = 120;

// dns_sd is part of libSystem, which every program links.
extern "C" {
    fn DNSServiceCreateConnection(sd_ref: *mut DnsServiceRef) -> i32;
    fn DNSServiceRegisterRecord(
        sd_ref: DnsServiceRef,
        record_ref: *mut DnsRecordRef,
        flags: u32,
        interface_index: u32,
        fullname: *const c_char,
        rrtype: u16,
        rrclass: u16,
        rdlen: u16,
        rdata: *const c_void,
        ttl: u32,
        callback: Option<RegisterRecordReply>,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRegister(
        sd_ref: *mut DnsServiceRef,
        flags: u32,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt_record: *const c_void,
        callback: *const c_void,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefDeallocate(sd_ref: DnsServiceRef);
}

unsafe extern "C" fn record_registered(
    _sd_ref: DnsServiceRef,
    _record_ref: DnsRecordRef,
    _flags: u32,
    _error: i32,
    _context: *mut c_void,
) {
}

/// The port of the user's adb server, from this agent's environment.
pub fn adb_server_port_now() -> u16 {
    adb_server_port(std::env::var(ADB_SERVER_PORT_VARIABLE).ok().as_deref())
}

/// A link to the user's adb server.
pub fn adb_server() -> AdbServerLink {
    dsa_host_core::adb_discovery::loopback_link(adb_server_port_now)
}

/// An advertisement, ended when dropped.
struct Advertised {
    connection: DnsServiceRef,
    service: DnsServiceRef,
}

// SAFETY: the references are used once more, to end them, from whichever thread drops this.
unsafe impl Send for Advertised {}

impl Drop for Advertised {
    fn drop(&mut self) {
        // SAFETY: both were made by dns_sd and are ended once, the service first.
        unsafe {
            DNSServiceRefDeallocate(self.service);
            DNSServiceRefDeallocate(self.connection);
        }
    }
}

/// Advertises developer mode's port as an `_adb._tcp` service on `127.0.0.1`, to programs on
/// this computer only.
pub fn advertise(port: u16) -> Option<Box<dyn Send>> {
    let host = format!("dsa-adb-{port}.local.");
    let (Ok(host_c), Ok(name), Ok(regtype)) = (
        CString::new(host),
        CString::new(mdns_instance_name(port)),
        CString::new(MDNS_SERVICE_TYPE),
    ) else {
        return None;
    };
    let mut connection: DnsServiceRef = std::ptr::null_mut();
    // SAFETY: an out pointer for the new connection.
    let code = unsafe { DNSServiceCreateConnection(&mut connection) };
    if code != 0 {
        eprintln!("developer mode: mDNSResponder cannot be reached ({code}); port {port} is not advertised");
        return None;
    }
    let address = [127u8, 0, 0, 1];
    let mut record: DnsRecordRef = std::ptr::null_mut();
    // SAFETY: a live connection, a NUL-terminated name and four bytes of address, which the
    // call copies.
    let code = unsafe {
        DNSServiceRegisterRecord(
            connection,
            &mut record,
            FLAGS_UNIQUE,
            INTERFACE_LOCAL_ONLY,
            host_c.as_ptr(),
            TYPE_A,
            CLASS_IN,
            address.len() as u16,
            address.as_ptr().cast(),
            TTL,
            Some(record_registered),
            std::ptr::null_mut(),
        )
    };
    if code != 0 {
        // SAFETY: made above, ended once.
        unsafe { DNSServiceRefDeallocate(connection) };
        eprintln!("developer mode: mDNSResponder refuses the loopback host record ({code})");
        return None;
    }
    let mut service: DnsServiceRef = connection;
    // SAFETY: `service` starts as the shared connection, as ShareConnection requires; the
    // strings live through the call, which copies them. No callback: nothing is read back.
    let code = unsafe {
        DNSServiceRegister(
            &mut service,
            FLAGS_SHARE_CONNECTION,
            INTERFACE_LOCAL_ONLY,
            name.as_ptr(),
            regtype.as_ptr(),
            std::ptr::null(),
            host_c.as_ptr(),
            port.to_be(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if code != 0 {
        // SAFETY: made above, ended once.
        unsafe { DNSServiceRefDeallocate(connection) };
        eprintln!("developer mode: mDNSResponder refuses the service of port {port} ({code})");
        return None;
    }
    Some(Box::new(Advertised {
        connection,
        service,
    }))
}

/// The advertiser developer mode uses on macOS.
pub fn advertiser() -> AdbAdvertise {
    Arc::new(advertise)
}

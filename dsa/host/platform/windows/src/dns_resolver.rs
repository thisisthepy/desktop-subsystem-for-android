//! The host's resolver on Windows, which answers the guest's DNS queries
//! ([`dsa_host_core::dns`]).
//!
//! Windows' DNS client answers them, as it does every program on the PC, so the hosts file,
//! the name resolution policy table (the NRPT, where a company or a VPN routes its own
//! domains), a VPN's split DNS, encrypted DNS and a DNS filter apply to Android's apps too.
//!
//! - `DnsQueryRaw` (Windows 11 22H2 and later) takes the program's own query and gives back the
//!   whole response, as WSL's DNS tunneling uses it: TTLs, `NXDOMAIN`, `CNAME` chains and
//!   every record type as the DNS client has them. It is looked up at run time, so a Windows
//!   without it still starts.
//! - Where it is missing, `DnsQueryEx` answers with records, which become the wire answer: `A`,
//!   `AAAA`, `CNAME`, `PTR`, `NS`, `MX`, `SRV`, `TXT` and `SOA`. Its status says `NXDOMAIN` or
//!   an empty answer; any other failure is `SERVFAIL`.

use std::ffi::c_void;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dsa_host_core::dns::wire::{
    self, Name, Question, Rcode, Rdata, Record, CLASS_IN, TYPE_A, TYPE_AAAA, TYPE_CNAME,
    TYPE_DNAME, TYPE_MX, TYPE_NS, TYPE_PTR, TYPE_SOA, TYPE_SRV, TYPE_TXT,
};
use dsa_host_core::dns::{HostResolver, Resolution};
use windows_sys::Win32::NetworkManagement::Dns::{
    DnsFree, DnsFreeRecordList, DnsQueryEx, DNS_PROTOCOL_TCP, DNS_PROTOCOL_UDP, DNS_QUERY_CANCEL,
    DNS_QUERY_RAW_CANCEL, DNS_QUERY_RAW_REQUEST, DNS_QUERY_RAW_REQUEST_VERSION1,
    DNS_QUERY_RAW_RESULT, DNS_QUERY_RAW_RESULTS_VERSION1, DNS_QUERY_REQUEST,
    DNS_QUERY_REQUEST_VERSION1, DNS_QUERY_RESULT, DNS_QUERY_STANDARD, DNS_RECORDW,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// `DNS_REQUEST_PENDING`: the query goes on and its callback gives the result.
const DNS_REQUEST_PENDING: i32 = 9506;
/// `DNS_ERROR_RCODE_NAME_ERROR`: the name does not exist.
const DNS_ERROR_RCODE_NAME_ERROR: i32 = 9003;
/// `DNS_INFO_NO_RECORDS`: the name has no records of the type asked.
const DNS_INFO_NO_RECORDS: i32 = 9501;

/// How long a raw query may take before it is cancelled. The DNS service has answered the
/// program `SERVFAIL` long before; an answer that comes in this time still fills its cache.
const RAW_QUERY_LIMIT: Duration = Duration::from_secs(15);

/// The sections of `DnsQueryEx`'s records (the low bits of their flags).
const SECTION_ANSWER: u32 = 1;
const SECTION_AUTHORITY: u32 = 2;

type DnsQueryRawFn =
    unsafe extern "system" fn(*const DNS_QUERY_RAW_REQUEST, *mut DNS_QUERY_RAW_CANCEL) -> i32;
type DnsCancelQueryRawFn = unsafe extern "system" fn(*const DNS_QUERY_RAW_CANCEL) -> i32;
/// What GetProcAddress gives.
type Farproc = unsafe extern "system" fn() -> isize;
type DnsQueryRawResultFreeFn = unsafe extern "system" fn(*const DNS_QUERY_RAW_RESULT);

/// `DnsQueryRaw` and its two companions, where this Windows has them.
#[derive(Clone, Copy)]
struct RawApi {
    query: DnsQueryRawFn,
    cancel: DnsCancelQueryRawFn,
    free: DnsQueryRawResultFreeFn,
}

fn raw_api() -> Option<RawApi> {
    static API: OnceLock<Option<RawApi>> = OnceLock::new();
    *API.get_or_init(|| {
        let name: Vec<u16> = "dnsapi.dll".encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated name; dnsapi.dll stays loaded for the process's life.
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            return None;
        }
        // SAFETY: NUL-terminated names of functions whose signatures windns.h gives, which the
        // types above repeat.
        unsafe {
            let query = GetProcAddress(module, c"DnsQueryRaw".as_ptr().cast())?;
            let cancel = GetProcAddress(module, c"DnsCancelQueryRaw".as_ptr().cast())?;
            let free = GetProcAddress(module, c"DnsQueryRawResultFree".as_ptr().cast())?;
            Some(RawApi {
                query: std::mem::transmute::<Farproc, DnsQueryRawFn>(query),
                cancel: std::mem::transmute::<Farproc, DnsCancelQueryRawFn>(cancel),
                free: std::mem::transmute::<Farproc, DnsQueryRawResultFreeFn>(free),
            })
        }
    })
}

/// What a raw query's callback hands back.
enum RawOutcome {
    Response(Vec<u8>),
    Status(i32),
}

/// The callback's context: where to send the outcome, and the function that frees the result.
struct RawContext {
    done: Mutex<Option<mpsc::Sender<RawOutcome>>>,
    free: DnsQueryRawResultFreeFn,
}

unsafe extern "system" fn raw_completed(
    context: *const c_void,
    results: *const DNS_QUERY_RAW_RESULT,
) {
    // SAFETY: the context is the Arc DnsQueryRaw was handed, given back once with its result.
    let context = unsafe { Arc::from_raw(context as *const RawContext) };
    let outcome = if results.is_null() {
        RawOutcome::Status(-1)
    } else {
        // SAFETY: the DNS client's result, valid until it is freed below.
        let result = unsafe { &*results };
        if !result.queryRawResponse.is_null() && result.queryRawResponseSize > 0 {
            // SAFETY: the response's bytes, of the size the result gives.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    result.queryRawResponse,
                    result.queryRawResponseSize as usize,
                )
            };
            RawOutcome::Response(bytes.to_vec())
        } else {
            RawOutcome::Status(result.queryStatus)
        }
    };
    if !results.is_null() {
        // SAFETY: the result DnsQueryRaw gave this callback, freed once.
        unsafe { (context.free)(results) };
    }
    let done = context
        .done
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(done) = done {
        let _ = done.send(outcome);
    }
}

/// Asks with `DnsQueryRaw`. `None` when this Windows has no `DnsQueryRaw` or refused the query
/// before asking, so the caller asks with `DnsQueryEx`.
fn query_raw(api: RawApi, query: &[u8], tcp: bool) -> Option<Resolution> {
    let (done, wait) = mpsc::channel();
    let context = Arc::new(RawContext {
        done: Mutex::new(Some(done)),
        free: api.free,
    });
    let mut bytes = query.to_vec();
    let request = DNS_QUERY_RAW_REQUEST {
        version: DNS_QUERY_RAW_REQUEST_VERSION1,
        resultsVersion: DNS_QUERY_RAW_RESULTS_VERSION1,
        dnsQueryRawSize: bytes.len() as u32,
        dnsQueryRaw: bytes.as_mut_ptr(),
        queryOptions: DNS_QUERY_STANDARD as u64,
        queryCompletionCallback: Some(raw_completed),
        queryContext: Arc::into_raw(Arc::clone(&context)) as *mut c_void,
        protocol: if tcp {
            DNS_PROTOCOL_TCP
        } else {
            DNS_PROTOCOL_UDP
        },
        ..Default::default()
    };
    let mut cancel = DNS_QUERY_RAW_CANCEL::default();
    // SAFETY: the request and its query bytes live until the callback has run or the query is
    // cancelled below; the context's reference is the callback's to release.
    let status = unsafe { (api.query)(&request, &mut cancel) };
    if status != DNS_REQUEST_PENDING {
        // No callback comes; the context's reference is left, a few bytes, rather than risk
        // releasing it twice should one come after all.
        return None;
    }
    let outcome = match wait.recv_timeout(RAW_QUERY_LIMIT) {
        Ok(outcome) => outcome,
        Err(_) => {
            // SAFETY: the handle DnsQueryRaw filled for this query, which is still pending.
            unsafe { (api.cancel)(&cancel) };
            // The callback still runs once, with the cancellation, and the request's bytes must
            // outlive it.
            let _ = wait.recv_timeout(Duration::from_secs(5));
            // Should it not come, the DNS client may still hold the query's bytes.
            std::mem::forget(bytes);
            return Some(Resolution::Failed(
                "Windows' DNS client did not answer in time".into(),
            ));
        }
    };
    drop(bytes);
    Some(match outcome {
        RawOutcome::Response(response) => Resolution::Message(response),
        RawOutcome::Status(status) => status_resolution(status),
    })
}

/// What a `DnsQueryEx` or `DnsQueryRaw` failure status says.
fn status_resolution(status: i32) -> Resolution {
    match status {
        DNS_ERROR_RCODE_NAME_ERROR => Resolution::nxdomain(),
        DNS_INFO_NO_RECORDS => Resolution::no_data(),
        other => Resolution::Failed(format!(
            "Windows' DNS client failed ({})",
            std::io::Error::from_raw_os_error(other)
        )),
    }
}

/// A wide string from Windows, as text.
///
/// # Safety
/// `text` is null or a NUL-terminated UTF-16 string.
unsafe fn wide_text(text: *const u16) -> Option<String> {
    if text.is_null() {
        return None;
    }
    let mut len = 0;
    // SAFETY: the string ends with a NUL, as the caller promises.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` characters before the NUL.
    Some(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(text, len)
    }))
}

/// A name from a wide string of Windows'.
///
/// # Safety
/// As [`wide_text`].
unsafe fn wide_name(text: *const u16) -> Option<Name> {
    // SAFETY: as the caller promises.
    Name::from_dotted(&unsafe { wide_text(text) }?)
}

/// One of `DnsQueryEx`'s records as a wire record; `None` for a type it is not built for.
///
/// # Safety
/// `record` is a record of the list `DnsQueryEx` gave, not yet freed.
unsafe fn wire_record(record: &DNS_RECORDW) -> Option<Record> {
    // SAFETY: the record's own name and data, valid while the list is.
    unsafe {
        let name = wide_name(record.pName)?;
        let data = match record.wType {
            TYPE_A => Rdata::Raw(
                Ipv4Addr::from(record.Data.A.IpAddress.to_ne_bytes())
                    .octets()
                    .to_vec(),
            ),
            TYPE_AAAA => Rdata::Raw(
                Ipv6Addr::from(record.Data.AAAA.Ip6Address.IP6Byte)
                    .octets()
                    .to_vec(),
            ),
            TYPE_CNAME | TYPE_PTR | TYPE_NS | TYPE_DNAME => {
                Rdata::Name(wide_name(record.Data.PTR.pNameHost)?)
            }
            TYPE_MX => Rdata::Mx {
                preference: record.Data.MX.wPreference,
                exchange: wide_name(record.Data.MX.pNameExchange)?,
            },
            TYPE_SRV => Rdata::Srv {
                priority: record.Data.SRV.wPriority,
                weight: record.Data.SRV.wWeight,
                port: record.Data.SRV.wPort,
                target: wide_name(record.Data.SRV.pNameTarget)?,
            },
            TYPE_SOA => {
                let soa = record.Data.SOA;
                Rdata::Soa {
                    mname: wide_name(soa.pNamePrimaryServer)?,
                    rname: wide_name(soa.pNameAdministrator)?,
                    serial: soa.dwSerialNo,
                    refresh: soa.dwRefresh,
                    retry: soa.dwRetry,
                    expire: soa.dwExpire,
                    minimum: soa.dwDefaultTtl,
                }
            }
            TYPE_TXT => {
                let txt = &record.Data.TXT;
                // The strings follow the first in the record's own allocation.
                let strings = std::slice::from_raw_parts(
                    txt.pStringArray.as_ptr(),
                    txt.dwStringCount as usize,
                );
                let mut data = Vec::new();
                for &string in strings {
                    let text = wide_text(string).unwrap_or_default();
                    let bytes = text.as_bytes();
                    for chunk in bytes.chunks(255) {
                        data.push(chunk.len() as u8);
                        data.extend_from_slice(chunk);
                    }
                    if bytes.is_empty() {
                        data.push(0);
                    }
                }
                Rdata::Raw(data)
            }
            _ => return None,
        };
        Some(Record {
            name,
            rtype: record.wType,
            class: CLASS_IN,
            ttl: record.dwTtl,
            data,
        })
    }
}

/// Asks with `DnsQueryEx`, synchronously; the DNS service's timeout bounds how long the
/// program waits.
fn query_ex(question: &Question) -> Resolution {
    let text = question.name.to_dotted();
    if text.contains('\\') {
        return Resolution::Failed(format!(
            "{text} has bytes Windows' DNS client cannot be asked about by name"
        ));
    }
    let name: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let request = DNS_QUERY_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1,
        QueryName: name.as_ptr(),
        QueryType: question.qtype,
        QueryOptions: DNS_QUERY_STANDARD as u64,
        ..Default::default()
    };
    let mut result = DNS_QUERY_RESULT {
        Version: DNS_QUERY_REQUEST_VERSION1,
        ..Default::default()
    };
    // SAFETY: a request without a callback answers before it returns; the name outlives it.
    let status = unsafe {
        DnsQueryEx(
            &request,
            &mut result,
            std::ptr::null_mut::<DNS_QUERY_CANCEL>(),
        )
    };
    let records = result.pQueryRecords as *mut DNS_RECORDW;
    let mut answers = Vec::new();
    let mut authority = Vec::new();
    let mut at = records;
    while !at.is_null() {
        // SAFETY: a record of the list DnsQueryEx gave, freed only below.
        let record = unsafe { &*at };
        // SAFETY: as above; the flags' low two bits are the record's section.
        let section = unsafe { record.Flags.DW } & 0x3;
        // SAFETY: as above.
        if let Some(wire) = unsafe { wire_record(record) } {
            match section {
                SECTION_ANSWER => answers.push(wire),
                SECTION_AUTHORITY => authority.push(wire),
                _ => {}
            }
        }
        at = record.pNext;
    }
    if !records.is_null() {
        // SAFETY: the list DnsQueryEx gave, freed once.
        unsafe { DnsFree(records as *const c_void, DnsFreeRecordList) };
    }
    match status {
        0 => Resolution::Records {
            rcode: Rcode::NoError,
            answers,
            authority,
        },
        DNS_ERROR_RCODE_NAME_ERROR => Resolution::Records {
            rcode: Rcode::NxDomain,
            answers: Vec::new(),
            authority,
        },
        other => status_resolution(other),
    }
}

/// Windows' DNS client.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsResolver;

impl HostResolver for WindowsResolver {
    fn resolve(&self, question: &Question, query: &[u8]) -> Resolution {
        if let Some(api) = raw_api() {
            if let Some(resolution) = query_raw(api, query, false) {
                // An answer too large for UDP is asked again over TCP, so a program that asks
                // over TCP gets all of it.
                if let Resolution::Message(bytes) = &resolution {
                    if wire::parse(bytes).is_ok_and(|m| m.truncated()) {
                        if let Some(whole) = query_raw(api, query, true) {
                            return whole;
                        }
                    }
                }
                return resolution;
            }
        }
        query_ex(question)
    }
}

//! The host's resolver on Linux and macOS, which answers the guest's DNS queries
//! ([`dsa_host_core::dns`]).
//!
//! An address question (`A`, `AAAA`) is answered by `getaddrinfo`, the call every program on
//! the computer resolves names with: on Linux through the name service switch (the hosts file,
//! systemd-resolved with its per-link DNS, which is how a VPN's split DNS and DNS-over-TLS
//! reach it), on macOS through mDNSResponder (the hosts file, the scoped resolvers a VPN or a
//! configuration profile adds, encrypted DNS). It decides whether the name exists and which
//! addresses it has. `res_query` asks the same question on the wire; when its answer holds the
//! same addresses, that answer goes to the guest, with its TTLs and `CNAME` chain. Otherwise
//! (a hosts file entry, a name only a scoped resolver knows) the answer is built from what
//! `getaddrinfo` gave: its canonical name as a `CNAME`, and the addresses with a short TTL, so
//! a change on the host reaches the guest soon.
//!
//! Every other question (`MX`, `TXT`, `SRV`, `PTR`, ...) goes to `res_query`, which answers
//! from the system's DNS servers, retrying over TCP when a UDP answer comes truncated.
//!
//! `res_query` is found at run time (in libc on glibc 2.34 and later, in libresolv before and
//! on macOS), so the service needs no link to libresolv; without it, address questions are
//! still answered.

use std::collections::BTreeSet;
use std::ffi::{c_char, c_int, CStr, CString};
use std::net::IpAddr;
use std::sync::OnceLock;

use dsa_host_core::dns::wire::{
    self, Name, Question, Rcode, Record, CLASS_IN, TYPE_A, TYPE_AAAA, TYPE_CNAME,
};
use dsa_host_core::dns::{HostResolver, Resolution};

/// The TTL of records built from `getaddrinfo`, which gives none.
pub const ADDRESS_TTL: u32 = 30;

type ResQuery = unsafe extern "C" fn(*const c_char, c_int, c_int, *mut u8, c_int) -> c_int;

fn res_query_fn() -> Option<ResQuery> {
    static FOUND: OnceLock<Option<usize>> = OnceLock::new();
    let address = *FOUND.get_or_init(|| {
        let names: [&CStr; 3] = [c"res_query", c"__res_query", c"res_9_query"];
        let libraries: [&CStr; 3] = [
            c"libresolv.so.2",
            c"libresolv.9.dylib",
            c"/usr/lib/libresolv.9.dylib",
        ];
        for name in names {
            // SAFETY: looks a symbol up among the libraries already loaded.
            let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            if !symbol.is_null() {
                return Some(symbol as usize);
            }
        }
        for library in libraries {
            // SAFETY: loads a system library, which stays loaded for the process's life.
            let handle = unsafe { libc::dlopen(library.as_ptr(), libc::RTLD_NOW) };
            if handle.is_null() {
                continue;
            }
            for name in names {
                // SAFETY: a symbol of the library just loaded.
                let symbol = unsafe { libc::dlsym(handle, name.as_ptr()) };
                if !symbol.is_null() {
                    return Some(symbol as usize);
                }
            }
        }
        None
    });
    // SAFETY: the address of res_query, whose signature resolv.h gives and ResQuery repeats.
    address.map(|address| unsafe { std::mem::transmute::<usize, ResQuery>(address) })
}

/// macOS's `res_query` keeps its state in one global, so its calls take turns there.
#[cfg(target_os = "macos")]
static RES_QUERY_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Asks `res_query`; the response, or `None` when it gave none.
fn res_query(name: &CStr, qtype: u16) -> Option<Vec<u8>> {
    let query = res_query_fn()?;
    let mut answer = vec![0u8; wire::MAX_MESSAGE_LEN];
    #[cfg(target_os = "macos")]
    let _turn = RES_QUERY_TURN.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: a NUL-terminated name and a buffer of the length given.
    let len = unsafe {
        query(
            name.as_ptr(),
            CLASS_IN as c_int,
            qtype as c_int,
            answer.as_mut_ptr(),
            answer.len() as c_int,
        )
    };
    if len > 0 {
        answer.truncate((len as usize).min(answer.len()));
        return Some(answer);
    }
    // On NXDOMAIN or an empty answer res_query fails, but the response is in the buffer.
    let response = wire::parse(&answer).ok()?;
    (response.is_response() && !response.questions.is_empty()).then(|| wire::encode(&response))
}

/// What `getaddrinfo` said about a name in one family.
enum Lookup {
    Addresses {
        addresses: BTreeSet<IpAddr>,
        canonical: Option<String>,
    },
    /// The name does not exist.
    NoName,
    /// The name exists without addresses of the family.
    NoData,
    Failed(String),
}

/// glibc's code for a name that has addresses, but none of the family asked for. The libc
/// crate does not export it, so its value comes from glibc's `netdb.h`.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const EAI_ADDRFAMILY: c_int = -9;

fn lookup(name: &CStr, family: c_int) -> Lookup {
    // SAFETY: an all-zero addrinfo is a valid set of hints.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = family;
    hints.ai_socktype = libc::SOCK_DGRAM;
    hints.ai_flags = libc::AI_CANONNAME;
    let mut list: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name, the hints above and a pointer the call fills.
    let code = unsafe { libc::getaddrinfo(name.as_ptr(), std::ptr::null(), &hints, &mut list) };
    if code != 0 {
        return match code {
            libc::EAI_NONAME => Lookup::NoName,
            #[cfg(target_os = "linux")]
            libc::EAI_NODATA => Lookup::NoData,
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            EAI_ADDRFAMILY => Lookup::NoData,
            other => {
                // SAFETY: gai_strerror gives a static string for any code.
                let why = unsafe { CStr::from_ptr(libc::gai_strerror(other)) };
                Lookup::Failed(why.to_string_lossy().into_owned())
            }
        };
    }
    let mut addresses = BTreeSet::new();
    let mut canonical = None;
    let mut at = list;
    while !at.is_null() {
        // SAFETY: an entry of the list getaddrinfo gave, freed only below.
        let entry = unsafe { &*at };
        if canonical.is_none() && !entry.ai_canonname.is_null() {
            // SAFETY: getaddrinfo's NUL-terminated canonical name.
            canonical = Some(
                unsafe { CStr::from_ptr(entry.ai_canonname) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if let Some(address) = address_of(entry) {
            addresses.insert(address);
        }
        at = entry.ai_next;
    }
    // SAFETY: the list getaddrinfo gave, freed once.
    unsafe { libc::freeaddrinfo(list) };
    if addresses.is_empty() {
        return Lookup::NoData;
    }
    Lookup::Addresses {
        addresses,
        canonical,
    }
}

fn address_of(entry: &libc::addrinfo) -> Option<IpAddr> {
    if entry.ai_addr.is_null() {
        return None;
    }
    match entry.ai_family {
        libc::AF_INET => {
            // SAFETY: an AF_INET entry's address is a sockaddr_in.
            let address = unsafe { &*(entry.ai_addr as *const libc::sockaddr_in) };
            Some(IpAddr::from(address.sin_addr.s_addr.to_ne_bytes()))
        }
        libc::AF_INET6 => {
            // SAFETY: an AF_INET6 entry's address is a sockaddr_in6.
            let address = unsafe { &*(entry.ai_addr as *const libc::sockaddr_in6) };
            Some(IpAddr::from(address.sin6_addr.s6_addr))
        }
        _ => None,
    }
}

/// The addresses of a `res_query` answer of type `qtype`.
fn wire_addresses(response: &wire::Message, qtype: u16) -> BTreeSet<IpAddr> {
    response
        .answers
        .iter()
        .filter(|r| r.rtype == qtype)
        .filter_map(|r| match &r.data {
            wire::Rdata::Raw(bytes) if bytes.len() == 4 => {
                Some(IpAddr::from(<[u8; 4]>::try_from(bytes.as_slice()).ok()?))
            }
            wire::Rdata::Raw(bytes) if bytes.len() == 16 => {
                Some(IpAddr::from(<[u8; 16]>::try_from(bytes.as_slice()).ok()?))
            }
            _ => None,
        })
        .collect()
}

/// The answer built from `getaddrinfo`'s addresses for `question`.
fn built_answer(
    question: &Question,
    addresses: &BTreeSet<IpAddr>,
    canonical: Option<&str>,
) -> Resolution {
    let mut answers = Vec::new();
    let mut owner = question.name.clone();
    if let Some(canonical) = canonical.and_then(Name::from_dotted) {
        if !canonical.same(&question.name) {
            answers.push(Record::with_name(
                &question.name,
                TYPE_CNAME,
                ADDRESS_TTL,
                canonical.clone(),
            ));
            owner = canonical;
        }
    }
    for address in addresses {
        answers.push(match address {
            IpAddr::V4(v4) => Record::a(&owner, ADDRESS_TTL, *v4),
            IpAddr::V6(v6) => Record::aaaa(&owner, ADDRESS_TTL, *v6),
        });
    }
    Resolution::answers(answers)
}

/// The text of `name` getaddrinfo and res_query take; `None` for a name with a label they
/// cannot be given (a dot or a NUL inside it).
fn plain_text(name: &Name) -> Option<CString> {
    let labels = name.labels();
    if labels.iter().any(|l| l.contains(&b'.') || l.contains(&0)) {
        return None;
    }
    let text: Vec<u8> = labels.join(&b'.');
    CString::new(if text.is_empty() { b".".to_vec() } else { text }).ok()
}

/// The system's resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

impl SystemResolver {
    fn address(&self, question: &Question, name: &CStr) -> Resolution {
        let (family, other) = if question.qtype == TYPE_A {
            (libc::AF_INET, libc::AF_INET6)
        } else {
            (libc::AF_INET6, libc::AF_INET)
        };
        let wire_answer = res_query(name, question.qtype);
        let parsed = wire_answer
            .as_deref()
            .and_then(|bytes| wire::parse(bytes).ok());
        match lookup(name, family) {
            Lookup::Addresses {
                addresses,
                canonical,
            } => match (&wire_answer, &parsed) {
                (Some(bytes), Some(response))
                    if response.rcode == Rcode::NoError
                        && wire_addresses(response, question.qtype) == addresses =>
                {
                    Resolution::Message(bytes.clone())
                }
                _ => built_answer(question, &addresses, canonical.as_deref()),
            },
            // macOS says "no name" for a name without addresses of the family too.
            Lookup::NoName | Lookup::NoData => {
                let exists = matches!(lookup(name, other), Lookup::Addresses { .. });
                let rcode = if exists {
                    Rcode::NoError
                } else {
                    Rcode::NxDomain
                };
                match (&wire_answer, &parsed) {
                    // The wire answer agrees, and carries the SOA's negative TTL.
                    (Some(bytes), Some(response))
                        if response.rcode == rcode
                            && wire_addresses(response, question.qtype).is_empty() =>
                    {
                        Resolution::Message(bytes.clone())
                    }
                    _ if exists => Resolution::no_data(),
                    _ => Resolution::nxdomain(),
                }
            }
            Lookup::Failed(why) => Resolution::Failed(format!("getaddrinfo failed: {why}")),
        }
    }
}

impl HostResolver for SystemResolver {
    fn resolve(&self, question: &Question, _query: &[u8]) -> Resolution {
        let Some(name) = plain_text(&question.name) else {
            return Resolution::nxdomain();
        };
        if question.qclass == CLASS_IN && (question.qtype == TYPE_A || question.qtype == TYPE_AAAA)
        {
            return self.address(question, &name);
        }
        if res_query_fn().is_none() {
            return Resolution::Failed("this system has no res_query".into());
        }
        match res_query(&name, question.qtype) {
            Some(bytes) => Resolution::Message(bytes),
            None => Resolution::Failed("res_query gave no answer".into()),
        }
    }
}

//! The root certificates a user or an administrator added to Windows, for the user's Android
//! to trust beside its own.
//!
//! Windows keeps each trusted root store in the registry, one key per physical store, with the
//! certificates under its `Certificates` subkey. The service runs as SYSTEM, so the user's own
//! stores are read through their hive in `HKEY_USERS`, which Windows loads while they are signed
//! in. The stores read ([`stores`]):
//!
//! - the computer's Trusted Root Certification Authorities (`HKLM\SOFTWARE\Microsoft\
//!   SystemCertificates\Root`), where Windows keeps its own roots too;
//! - the roots the computer's group policy deploys (`HKLM\SOFTWARE\Policies\Microsoft\
//!   SystemCertificates\Root`) and those an Active Directory enterprise deploys
//!   (`HKLM\SOFTWARE\Microsoft\EnterpriseCertificates\Root`);
//! - the user's own Trusted Root Certification Authorities and the roots their group policy
//!   deploys, under `HKEY_USERS\<SID>`.
//!
//! Windows' own roots are left out ([`BuiltInRoots`]): every root of Microsoft's Trusted Root
//! Program, as the list Windows keeps of them says (the certificate trust list Automatic Root
//! Certificates Update writes to `HKLM\SOFTWARE\Microsoft\SystemCertificates\AuthRoot\
//! AutoUpdate\EncodedCtl`, which names each program root by its SHA-1 hash), Microsoft's own
//! product roots (crypt32's Microsoft root chain policy, which knows their keys), and the few
//! legacy roots Windows installs that are in neither. That list is the one Windows itself
//! trusts the program's roots from, so it is the same on every computer that updates its roots;
//! a computer that never downloaded it (root updates turned off) has only Windows' installed
//! roots in its stores beside what an administrator put there, and those installed roots are
//! the policy's and the legacy list's.
//!
//! A change to any of these keys wakes the watcher (`RegNotifyChangeKeyValue` on the key, or on
//! its nearest existing parent while it does not exist yet).

use std::time::Duration;

use dsa_host_core::host_certificates::{self, source, FoundCertificate, HostRootStore};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, WAIT_FAILED};
use windows_sys::Win32::Security::Cryptography::{
    CertCloseStore, CertCreateCTLContext, CertEnumCertificatesInStore, CertFreeCTLContext,
    CertFreeCertificateChain, CertFreeCertificateContext, CertGetCertificateChain,
    CertGetCertificateContextProperty, CertOpenStore, CertVerifyCertificateChainPolicy,
    CERT_CHAIN_PARA, CERT_CHAIN_POLICY_MICROSOFT_ROOT, CERT_CHAIN_POLICY_PARA,
    CERT_CHAIN_POLICY_STATUS, CERT_CONTEXT, CERT_SHA1_HASH_PROP_ID, CERT_STORE_PROV_REG,
    CERT_STORE_READONLY_FLAG, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW, HKEY,
    HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET,
    REG_NOTIFY_CHANGE_NAME,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

/// Where Windows keeps the list of Microsoft's Trusted Root Program, under HKLM.
const ROOT_PROGRAM_KEY: &str = r"SOFTWARE\Microsoft\SystemCertificates\AuthRoot\AutoUpdate";
const ROOT_PROGRAM_VALUE: &str = "EncodedCtl";

/// Roots Windows installs that neither the root program's list nor the Microsoft root policy
/// names: the legacy code signing and time stamping roots of the 1990s, which Windows keeps for
/// old signatures. Their SHA-1 thumbprints.
const LEGACY_WINDOWS_ROOTS: &[&str] = &[
    // Microsoft Root Authority.
    "a43489159a520f0d93d032ccaf37e7fe20a8b419",
    // Copyright (c) 1997 Microsoft Corp.
    "245c97df7514e7cf2df8be72ae957b9e04741e85",
    // Microsoft Authenticode(tm) Root Authority.
    "7f88cd7223f3c813818c994614a89c99fa3b5247",
    // NO LIABILITY ACCEPTED, (c)97 VeriSign, Inc.
    "18f7c1fcc3090203fd5baa2f861a754976c8dd25",
    // Thawte Timestamping CA.
    "be36a4562fb2ee05dbb3d32323adf445084ed656",
    // Microsoft Root Certificate Authority (2001), 2010 and 2011.
    "cdd4eeae6000ac7f40c3802c171e30148030c072",
    "3b1efd3a66ea28b16697394703a72ca340a05bd5",
    "8f43288ad272f3103b6fb1428485ea3014c0bcfe",
];

/// One store the service reads: its hive, its key and how the local API names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub machine: bool,
    pub path: String,
    pub source: &'static str,
}

impl Store {
    /// The store as Windows' registry editor names it.
    pub fn location(&self) -> String {
        let hive = if self.machine {
            "HKEY_LOCAL_MACHINE"
        } else {
            "HKEY_USERS"
        };
        format!(r"{hive}\{}", self.path)
    }
}

/// The trusted root stores of the computer and of the user `sid`.
pub fn stores(sid: &str) -> Vec<Store> {
    let machine = |path: &str, source| Store {
        machine: true,
        path: path.to_string(),
        source,
    };
    let user = |path: &str, source| Store {
        machine: false,
        path: format!(r"{sid}\{path}"),
        source,
    };
    vec![
        machine(
            r"SOFTWARE\Microsoft\SystemCertificates\Root",
            source::MACHINE,
        ),
        machine(
            r"SOFTWARE\Policies\Microsoft\SystemCertificates\Root",
            source::MACHINE_GROUP_POLICY,
        ),
        machine(
            r"SOFTWARE\Microsoft\EnterpriseCertificates\Root",
            source::MACHINE_ENTERPRISE,
        ),
        user(r"Software\Microsoft\SystemCertificates\Root", source::USER),
        user(
            r"Software\Policies\Microsoft\SystemCertificates\Root",
            source::USER_GROUP_POLICY,
        ),
    ]
}

/// What Windows ships, by SHA-1 thumbprint in lower-case hexadecimal.
#[derive(Debug, Default, Clone)]
pub struct BuiltInRoots {
    program: std::collections::HashSet<String>,
}

impl BuiltInRoots {
    /// The root program's list as Windows keeps it now. Empty (with a line in the log) when the
    /// computer has none.
    pub fn read() -> BuiltInRoots {
        let program = match read_root_program() {
            Ok(program) => program,
            Err(why) => {
                eprintln!(
                    "host certificates: Microsoft's root program list cannot be read ({why}); \
                     only Windows' installed roots are left out"
                );
                Default::default()
            }
        };
        BuiltInRoots { program }
    }

    /// Whether a root with this SHA-1 thumbprint is one Windows ships, without asking crypt32.
    pub fn listed(&self, sha1: &str) -> bool {
        self.program.contains(sha1) || LEGACY_WINDOWS_ROOTS.contains(&sha1)
    }
}

/// The user's trusted root stores and the computer's.
pub struct WindowsRootStore {
    sid: String,
}

impl WindowsRootStore {
    pub fn for_user(sid: &str) -> WindowsRootStore {
        WindowsRootStore {
            sid: sid.to_string(),
        }
    }
}

impl HostRootStore for WindowsRootStore {
    fn added_roots(&self) -> Result<Vec<FoundCertificate>, String> {
        let built_in = BuiltInRoots::read();
        let stores = stores(&self.sid);
        let mut found = Vec::new();
        let mut problems = Vec::new();
        for store in &stores {
            match read_store(store) {
                Ok(certificates) => found.extend(certificates),
                Err(why) => problems.push(format!("{}: {why}", store.location())),
            }
        }
        // With no store read, nothing is known, and what the guest has is kept.
        if problems.len() == stores.len() {
            return Err(problems.join("; "));
        }
        for problem in &problems {
            eprintln!("host certificates: {problem}");
        }
        Ok(host_certificates::added_roots(found, |certificate| {
            ships_with_windows(&certificate.der, &built_in)
        }))
    }

    fn wait_for_change(&self, timeout: Duration) {
        wait_for_registry_change(&stores(&self.sid), timeout);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An open registry key, closed when dropped.
struct Key(HKEY);

impl Key {
    fn open(machine: bool, path: &str, access: u32) -> Option<Key> {
        let root = if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_USERS
        };
        let path = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path; the key is closed when dropped.
        let code = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, access, &mut key) };
        (code == ERROR_SUCCESS).then_some(Key(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: a key this value opened.
        unsafe { RegCloseKey(self.0) };
    }
}

/// The certificates of one store. A store whose key does not exist holds none.
fn read_store(store: &Store) -> Result<Vec<FoundCertificate>, String> {
    let Some(key) = Key::open(store.machine, &store.path, KEY_READ) else {
        return Ok(Vec::new());
    };
    // SAFETY: the registry provider reads the store from the open key, which outlives it.
    let handle = unsafe {
        CertOpenStore(
            CERT_STORE_PROV_REG,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            0,
            CERT_STORE_READONLY_FLAG,
            key.0 as *const core::ffi::c_void,
        )
    };
    if handle.is_null() {
        return Err(format!(
            "the store cannot be opened ({})",
            std::io::Error::last_os_error()
        ));
    }
    let mut found = Vec::new();
    let mut context: *const CERT_CONTEXT = std::ptr::null();
    loop {
        // SAFETY: the enumeration frees the previous context and returns the next, or null.
        context = unsafe { CertEnumCertificatesInStore(handle, context) };
        if context.is_null() {
            break;
        }
        // SAFETY: a context the store returned, whose encoding it owns.
        let der = unsafe {
            std::slice::from_raw_parts((*context).pbCertEncoded, (*context).cbCertEncoded as usize)
        }
        .to_vec();
        found.push(FoundCertificate::new(der, store.source, store.location()));
    }
    // SAFETY: the store this function opened, with no context left open.
    unsafe { CertCloseStore(handle, 0) };
    drop(key);
    Ok(found)
}

/// The SHA-1 thumbprints of Microsoft's Trusted Root Program, from the list Windows keeps.
fn read_root_program() -> Result<std::collections::HashSet<String>, String> {
    let key = Key::open(true, ROOT_PROGRAM_KEY, KEY_READ)
        .ok_or_else(|| "the computer has never downloaded it".to_string())?;
    let name = wide(ROOT_PROGRAM_VALUE);
    let mut len: u32 = 0;
    // SAFETY: asks for the value's size only.
    let code = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if code != ERROR_SUCCESS || len == 0 {
        return Err("the computer has never downloaded it".into());
    }
    let mut data = vec![0u8; len as usize];
    // SAFETY: a buffer of the size the registry said.
    let code = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            data.as_mut_ptr(),
            &mut len,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(format!("reading it failed (error {code})"));
    }
    data.truncate(len as usize);
    // SAFETY: crypt32 parses the encoded list into a context of its own, freed below.
    let ctl = unsafe {
        CertCreateCTLContext(
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            data.as_ptr(),
            data.len() as u32,
        )
    };
    if ctl.is_null() {
        return Err(format!(
            "it cannot be parsed ({})",
            std::io::Error::last_os_error()
        ));
    }
    let mut thumbprints = std::collections::HashSet::new();
    // SAFETY: the list's entries, which the context owns until it is freed.
    unsafe {
        let info = &*(*ctl).pCtlInfo;
        for index in 0..info.cCTLEntry as usize {
            let entry = &*info.rgCTLEntry.add(index);
            let id = std::slice::from_raw_parts(
                entry.SubjectIdentifier.pbData,
                entry.SubjectIdentifier.cbData as usize,
            );
            thumbprints.insert(hex(id));
        }
        CertFreeCTLContext(ctl);
    }
    Ok(thumbprints)
}

/// Whether `der` is a root Windows ships: on the root program's list, on the legacy list, or
/// one of Microsoft's own product roots.
fn ships_with_windows(der: &[u8], built_in: &BuiltInRoots) -> bool {
    let Some(context) = certificate_context(der) else {
        return false;
    };
    let sha1 = thumbprint(context.0);
    if sha1.as_deref().is_some_and(|sha1| built_in.listed(sha1)) {
        return true;
    }
    microsoft_root(context.0)
}

/// A certificate context made from DER, freed when dropped.
struct Context(*const CERT_CONTEXT);

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: a context this value made.
        unsafe { CertFreeCertificateContext(self.0) };
    }
}

fn certificate_context(der: &[u8]) -> Option<Context> {
    use windows_sys::Win32::Security::Cryptography::CertCreateCertificateContext;
    // SAFETY: crypt32 copies the encoding into a context of its own.
    let context = unsafe {
        CertCreateCertificateContext(
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            der.as_ptr(),
            der.len() as u32,
        )
    };
    (!context.is_null()).then_some(Context(context))
}

/// The SHA-1 thumbprint of a certificate, in lower-case hexadecimal.
fn thumbprint(context: *const CERT_CONTEXT) -> Option<String> {
    let mut hash = [0u8; 20];
    let mut len = hash.len() as u32;
    // SAFETY: a buffer of the length given; crypt32 computes the hash when it has none kept.
    let ok = unsafe {
        CertGetCertificateContextProperty(
            context,
            CERT_SHA1_HASH_PROP_ID,
            hash.as_mut_ptr().cast(),
            &mut len,
        )
    };
    (ok != 0 && len == 20).then(|| hex(&hash))
}

/// Whether the certificate is one of Microsoft's own roots, by crypt32's Microsoft root chain
/// policy, which checks its key against the keys Windows knows.
fn microsoft_root(context: *const CERT_CONTEXT) -> bool {
    let mut para: CERT_CHAIN_PARA = unsafe { std::mem::zeroed() };
    para.cbSize = std::mem::size_of::<CERT_CHAIN_PARA>() as u32;
    let mut chain = std::ptr::null_mut();
    // SAFETY: the default chain engine, the certificate's own chain; freed below.
    let ok = unsafe {
        CertGetCertificateChain(
            std::ptr::null_mut(),
            context,
            std::ptr::null(),
            std::ptr::null_mut(),
            &para,
            0,
            std::ptr::null(),
            &mut chain,
        )
    };
    if ok == 0 || chain.is_null() {
        return false;
    }
    let mut policy: CERT_CHAIN_POLICY_PARA = unsafe { std::mem::zeroed() };
    policy.cbSize = std::mem::size_of::<CERT_CHAIN_POLICY_PARA>() as u32;
    let mut status: CERT_CHAIN_POLICY_STATUS = unsafe { std::mem::zeroed() };
    status.cbSize = std::mem::size_of::<CERT_CHAIN_POLICY_STATUS>() as u32;
    // SAFETY: the chain built above and structures of the sizes they say.
    let checked = unsafe {
        CertVerifyCertificateChainPolicy(
            CERT_CHAIN_POLICY_MICROSOFT_ROOT,
            chain,
            &policy,
            &mut status,
        )
    };
    // SAFETY: the chain built above.
    unsafe { CertFreeCertificateChain(chain) };
    checked != 0 && status.dwError == 0
}

/// Waits until a key of `stores` changes, or `timeout` passes. A store's key that does not exist
/// yet is followed through its nearest parent that does, so a store made later wakes it too.
fn wait_for_registry_change(stores: &[Store], timeout: Duration) {
    let mut keys = Vec::new();
    let mut events: Vec<HANDLE> = Vec::new();
    for store in stores {
        let mut path = store.path.as_str();
        let key = loop {
            if let Some(key) = Key::open(store.machine, path, KEY_NOTIFY) {
                break Some(key);
            }
            match path.rfind('\\') {
                Some(at) => path = &path[..at],
                None => break None,
            }
        };
        let Some(key) = key else { continue };
        // SAFETY: an unnamed manual-reset event, closed below.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            continue;
        }
        // SAFETY: an open key and an event; the notification fires once.
        let code = unsafe {
            RegNotifyChangeKeyValue(
                key.0,
                1,
                REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET,
                event,
                1,
            )
        };
        if code != ERROR_SUCCESS {
            // SAFETY: the event made above.
            unsafe { CloseHandle(event) };
            continue;
        }
        keys.push(key);
        events.push(event);
    }
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    if events.is_empty() {
        std::thread::sleep(timeout);
        return;
    }
    // SAFETY: events this function made; a change to any key signals its event.
    let result = unsafe { WaitForMultipleObjects(events.len() as u32, events.as_ptr(), 0, millis) };
    if result == WAIT_FAILED {
        std::thread::sleep(timeout.min(Duration::from_secs(5)));
    }
    for event in events {
        // SAFETY: the events made above.
        unsafe { CloseHandle(event) };
    }
    drop(keys);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr28_the_computers_and_the_users_root_stores_are_read() {
        let stores = stores("S-1-5-21-1-2-3-1001");
        let locations: Vec<String> = stores.iter().map(Store::location).collect();
        assert_eq!(
            locations,
            [
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\SystemCertificates\Root",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Policies\Microsoft\SystemCertificates\Root",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\EnterpriseCertificates\Root",
                r"HKEY_USERS\S-1-5-21-1-2-3-1001\Software\Microsoft\SystemCertificates\Root",
                r"HKEY_USERS\S-1-5-21-1-2-3-1001\Software\Policies\Microsoft\SystemCertificates\Root",
            ]
        );
        let sources: Vec<&str> = stores.iter().map(|store| store.source).collect();
        assert_eq!(
            sources,
            [
                source::MACHINE,
                source::MACHINE_GROUP_POLICY,
                source::MACHINE_ENTERPRISE,
                source::USER,
                source::USER_GROUP_POLICY,
            ]
        );
    }

    #[test]
    fn fr28_windows_legacy_roots_are_built_in_and_others_are_not() {
        let built_in = BuiltInRoots::default();
        assert!(built_in.listed("a43489159a520f0d93d032ccaf37e7fe20a8b419"));
        assert!(!built_in.listed("0000000000000000000000000000000000000000"));
        let program = BuiltInRoots {
            program: ["1111111111111111111111111111111111111111".to_string()].into(),
        };
        assert!(program.listed("1111111111111111111111111111111111111111"));
    }
}

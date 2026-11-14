//! The root certificates a user or an administrator added to the Mac, for the user's Android to
//! trust beside its own.
//!
//! macOS keeps whom it trusts in three trust settings domains: the system's (the roots Apple
//! ships, which nobody changes), the administrator's (what an administrator or a configuration
//! profile set for every user) and the user's own. The service runs as the user, so it reads the
//! administrator's domain and the user's with `SecTrustSettingsCopyCertificates`. A certificate
//! there counts when its trust settings make it a trusted root (no settings at all, or a setting
//! whose result is "trust root" or "trust as root") and none denies it; one that is also in the
//! system domain is left out, since that only changes how a shipped root is trusted.
//!
//! Security.framework posts a Darwin notification when trust settings change, which wakes the
//! watcher; the stores are also read again at the core's rescan pace, whatever is posted.

use std::ffi::{c_char, c_void};
use std::time::{Duration, Instant};

use dsa_host_core::host_certificates::{self, source, FoundCertificate, HostRootStore};

use crate::security::{CFTypeRef, Cf};

/// The trust settings domains (`SecTrustSettingsDomain`).
const DOMAIN_USER: u32 = 0;
const DOMAIN_ADMIN: u32 = 1;
const DOMAIN_SYSTEM: u32 = 2;

/// `errSecNoTrustSettings`: the domain holds no certificate.
const NO_TRUST_SETTINGS: i32 = -25263;

/// The key of a trust setting's result. Security.framework's header defines
/// `kSecTrustSettingsResult` as a `CFSTR` macro of this text, not as an exported symbol, so the
/// key is made here from the same text.
const RESULT_KEY: &str = "kSecTrustSettingsResult";

/// The result values under that key.
const RESULT_TRUST_ROOT: i32 = 1;
const RESULT_TRUST_AS_ROOT: i32 = 2;
const RESULT_DENY: i32 = 3;

/// `kCFNumberSInt32Type`.
const SINT32: isize = 3;

/// The Darwin notifications trust settings changes are posted under.
const CHANGE_NOTIFICATIONS: &[&str] = &[
    "com.apple.security.trustsettingschanged",
    "com.apple.trust-settings.admin",
    "com.apple.trust-settings.user",
];

/// How often a wait looks whether a change was posted.
const NOTIFY_POLL: Duration = Duration::from_secs(2);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecTrustSettingsCopyCertificates(domain: u32, certificates: *mut CFTypeRef) -> i32;
    fn SecTrustSettingsCopyTrustSettings(
        certificate: CFTypeRef,
        domain: u32,
        settings: *mut CFTypeRef,
    ) -> i32;
    fn SecCertificateCopyData(certificate: CFTypeRef) -> CFTypeRef;
}

// libSystem's notify(3).
extern "C" {
    fn notify_register_check(name: *const c_char, token: *mut i32) -> u32;
    fn notify_check(token: i32, check: *mut i32) -> u32;
    fn notify_cancel(token: i32) -> u32;
}

/// The administrator's and the user's trust settings.
#[derive(Default)]
pub struct MacRootStore;

impl HostRootStore for MacRootStore {
    fn added_roots(&self) -> Result<Vec<FoundCertificate>, String> {
        let system: std::collections::HashSet<Vec<u8>> = certificates_of(DOMAIN_SYSTEM)
            .unwrap_or_default()
            .into_iter()
            .map(|(der, _)| der)
            .collect();
        let mut found = Vec::new();
        for (domain, source, location) in [
            (DOMAIN_ADMIN, source::ADMIN, "administrator trust settings"),
            (DOMAIN_USER, source::USER, "user trust settings"),
        ] {
            for (der, trusted) in certificates_of(domain)? {
                if trusted {
                    found.push(FoundCertificate::new(der, source, location));
                }
            }
        }
        Ok(host_certificates::added_roots(found, |certificate| {
            system.contains(&certificate.der)
        }))
    }

    fn wait_for_change(&self, timeout: Duration) {
        let tokens: Vec<i32> = CHANGE_NOTIFICATIONS
            .iter()
            .filter_map(|name| {
                let name = std::ffi::CString::new(*name).ok()?;
                let mut token = 0;
                // SAFETY: a NUL-terminated name; the token is cancelled below.
                (unsafe { notify_register_check(name.as_ptr(), &mut token) } == 0).then_some(token)
            })
            .collect();
        // The first check of a new token says nothing has changed yet.
        for &token in &tokens {
            let mut check = 0;
            // SAFETY: a token registered above.
            unsafe { notify_check(token, &mut check) };
        }
        let deadline = Instant::now() + timeout;
        'wait: while Instant::now() < deadline {
            std::thread::sleep(NOTIFY_POLL.min(deadline.saturating_duration_since(Instant::now())));
            for &token in &tokens {
                let mut check = 0;
                // SAFETY: a token registered above.
                if unsafe { notify_check(token, &mut check) } == 0 && check != 0 {
                    break 'wait;
                }
            }
        }
        for token in tokens {
            // SAFETY: a token registered above.
            unsafe { notify_cancel(token) };
        }
    }
}

/// Each certificate of `domain`, DER-encoded, with whether its settings there make it a trusted
/// root.
fn certificates_of(domain: u32) -> Result<Vec<(Vec<u8>, bool)>, String> {
    let mut array: CFTypeRef = std::ptr::null();
    // SAFETY: Security.framework hands back an array this code then owns.
    let status = unsafe { SecTrustSettingsCopyCertificates(domain, &mut array) };
    // SAFETY: the array the call made, or null.
    let array = unsafe { Cf::owned(array) };
    if status == NO_TRUST_SETTINGS {
        return Ok(Vec::new());
    }
    if status != 0 || array.is_null() {
        return Err(format!(
            "the trust settings cannot be read: {}",
            crate::security::status_message(status)
        ));
    }
    // SAFETY: a CFArray of SecCertificates, which it owns.
    let count = unsafe { CFArrayGetCount(array.get()) };
    let mut found = Vec::new();
    for index in 0..count {
        // SAFETY: an index within the array; the certificate is the array's.
        let certificate = unsafe { CFArrayGetValueAtIndex(array.get(), index) };
        // SAFETY: a SecCertificate; the data is a copy this code owns.
        let data = unsafe { Cf::owned(SecCertificateCopyData(certificate)) };
        let Some(der) = data.bytes() else {
            continue;
        };
        found.push((der, trusted_root(certificate, domain)));
    }
    Ok(found)
}

/// Whether a certificate's trust settings in `domain` make it a trusted root: no settings at
/// all (trusted as a root for everything), or one that trusts it as a root, and none that
/// denies it.
fn trusted_root(certificate: CFTypeRef, domain: u32) -> bool {
    let mut settings: CFTypeRef = std::ptr::null();
    // SAFETY: a SecCertificate; the settings are an array this code then owns.
    let status = unsafe { SecTrustSettingsCopyTrustSettings(certificate, domain, &mut settings) };
    // SAFETY: the array the call made, or null.
    let settings = unsafe { Cf::owned(settings) };
    if status != 0 || settings.is_null() {
        return false;
    }
    // SAFETY: a CFArray of CFDictionaries, which it owns.
    let count = unsafe { CFArrayGetCount(settings.get()) };
    if count == 0 {
        return true;
    }
    let result_key = Cf::string(RESULT_KEY);
    let mut trusted = false;
    for index in 0..count {
        // SAFETY: an index within the array; the dictionary and its number are the array's.
        let result = unsafe {
            let setting = CFArrayGetValueAtIndex(settings.get(), index);
            let number = CFDictionaryGetValue(setting, result_key.get());
            let mut value: i32 = RESULT_TRUST_ROOT;
            if !number.is_null() {
                CFNumberGetValue(number, SINT32, (&mut value as *mut i32).cast());
            }
            value
        };
        match result {
            RESULT_DENY => return false,
            RESULT_TRUST_ROOT | RESULT_TRUST_AS_ROOT => trusted = true,
            _ => {}
        }
    }
    trusted
}

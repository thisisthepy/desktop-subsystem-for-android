//! The root certificates a user or an administrator added to the host, which Android trusts as
//! system certificates beside its own.
//!
//! Internet filters and company proxies that inspect HTTPS install a root certificate of their
//! own in the computer's trust store. The host's browsers trust it, and Android's apps reach the
//! internet on that computer only if Android trusts it too. The platform reads its trust stores
//! ([`HostRootStore`]) and leaves out the roots its operating system ships, so only what someone
//! added reaches the guest. The session sends what it found to the guest's control daemon each
//! time the daemon or the agent connects, and again whenever the store or the user's setting
//! changes ([`watch`]); the daemon adds them to Android's system certificate folders and takes
//! away those it added that are no longer named.
//!
//! Nothing here names a host API: each platform reads its own stores, and the parsing, the
//! filtering and the watching are the same for all of them.

use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// The most certificates the guest is given. A trust store holds a few dozen at most; the limit
/// keeps a store that someone filled with thousands from making a message the guest cannot take.
pub const MAX_CERTIFICATES: usize = 500;

/// The largest certificate the guest is given, in bytes. A root certificate is a few KiB.
pub const MAX_CERTIFICATE_LEN: usize = 64 * 1024;

/// How long the watcher waits for the store to say it changed before it reads it again anyway.
/// A store that cannot say (a folder made after the watch began, a key that did not exist) is
/// still followed, at this pace.
pub const RESCAN: Duration = Duration::from_secs(60);

/// Where a certificate was found, as the local API names it.
pub mod source {
    /// The computer's trusted root store (on Windows, Local Computer, Trusted Root
    /// Certification Authorities).
    pub const MACHINE: &str = "machine";
    /// Roots the computer's group policy deploys.
    pub const MACHINE_GROUP_POLICY: &str = "machine_group_policy";
    /// Roots an Active Directory enterprise deploys.
    pub const MACHINE_ENTERPRISE: &str = "machine_enterprise";
    /// The user's own trusted root store, or (on macOS) the user's trust settings.
    pub const USER: &str = "user";
    /// Roots the user's group policy deploys.
    pub const USER_GROUP_POLICY: &str = "user_group_policy";
    /// The computer's administrator trust settings (macOS).
    pub const ADMIN: &str = "admin";
    /// A certificate added to the system's certificate store beside the distribution's bundle
    /// (Linux).
    pub const SYSTEM: &str = "system";
}

/// A root certificate the platform found in one of its trust stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundCertificate {
    /// The certificate, DER-encoded X.509.
    pub der: Vec<u8>,
    /// One of the names in [`source`].
    pub source: String,
    /// The store or the file it was found in, as the operating system names it.
    pub location: String,
}

impl FoundCertificate {
    pub fn new(der: Vec<u8>, source: &str, location: impl Into<String>) -> FoundCertificate {
        FoundCertificate {
            der,
            source: source.to_string(),
            location: location.into(),
        }
    }
}

/// A certificate the guest is given, as the settings app lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCertificate {
    /// Who the certificate is for and who signed it, as distinguished names.
    pub subject: String,
    pub issuer: String,
    /// One of the names in [`source`].
    pub source: String,
    pub location: String,
    /// The SHA-256 fingerprint, in lower-case hexadecimal.
    pub sha256: String,
    /// When it stops being valid, in seconds since the Unix epoch; 0 when unknown.
    pub not_after: i64,
}

/// One platform's trust stores.
pub trait HostRootStore: Send + Sync {
    /// Every root certificate a user or an administrator added, with the operating system's own
    /// left out. The order is the stores' order; [`added_roots`] drops the repeats.
    fn added_roots(&self) -> Result<Vec<FoundCertificate>, String>;

    /// Waits until a store may have changed, or `timeout` has passed. It may return early for
    /// no reason: the watcher reads the stores again and sends only what changed.
    fn wait_for_change(&self, timeout: Duration);
}

/// What a certificate says of itself, read from its DER encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateFacts {
    pub subject: String,
    pub issuer: String,
    /// The subject Name as it is encoded, which Android names its certificate files by.
    pub subject_der: Vec<u8>,
    pub not_after: i64,
}

/// Reads the subject, the issuer and the end of validity of a DER-encoded X.509 certificate.
/// An error says what is wrong with it.
pub fn facts(der: &[u8]) -> Result<CertificateFacts, String> {
    let mut outer = Der::new(der);
    let certificate = outer.expect(SEQUENCE, "the certificate")?;
    if !outer.is_empty() {
        return Err("bytes follow the certificate".into());
    }
    let mut certificate = Der::new(certificate);
    let tbs = certificate.expect(SEQUENCE, "the certificate's body")?;
    let mut tbs = Der::new(tbs);
    if tbs.peek_tag() == Some(0xa0) {
        tbs.next("the version")?;
    }
    tbs.expect(INTEGER, "the serial number")?;
    tbs.expect(SEQUENCE, "the signature algorithm")?;
    let (issuer, _) = tbs.expect_with_der(SEQUENCE, "the issuer")?;
    let validity = tbs.expect(SEQUENCE, "the validity")?;
    let (subject, subject_der) = tbs.expect_with_der(SEQUENCE, "the subject")?;
    tbs.expect(SEQUENCE, "the public key")?;
    let mut validity = Der::new(validity);
    validity.next("the start of validity")?;
    let (tag, end) = validity.next("the end of validity")?;
    Ok(CertificateFacts {
        subject: name_text(subject)?,
        issuer: name_text(issuer)?,
        subject_der: subject_der.to_vec(),
        not_after: time(tag, end).unwrap_or(0),
    })
}

/// The certificates in a certificate file as the systems keep them: DER, or PEM with any number
/// of `CERTIFICATE` blocks (`TRUSTED CERTIFICATE` blocks too, whose trust settings after the
/// certificate are dropped). Text around the blocks is ignored; a block that does not decode is
/// skipped.
pub fn certificates_in_file(bytes: &[u8]) -> Vec<Vec<u8>> {
    if bytes.first() == Some(&SEQUENCE) {
        return vec![bytes.to_vec()];
    }
    let text = String::from_utf8_lossy(bytes);
    let mut found = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("-----BEGIN ") {
        rest = &rest[start + "-----BEGIN ".len()..];
        let Some(label_end) = rest.find("-----") else {
            break;
        };
        let label = rest[..label_end].to_string();
        rest = &rest[label_end + 5..];
        let end_marker = format!("-----END {label}-----");
        let Some(end) = rest.find(&end_marker) else {
            break;
        };
        let body = &rest[..end];
        rest = &rest[end + end_marker.len()..];
        if label != "CERTIFICATE" && label != "TRUSTED CERTIFICATE" {
            continue;
        }
        let Some(mut der) = base64_decode(body) else {
            continue;
        };
        if label == "TRUSTED CERTIFICATE" {
            // The certificate, then OpenSSL's trust settings: keep the first element.
            let mut element = Der::new(&der);
            match element.next_with_der("the certificate") {
                Ok((_, _, whole)) => der = whole.to_vec(),
                Err(_) => continue,
            }
        }
        found.push(der);
    }
    found
}

/// Standard base64, with whitespace ignored. `None` when it is not base64.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The SHA-256 fingerprint of `der`, in lower-case hexadecimal.
pub fn sha256_hex(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The certificates the guest is given out of what the stores hold: each parses as X.509, is
/// not one `built_in` says the operating system ships, and is not given twice (the first store
/// it was found in is the one named). At most [`MAX_CERTIFICATES`], none larger than
/// [`MAX_CERTIFICATE_LEN`]. Each one left out is logged with its reason.
pub fn added_roots(
    found: Vec<FoundCertificate>,
    built_in: impl Fn(&FoundCertificate) -> bool,
) -> Vec<FoundCertificate> {
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for certificate in found {
        if certificate.der.len() > MAX_CERTIFICATE_LEN {
            eprintln!(
                "host certificates: left out a certificate of {} bytes in {}, larger than \
                 {MAX_CERTIFICATE_LEN}",
                certificate.der.len(),
                certificate.location
            );
            continue;
        }
        if let Err(why) = facts(&certificate.der) {
            eprintln!(
                "host certificates: left out a certificate in {} that cannot be read: {why}",
                certificate.location
            );
            continue;
        }
        if built_in(&certificate) || !seen.insert(certificate.der.clone()) {
            continue;
        }
        if kept.len() == MAX_CERTIFICATES {
            eprintln!(
                "host certificates: the stores hold more than {MAX_CERTIFICATES} added roots; \
                 the rest are left out"
            );
            break;
        }
        kept.push(certificate);
    }
    kept
}

/// What the settings app shows of `certificate`.
pub fn imported(certificate: &FoundCertificate) -> ImportedCertificate {
    let facts = facts(&certificate.der).ok();
    ImportedCertificate {
        subject: facts
            .as_ref()
            .map(|f| f.subject.clone())
            .unwrap_or_default(),
        issuer: facts.as_ref().map(|f| f.issuer.clone()).unwrap_or_default(),
        source: certificate.source.clone(),
        location: certificate.location.clone(),
        sha256: sha256_hex(&certificate.der),
        not_after: facts.map_or(0, |f| f.not_after),
    }
}

/// Follows `store` on a thread of its own until `alive` returns `None`: reads it now, and again
/// each time it may have changed, and hands each new set of added roots to `changed`. A read that
/// fails is logged and the last set kept, so a store that cannot be read for a moment does not
/// take Android's certificates away.
pub fn watch<T: Send + Sync + 'static>(
    store: Arc<dyn HostRootStore>,
    owner: Weak<T>,
    changed: impl Fn(&T, Vec<FoundCertificate>) + Send + 'static,
) {
    let spawned = thread::Builder::new()
        .name("dsa-host-certificates".into())
        .spawn(move || {
            let mut last: Option<Vec<FoundCertificate>> = None;
            let mut reported = false;
            loop {
                let found =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.added_roots()))
                        .unwrap_or_else(|_| Err("reading the trust stores panicked".into()));
                match found {
                    Ok(found) => {
                        reported = false;
                        let found = added_roots(found, |_| false);
                        if last.as_ref() != Some(&found) {
                            let Some(owner) = owner.upgrade() else {
                                return;
                            };
                            eprintln!(
                                "host certificates: {} root certificates added on this computer",
                                found.len()
                            );
                            changed(&owner, found.clone());
                            last = Some(found);
                        }
                    }
                    Err(why) => {
                        if !reported {
                            eprintln!("host certificates: the trust stores cannot be read: {why}");
                            reported = true;
                        }
                    }
                }
                if owner.strong_count() == 0 {
                    return;
                }
                store.wait_for_change(RESCAN);
                if owner.strong_count() == 0 {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("host certificates: the trust stores cannot be followed: {error}");
    }
}

// ---------------------------------------------------------------------------------------------
// DER, as much of it as a certificate's names and validity need.

const INTEGER: u8 = 0x02;
const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;

struct Der<'a> {
    rest: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Der<'a> {
        Der { rest: bytes }
    }

    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    fn peek_tag(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// The next element: its tag and its contents.
    fn next(&mut self, what: &str) -> Result<(u8, &'a [u8]), String> {
        self.next_with_der(what)
            .map(|(tag, contents, _)| (tag, contents))
    }

    /// The next element: its tag, its contents and the whole element as encoded.
    fn next_with_der(&mut self, what: &str) -> Result<(u8, &'a [u8], &'a [u8]), String> {
        let whole = self.rest;
        let short = || format!("{what} is cut short");
        let (&tag, rest) = whole.split_first().ok_or_else(short)?;
        if tag & 0x1f == 0x1f {
            return Err(format!("{what} has a tag DER certificates do not use"));
        }
        let (&first, mut rest) = rest.split_first().ok_or_else(short)?;
        let len = if first < 0x80 {
            usize::from(first)
        } else {
            let count = usize::from(first & 0x7f);
            if count == 0 || count > 4 || rest.len() < count {
                return Err(format!("{what} has a length that cannot be read"));
            }
            let (bytes, after) = rest.split_at(count);
            rest = after;
            bytes.iter().fold(0usize, |n, b| (n << 8) | usize::from(*b))
        };
        if rest.len() < len {
            return Err(short());
        }
        let (contents, after) = rest.split_at(len);
        let header = whole.len() - rest.len();
        self.rest = after;
        Ok((tag, contents, &whole[..header + len]))
    }

    fn expect(&mut self, tag: u8, what: &str) -> Result<&'a [u8], String> {
        self.expect_with_der(tag, what)
            .map(|(contents, _)| contents)
    }

    fn expect_with_der(&mut self, tag: u8, what: &str) -> Result<(&'a [u8], &'a [u8]), String> {
        let (found, contents, der) = self.next_with_der(what)?;
        if found != tag {
            return Err(format!("{what} is not where a certificate has it"));
        }
        Ok((contents, der))
    }
}

/// A Name as text: each attribute as `<short name>=<value>`, in the order they are encoded,
/// joined by ", ". An attribute with no short name is given by its object identifier.
fn name_text(name: &[u8]) -> Result<String, String> {
    let mut parts = Vec::new();
    let mut rdns = Der::new(name);
    while !rdns.is_empty() {
        let set = rdns.expect(SET, "a part of a name")?;
        let mut attributes = Der::new(set);
        while !attributes.is_empty() {
            let attribute = attributes.expect(SEQUENCE, "an attribute of a name")?;
            let mut attribute = Der::new(attribute);
            let oid = attribute.expect(OID, "an attribute's type")?;
            let (tag, value) = attribute.next("an attribute's value")?;
            let key = attribute_name(oid).map_or_else(|| oid_text(oid), str::to_string);
            parts.push(format!("{key}={}", string_value(tag, value)));
        }
    }
    Ok(parts.join(", "))
}

fn attribute_name(oid: &[u8]) -> Option<&'static str> {
    Some(match oid {
        [0x55, 0x04, 0x03] => "CN",
        [0x55, 0x04, 0x04] => "SN",
        [0x55, 0x04, 0x05] => "SERIALNUMBER",
        [0x55, 0x04, 0x06] => "C",
        [0x55, 0x04, 0x07] => "L",
        [0x55, 0x04, 0x08] => "ST",
        [0x55, 0x04, 0x09] => "STREET",
        [0x55, 0x04, 0x0a] => "O",
        [0x55, 0x04, 0x0b] => "OU",
        [0x55, 0x04, 0x0c] => "T",
        [0x55, 0x04, 0x11] => "PostalCode",
        [0x55, 0x04, 0x2a] => "G",
        [0x55, 0x04, 0x2e] => "dnQualifier",
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x01] => "E",
        [0x09, 0x92, 0x26, 0x89, 0x93, 0xf2, 0x2c, 0x64, 0x01, 0x19] => "DC",
        [0x09, 0x92, 0x26, 0x89, 0x93, 0xf2, 0x2c, 0x64, 0x01, 0x01] => "UID",
        _ => return None,
    })
}

/// An object identifier in dotted form.
fn oid_text(oid: &[u8]) -> String {
    let mut arcs: Vec<u64> = Vec::new();
    let mut value: u64 = 0;
    for &byte in oid {
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            if arcs.is_empty() {
                let first = (value / 40).min(2);
                arcs.push(first);
                arcs.push(value - first * 40);
            } else {
                arcs.push(value);
            }
            value = 0;
        }
    }
    arcs.iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// A directory string's text, whichever of the string types it is in.
fn string_value(tag: u8, value: &[u8]) -> String {
    match tag {
        // BMPString: UTF-16, big-endian.
        0x1e => {
            let units: Vec<u16> = value
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        // UniversalString: UTF-32, big-endian.
        0x1c => value
            .chunks_exact(4)
            .map(|four| {
                char::from_u32(u32::from_be_bytes([four[0], four[1], four[2], four[3]]))
                    .unwrap_or(char::REPLACEMENT_CHARACTER)
            })
            .collect(),
        // T61String: read as Latin-1, as most software does.
        0x14 => value.iter().map(|&b| char::from(b)).collect(),
        // UTF8String, PrintableString, IA5String, VisibleString, NumericString, and anything
        // else: UTF-8, or Latin-1 when it is not.
        _ => match std::str::from_utf8(value) {
            Ok(text) => text.to_string(),
            Err(_) => value.iter().map(|&b| char::from(b)).collect(),
        },
    }
}

/// A UTCTime or GeneralizedTime in seconds since the Unix epoch.
fn time(tag: u8, value: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(value).ok()?;
    let text = text.strip_suffix('Z')?;
    let (year, rest) = match tag {
        0x17 => {
            let year: i64 = text.get(..2)?.parse().ok()?;
            (
                if year < 50 { 2000 + year } else { 1900 + year },
                &text[2..],
            )
        }
        0x18 => (text.get(..4)?.parse().ok()?, &text[4..]),
        _ => return None,
    };
    if rest.len() < 10 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |at: usize| rest[at..at + 2].parse::<i64>().ok();
    let (month, day, hour, minute, second) =
        (field(0)?, field(2)?, field(4)?, field(6)?, field(8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days from 1970-01-01 to a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2030, 1, 1), 21_915);
    }

    #[test]
    fn times_of_both_kinds_are_read() {
        assert_eq!(time(0x17, b"300101000000Z"), Some(1_893_456_000));
        assert_eq!(time(0x18, b"20300101000000Z"), Some(1_893_456_000));
        assert_eq!(time(0x17, b"not a time"), None);
    }

    #[test]
    fn fr28_pem_and_der_certificate_files_are_read() {
        assert_eq!(base64_decode("aGVs bG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert!(base64_decode("not*base64").is_none());
        let der = [0x30, 0x03, 0x02, 0x01, 0x05];
        assert_eq!(certificates_in_file(&der), vec![der.to_vec()]);
        let pem = "comment\n-----BEGIN CERTIFICATE-----\nMAMCAQU=\n-----END CERTIFICATE-----\n\
                   -----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n\
                   -----BEGIN TRUSTED CERTIFICATE-----\nMAMCAQUwAA==\n-----END TRUSTED CERTIFICATE-----\n";
        assert_eq!(
            certificates_in_file(pem.as_bytes()),
            vec![der.to_vec(), der.to_vec()],
            "the private key is skipped and the trust settings dropped"
        );
    }

    #[test]
    fn an_unknown_attribute_is_named_by_its_oid() {
        assert_eq!(
            oid_text(&[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37]),
            "1.3.6.1.4.1.311"
        );
    }
}

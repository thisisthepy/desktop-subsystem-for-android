//! Who signed an APK: the certificate of its signer, as the APK Signature Scheme (v3.1, v3
//! or v2) or, for an old APK, the JAR signature (v1) carries it.
//!
//! The signature itself is not checked here; Android checks it when it installs the APK and
//! refuses one that does not verify. The host reads the certificate only to show it and to
//! tell, before Android is asked, whether the APK can replace the installed app.

use sha2::{Digest, Sha256};

use super::zip::Archive;

pub(crate) const BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";
const V2: u32 = 0x7109_871a;
const V3: u32 = 0xf053_68c0;
const V31: u32 = 0x1b93_ad61;
/// The v3 signer's proof of rotation: the chain of earlier signing certificates.
const PROOF_OF_ROTATION: u32 = 0x3ba0_6f8c;

/// The largest signing block read.
const MAX_BLOCK: u64 = 16 * 1024 * 1024;

/// Which scheme the signer was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    V1,
    V2,
    V3,
    V31,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::V1 => "v1",
            Scheme::V2 => "v2",
            Scheme::V3 => "v3",
            Scheme::V31 => "v3.1",
        }
    }
}

/// The APK's signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    pub scheme: Scheme,
    /// The signer's certificate, DER-encoded.
    pub certificate: Vec<u8>,
    /// The SHA-256 digest of each earlier signing certificate the proof of rotation names,
    /// oldest first. Android takes an update signed by the current key over an app signed
    /// by one of these.
    pub past_signers: Vec<Vec<u8>>,
}

impl Signer {
    /// The SHA-256 digest of the certificate, as `apksigner` and the package manager show it.
    pub fn sha256(&self) -> Vec<u8> {
        Sha256::digest(&self.certificate).to_vec()
    }
}

/// A length-prefixed (u32, little-endian) sequence, as the signing block nests its parts.
struct Seq<'a>(&'a [u8]);

impl<'a> Seq<'a> {
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.0.len() < 4 {
            return None;
        }
        let length = u32::from_le_bytes(self.0[..4].try_into().unwrap()) as usize;
        let item = self.0.get(4..4 + length)?;
        self.0 = &self.0[4 + length..];
        Some(item)
    }

    fn u32(&mut self) -> Option<u32> {
        let value = u32::from_le_bytes(self.0.get(..4)?.try_into().unwrap());
        self.0 = &self.0[4..];
        Some(value)
    }
}

/// The signing block's pairs of ID and value, read from the bytes before the central
/// directory. None when the APK has no signing block.
pub fn signing_block_pairs(block: &[u8]) -> Option<Vec<(u32, &[u8])>> {
    // The block: its size (u64), the pairs, its size again, and the magic.
    let pairs = block.get(8..block.len().checked_sub(24)?)?;
    let mut out = Vec::new();
    let mut at = 0;
    while at + 12 <= pairs.len() {
        let length = u64::from_le_bytes(pairs[at..at + 8].try_into().unwrap()) as usize;
        if length < 4 || at + 8 + length > pairs.len() {
            return None;
        }
        let id = u32::from_le_bytes(pairs[at + 8..at + 12].try_into().unwrap());
        out.push((id, &pairs[at + 12..at + 8 + length]));
        at += 8 + length;
    }
    Some(out)
}

/// The first signer of a v2 or v3 scheme value: its first certificate, and (v3) the
/// earlier certificates its proof of rotation names.
fn scheme_signer(value: &[u8], v3: bool) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut signers = Seq(Seq(value).next()?);
    let signer = signers.next()?;
    let signed_data = Seq(signer).next()?;
    let mut data = Seq(signed_data);
    let _digests = data.next()?;
    let certificate = Seq(data.next()?).next()?.to_vec();
    let mut past = Vec::new();
    if v3 {
        let _min_sdk = data.u32()?;
        let _max_sdk = data.u32()?;
        if let Some(attributes) = data.next() {
            let mut attributes = Seq(attributes);
            while let Some(attribute) = attributes.next() {
                let mut attribute = Seq(attribute);
                if attribute.u32() == Some(PROOF_OF_ROTATION) {
                    past = rotation_certificates(attribute.0);
                }
            }
        }
    }
    Some((certificate, past))
}

/// The SHA-256 digest of each certificate of a proof of rotation, oldest first, without the
/// last (which is the current signer).
fn rotation_certificates(lineage: &[u8]) -> Vec<Vec<u8>> {
    let mut lineage = Seq(lineage);
    let _version = lineage.u32();
    let mut certificates = Vec::new();
    while let Some(node) = lineage.next() {
        let Some(signed) = Seq(node).next() else {
            break;
        };
        let Some(certificate) = Seq(signed).next() else {
            break;
        };
        certificates.push(Sha256::digest(certificate).to_vec());
    }
    certificates.pop();
    certificates
}

/// Reads the APK's signer from its signing block, else from its JAR signature. None when
/// the APK is not signed in a way read here.
pub fn signer<R: std::io::Read + std::io::Seek>(archive: &mut Archive<R>) -> Option<Signer> {
    if let Some(signer) = block_signer(archive) {
        return Some(signer);
    }
    jar_signer(archive)
}

fn block_signer<R: std::io::Read + std::io::Seek>(archive: &mut Archive<R>) -> Option<Signer> {
    let end = archive.central_directory_offset;
    let footer = archive.read_raw(end.checked_sub(24)?, 24).ok()?;
    if &footer[8..] != BLOCK_MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(footer[..8].try_into().unwrap());
    if size > MAX_BLOCK || size + 8 > end {
        return None;
    }
    let block = archive.read_raw(end - size - 8, (size + 8) as usize).ok()?;
    let pairs = signing_block_pairs(&block)?;
    for (id, scheme, v3) in [
        (V31, Scheme::V31, true),
        (V3, Scheme::V3, true),
        (V2, Scheme::V2, false),
    ] {
        if let Some((_, value)) = pairs.iter().find(|(pair, _)| *pair == id) {
            if let Some((certificate, past_signers)) = scheme_signer(value, v3) {
                return Some(Signer {
                    scheme,
                    certificate,
                    past_signers,
                });
            }
        }
    }
    None
}

fn jar_signer<R: std::io::Read + std::io::Seek>(archive: &mut Archive<R>) -> Option<Signer> {
    let entry = archive
        .entries
        .iter()
        .find(|entry| {
            let name = entry.name.to_ascii_uppercase();
            name.starts_with("META-INF/")
                && !name["META-INF/".len()..].contains('/')
                && (name.ends_with(".RSA") || name.ends_with(".DSA") || name.ends_with(".EC"))
        })?
        .clone();
    let pkcs7 = archive.read(&entry, 1024 * 1024).ok()?;
    Some(Signer {
        scheme: Scheme::V1,
        certificate: pkcs7_first_certificate(&pkcs7)?,
        past_signers: Vec::new(),
    })
}

/// One DER element: its tag, its content, and the whole element with its header.
fn der(data: &[u8]) -> Option<(u8, &[u8], &[u8], &[u8])> {
    let tag = *data.first()?;
    let first = *data.get(1)?;
    let (length, header) = if first & 0x80 == 0 {
        (usize::from(first), 2)
    } else {
        let bytes = usize::from(first & 0x7f);
        if bytes == 0 || bytes > 4 {
            return None;
        }
        let mut length = 0usize;
        for i in 0..bytes {
            length = (length << 8) | usize::from(*data.get(2 + i)?);
        }
        (length, 2 + bytes)
    };
    let end = header.checked_add(length)?;
    let content = data.get(header..end)?;
    Some((tag, content, &data[..end], &data[end..]))
}

/// The first certificate of a PKCS #7 SignedData, as a JAR signature file holds it.
pub fn pkcs7_first_certificate(data: &[u8]) -> Option<Vec<u8>> {
    // ContentInfo ::= SEQUENCE { contentType OID, content [0] EXPLICIT SignedData }
    let (0x30, content_info, _, _) = der(data)? else {
        return None;
    };
    let (0x06, _, _, rest) = der(content_info)? else {
        return None;
    };
    let (0xa0, explicit, _, _) = der(rest)? else {
        return None;
    };
    // SignedData ::= SEQUENCE { version, digestAlgorithms, contentInfo,
    //                           certificates [0] IMPLICIT SET OF Certificate OPTIONAL, ... }
    let (0x30, signed_data, _, _) = der(explicit)? else {
        return None;
    };
    let (_, _, _, rest) = der(signed_data)?;
    let (_, _, _, rest) = der(rest)?;
    let (_, _, _, rest) = der(rest)?;
    let (0xa0, certificates, _, _) = der(rest)? else {
        return None;
    };
    let (0x30, _, certificate, _) = der(certificates)? else {
        return None;
    };
    Some(certificate.to_vec())
}

/// A digest as `apksigner` prints it: uppercase hex pairs joined by colons.
pub fn fingerprint(digest: &[u8]) -> String {
    digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::fixture::{archive, signing_block, v2_value};
    use std::io::Cursor;

    #[test]
    fn fr11_the_signer_is_read_from_the_apk_signing_block() {
        let certificate = b"\x30\x03\x02\x01\x07".to_vec();
        let block = signing_block(&[
            (0x4242_4242, &b"padding"[..]),
            (V2, &v2_value(&certificate)[..]),
        ]);
        let data = archive(&[("AndroidManifest.xml", &b"m"[..])], &block);
        let mut zip = Archive::open(Cursor::new(data)).unwrap();
        let signer = signer(&mut zip).expect("a signer");
        assert_eq!(signer.scheme, Scheme::V2);
        assert_eq!(signer.certificate, certificate);
        assert_eq!(signer.sha256(), Sha256::digest(&certificate).to_vec());
    }

    #[test]
    fn fr11_the_signer_of_an_apk_with_only_a_jar_signature_is_read() {
        let certificate = [0x30u8, 0x03, 0x02, 0x01, 0x07];
        // SignedData { version 1, digestAlgorithms {}, contentInfo { data }, [0] { cert } }
        let mut signed = vec![
            0x02, 0x01, 0x01, 0x31, 0x00, 0x30, 0x02, 0x06, 0x00, 0xa0, 0x05,
        ];
        signed.extend_from_slice(&certificate);
        let mut signed_data = vec![0x30, signed.len() as u8];
        signed_data.extend_from_slice(&signed);
        let mut explicit = vec![0xa0, signed_data.len() as u8];
        explicit.extend_from_slice(&signed_data);
        let mut info = vec![0x06, 0x01, 0x2a];
        info.extend_from_slice(&explicit);
        let mut pkcs7 = vec![0x30, info.len() as u8];
        pkcs7.extend_from_slice(&info);
        let data = archive(
            &[
                ("AndroidManifest.xml", &b"m"[..]),
                ("META-INF/CERT.RSA", &pkcs7[..]),
            ],
            b"",
        );
        let mut zip = Archive::open(Cursor::new(data)).unwrap();
        let signer = signer(&mut zip).expect("a signer");
        assert_eq!(signer.scheme, Scheme::V1);
        assert_eq!(signer.certificate, certificate);
    }

    #[test]
    fn fr11_an_unsigned_apk_has_no_signer() {
        let data = archive(&[("AndroidManifest.xml", &b"m"[..])], b"");
        let mut zip = Archive::open(Cursor::new(data)).unwrap();
        assert_eq!(signer(&mut zip), None);
    }

    #[test]
    fn fr11_a_fingerprint_reads_as_apksigner_prints_it() {
        assert_eq!(fingerprint(&[0x0a, 0xff, 0x10]), "0A:FF:10");
    }
}

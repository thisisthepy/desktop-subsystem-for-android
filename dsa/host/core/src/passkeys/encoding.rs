//! The encodings WebAuthn's JSON and its authenticator data use: unpadded base64url, and the
//! small part of CBOR an attestation object needs.

/// `data` in base64url without padding, as WebAuthn's JSON writes binary values.
pub fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = match chunk.len() {
            3 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            _ => (chunk[0] as u32) << 16,
        };
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Decodes base64url, with or without padding. Standard base64's `+` and `/` are taken too,
/// since some relying parties send them.
pub fn from_base64url(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // A lone character left over carries less than a byte: the text was cut.
    (bits < 6).then_some(out)
}

/// One CBOR value, as far as an attestation object and a COSE key need.
#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Unsigned(u64),
    Negative(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Bool(bool),
    Null,
}

impl Cbor {
    /// The value under `key` of a map.
    pub fn get(&self, key: &Cbor) -> Option<&Cbor> {
        match self {
            Cbor::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Cbor::Unsigned(n) => i64::try_from(*n).ok(),
            Cbor::Negative(n) => Some(*n),
            _ => None,
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

/// The deepest nesting a value may have.
const MAX_DEPTH: u32 = 16;

/// Reads one CBOR value from the start of `data`; returns it and the bytes it took.
pub fn read_cbor(data: &[u8]) -> Option<(Cbor, usize)> {
    let mut at = 0;
    let value = read_value(data, &mut at, 0)?;
    Some((value, at))
}

fn read_length(data: &[u8], at: &mut usize, info: u8) -> Option<u64> {
    let take = |at: &mut usize, n: usize| -> Option<u64> {
        let bytes = data.get(*at..*at + n)?;
        *at += n;
        Some(bytes.iter().fold(0u64, |acc, &b| acc << 8 | b as u64))
    };
    match info {
        0..=23 => Some(info as u64),
        24 => take(at, 1),
        25 => take(at, 2),
        26 => take(at, 4),
        27 => take(at, 8),
        _ => None,
    }
}

fn read_value(data: &[u8], at: &mut usize, depth: u32) -> Option<Cbor> {
    if depth > MAX_DEPTH {
        return None;
    }
    let first = *data.get(*at)?;
    *at += 1;
    let (major, info) = (first >> 5, first & 31);
    if major == 7 {
        return match info {
            20 => Some(Cbor::Bool(false)),
            21 => Some(Cbor::Bool(true)),
            22 => Some(Cbor::Null),
            _ => None,
        };
    }
    let n = read_length(data, at, info)?;
    let len = || usize::try_from(n).ok().filter(|&n| n <= data.len());
    match major {
        0 => Some(Cbor::Unsigned(n)),
        1 => Some(Cbor::Negative(-1 - i64::try_from(n).ok()?)),
        2 | 3 => {
            let len = len()?;
            let bytes = data.get(*at..*at + len)?.to_vec();
            *at += len;
            if major == 2 {
                Some(Cbor::Bytes(bytes))
            } else {
                String::from_utf8(bytes).ok().map(Cbor::Text)
            }
        }
        4 => {
            let len = len()?;
            let mut items = Vec::with_capacity(len.min(64));
            for _ in 0..len {
                items.push(read_value(data, at, depth + 1)?);
            }
            Some(Cbor::Array(items))
        }
        5 => {
            let len = len()?;
            let mut entries = Vec::with_capacity(len.min(64));
            for _ in 0..len {
                let key = read_value(data, at, depth + 1)?;
                let value = read_value(data, at, depth + 1)?;
                entries.push((key, value));
            }
            Some(Cbor::Map(entries))
        }
        _ => None,
    }
}

/// What a new passkey's authenticator data says of it.
#[derive(Debug, Clone, PartialEq)]
pub struct AttestedCredential {
    pub authenticator_data: Vec<u8>,
    pub credential_id: Vec<u8>,
    /// The COSE algorithm of its public key.
    pub algorithm: i64,
    /// Its public key as a DER SubjectPublicKeyInfo, for the algorithms that have one here
    /// (ES256, RS256, EdDSA); `None` for another.
    pub public_key_der: Option<Vec<u8>>,
}

/// Reads the authenticator data of an attestation object, and the credential it attests.
pub fn attested_credential(attestation_object: &[u8]) -> Option<AttestedCredential> {
    let (object, _) = read_cbor(attestation_object)?;
    let auth_data = object
        .get(&Cbor::Text("authData".into()))?
        .bytes()?
        .to_vec();
    // rpIdHash (32), flags (1), signCount (4), then the attested credential data.
    let flags = *auth_data.get(32)?;
    if flags & 0x40 == 0 {
        return None;
    }
    let mut at = 37 + 16; // after the AAGUID
    let id_len = u16::from_be_bytes(auth_data.get(at..at + 2)?.try_into().ok()?) as usize;
    at += 2;
    let credential_id = auth_data.get(at..at + id_len)?.to_vec();
    at += id_len;
    let (key, _) = read_cbor(auth_data.get(at..)?)?;
    let algorithm = key.get(&Cbor::Unsigned(3))?.int()?;
    let public_key_der = spki(&key, algorithm);
    Some(AttestedCredential {
        authenticator_data: auth_data,
        credential_id,
        algorithm,
        public_key_der,
    })
}

fn der_length(len: usize, out: &mut Vec<u8>) {
    if len < 128 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = (len as u32)
            .to_be_bytes()
            .into_iter()
            .skip_while(|&b| b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    der_length(content.len(), &mut out);
    out.extend_from_slice(content);
    out
}

fn der_unsigned(value: &[u8]) -> Vec<u8> {
    let trimmed: Vec<u8> = value.iter().copied().skip_while(|&b| b == 0).collect();
    let mut content = Vec::with_capacity(trimmed.len() + 1);
    if trimmed.first().is_none_or(|&b| b & 0x80 != 0) {
        content.push(0);
    }
    content.extend(trimmed);
    der(0x02, &content)
}

/// The SubjectPublicKeyInfo of a COSE key.
fn spki(key: &Cbor, algorithm: i64) -> Option<Vec<u8>> {
    let kty = key.get(&Cbor::Unsigned(1))?.int()?;
    match (kty, algorithm) {
        // EC2, ES256 on P-256.
        (2, -7) => {
            let x = key.get(&Cbor::Negative(-2))?.bytes()?;
            let y = key.get(&Cbor::Negative(-3))?.bytes()?;
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            // SEQUENCE { SEQUENCE { id-ecPublicKey, prime256v1 }, BIT STRING 04||x||y }
            let algorithm = [
                0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
                0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
            ];
            let mut point = vec![0x00, 0x04];
            point.extend_from_slice(x);
            point.extend_from_slice(y);
            let mut content = algorithm.to_vec();
            content.extend(der(0x03, &point));
            Some(der(0x30, &content))
        }
        // RSA, RS256.
        (3, -257) => {
            let n = key.get(&Cbor::Negative(-1))?.bytes()?;
            let e = key.get(&Cbor::Negative(-2))?.bytes()?;
            let mut rsa = der_unsigned(n);
            rsa.extend(der_unsigned(e));
            let rsa = der(0x30, &rsa);
            let algorithm = [
                0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05,
                0x00,
            ];
            let mut bits = vec![0x00];
            bits.extend(rsa);
            let mut content = algorithm.to_vec();
            content.extend(der(0x03, &bits));
            Some(der(0x30, &content))
        }
        // OKP, EdDSA on Ed25519.
        (1, -8) => {
            let x = key.get(&Cbor::Negative(-2))?.bytes()?;
            if x.len() != 32 {
                return None;
            }
            let algorithm = [0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70];
            let mut bits = vec![0x00];
            bits.extend_from_slice(x);
            let mut content = algorithm.to_vec();
            content.extend(der(0x03, &bits));
            Some(der(0x30, &content))
        }
        _ => None,
    }
}

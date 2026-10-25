//! The boot secret that tells the guest's own control daemon apart from any other process
//! in the guest.
//!
//! Every app in the guest can open a vsock connection to the host, because the graphics
//! path needs that. The host therefore makes a new secret for each boot and hands it to the
//! guest's kernel as a bootconfig entry, which apps cannot read. On each connection the host
//! sends a fresh nonce, and the guest answers with HMAC-SHA256 of it keyed with the secret.
//! The secret itself never crosses the connection, and an answer seen once is of no use
//! again.

use std::fmt;

use sha2::{Digest, Sha256};

/// The length of a boot secret, of a nonce and of a proof, in bytes.
pub const SECRET_LEN: usize = 32;

/// The bootconfig key the host puts the secret under. It must not start with
/// `androidboot.`: Android copies those into system properties, which every app can read.
pub const BOOTCONFIG_KEY: &str = "dsa.control.secret";

/// One boot's secret.
#[derive(Clone, PartialEq, Eq)]
pub struct BootSecret([u8; SECRET_LEN]);

impl BootSecret {
    /// A new random secret from the operating system's generator.
    pub fn generate() -> Result<BootSecret, String> {
        random().map(BootSecret)
    }

    pub fn from_bytes(bytes: [u8; SECRET_LEN]) -> BootSecret {
        BootSecret(bytes)
    }

    /// Reads the lower-case or upper-case hexadecimal form of a secret.
    pub fn from_hex(text: &str) -> Option<BootSecret> {
        let text = text.trim();
        if text.len() != SECRET_LEN * 2 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; SECRET_LEN];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(BootSecret(bytes))
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// The answer to a challenge: HMAC-SHA256 of `nonce`, keyed with this secret.
    pub fn proof(&self, nonce: &[u8]) -> [u8; SECRET_LEN] {
        hmac_sha256(&self.0, nonce)
    }

    /// Whether `mac` answers `nonce` for this secret. The comparison takes the same time
    /// however many bytes match, so timing tells a guesser nothing.
    pub fn verify(&self, nonce: &[u8], mac: &[u8]) -> bool {
        let expected = self.proof(nonce);
        mac.len() == expected.len()
            && expected
                .iter()
                .zip(mac)
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0
    }
}

impl fmt::Debug for BootSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BootSecret(..)")
    }
}

/// A fresh random nonce for one challenge.
pub fn nonce() -> Result<[u8; SECRET_LEN], String> {
    random()
}

fn random() -> Result<[u8; SECRET_LEN], String> {
    let mut bytes = [0u8; SECRET_LEN];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("the system's random number generator failed: {error}"))?;
    Ok(bytes)
}

/// HMAC-SHA256 (RFC 2104) of `data` keyed with `key`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Lower-case hexadecimal of `bytes`.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bootconfig line that carries `secret`, newline included.
pub fn bootconfig_entry(secret: &BootSecret) -> String {
    format!("{BOOTCONFIG_KEY} = \"{}\"\n", secret.to_hex())
}

/// Finds the secret in the text of the guest's `/proc/bootconfig`, which lists one
/// `key = "value"` per line.
pub fn secret_from_bootconfig(text: &str) -> Option<BootSecret> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != BOOTCONFIG_KEY {
            return None;
        }
        BootSecret::from_hex(value.trim().trim_matches('"'))
    })
}

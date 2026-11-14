//! The root key of the guest's KeyMint, which the host gives once each boot.
//!
//! KeyMint wraps every key it makes (the apps' Keystore keys and the keys that encrypt
//! Android's own storage) under a root key. DSA's KeyMint takes that key from the host, which
//! keeps it sealed by the computer's TPM (or Secure Enclave), and by the user's credential
//! store only where there is none, so a copy of the guest's disk alone opens none of those
//! keys.
//!
//! `dsa-channeld --keymint-key` runs once, early in boot, before KeyMint is needed. It
//! connects to the host's KeyMint port ([`PORT`]), passes the same handshake as the control
//! channel with the boot's secret, asks with `KeyMintRootKeyRequest` and reads
//! `KeyMintRootKey`. It writes what it got to [`FILE_NAME`] in a tmpfs folder init makes for
//! it ([`DIR`]), which only KeyMint may read, and exits. The key lives in memory for the
//! boot: never on the user's disk, and only KeyMint's domain can read the file.
//!
//! The file is one line: `key <64 hex digits>` or `none <why>` when the host has no key to
//! give, and KeyMint then keeps the keys it made without one. It is written whole (a new
//! file renamed over the old), so KeyMint never reads half of it.
//!
//! The host may not listen yet when the guest starts, so the daemon tries again for a while
//! ([`Retry`]) and then writes `none` with the reason, so KeyMint never waits for a file that
//! does not come. A host that refuses this guest, or answers with something that is not a
//! key, is not asked again.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Hello};
use dsa_protocol::handshake::{guest_connect, HandshakeError};
use dsa_transport::{Connection, Connector, TransportError};

/// The host's vsock port for KeyMint's root key; the host serves it on the same number.
pub const PORT: u32 = 5604;

/// The tmpfs folder init makes for the key, readable only by KeyMint.
pub const DIR: &str = "/dev/dsa_keymint";

/// The file in [`DIR`] KeyMint reads.
pub const FILE_NAME: &str = "root_key";

/// The file being written, renamed over [`FILE_NAME`] once whole.
const NEW_FILE_NAME: &str = ".root_key.new";

/// The length of the root key.
pub const KEY_LEN: usize = 32;

/// The longest reason kept in the file.
const MAX_REASON_CHARS: usize = 512;

/// What the host gave this boot.
#[derive(Clone, PartialEq, Eq)]
pub enum RootKey {
    Key([u8; KEY_LEN]),
    /// No key, and why.
    None(String),
}

impl std::fmt::Debug for RootKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The key itself never goes into a log line.
            RootKey::Key(_) => f.write_str("RootKey::Key(..)"),
            RootKey::None(why) => f.debug_tuple("RootKey::None").field(why).finish(),
        }
    }
}

impl RootKey {
    /// The file's text: `key <hex>` or `none <reason>`, one line. The reason is cut to one
    /// line of printable text, so the file always has exactly one line.
    pub fn file_text(&self) -> String {
        match self {
            RootKey::Key(key) => {
                let mut text = String::with_capacity(5 + 2 * KEY_LEN);
                text.push_str("key ");
                for byte in key {
                    let _ = write!(text, "{byte:02x}");
                }
                text.push('\n');
                text
            }
            RootKey::None(why) => {
                let mut reason: String = why
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .take(MAX_REASON_CHARS)
                    .collect::<String>()
                    .trim()
                    .to_string();
                if reason.is_empty() {
                    reason = "the host gave no reason".into();
                }
                format!("none {reason}\n")
            }
        }
    }

    /// Reads a file's text as [`RootKey::file_text`] writes it; `None` for anything else.
    pub fn parse(text: &str) -> Option<RootKey> {
        let line = text.strip_suffix('\n')?;
        if line.contains('\n') {
            return None;
        }
        if let Some(hex) = line.strip_prefix("key ") {
            if hex.len() != 2 * KEY_LEN {
                return None;
            }
            let mut key = [0u8; KEY_LEN];
            for (i, byte) in key.iter_mut().enumerate() {
                let digits = hex.get(2 * i..2 * i + 2)?;
                if !digits
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return None;
                }
                *byte = u8::from_str_radix(digits, 16).ok()?;
            }
            return Some(RootKey::Key(key));
        }
        line.strip_prefix("none ")
            .map(|why| RootKey::None(why.to_string()))
    }
}

/// Why one attempt to get the key failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    /// The host could not be reached, or did not answer: it may yet.
    Again(String),
    /// The host refused this guest, or its answer is not a key: asking again changes nothing.
    Final(String),
}

/// Asks for the key on `connection`: passes the handshake with `secret`, sends the request
/// and reads the answer, each within `wait`.
pub fn ask(
    connection: &mut dyn Connection,
    hello: &Hello,
    secret: &BootSecret,
    wait: Duration,
) -> Result<RootKey, AskError> {
    match guest_connect(connection, hello, secret, wait) {
        Ok(_) => {}
        Err(HandshakeError::Transport(error)) => {
            return Err(AskError::Again(format!(
                "the handshake with the host broke off ({error})"
            )))
        }
        Err(error) => {
            return Err(AskError::Final(format!(
                "the host refused this guest's handshake ({error})"
            )))
        }
    }
    let request = dsa_protocol::encode(&Envelope {
        body: Some(Body::KeymintRootKeyRequest(
            control::KeyMintRootKeyRequest {},
        )),
    });
    connection
        .send(&request)
        .map_err(|e| AskError::Again(format!("the request did not reach the host ({e})")))?;
    let frame = match connection.recv(Some(wait)) {
        Ok(frame) => frame,
        Err(error @ (TransportError::Timeout | TransportError::Closed)) => {
            // A host that has given this boot its key already closes without answering.
            return Err(AskError::Again(format!(
                "the host did not answer ({error}); it gives the key once per boot"
            )));
        }
        Err(error) => return Err(AskError::Again(format!("the answer was lost ({error})"))),
    };
    match dsa_protocol::decode(&frame) {
        Ok(Envelope {
            body: Some(Body::KeymintRootKey(answer)),
        }) => {
            if answer.key.is_empty() {
                return Ok(RootKey::None(if answer.message.is_empty() {
                    "the host has no key for KeyMint".to_string()
                } else {
                    answer.message
                }));
            }
            let key: [u8; KEY_LEN] = answer.key.as_slice().try_into().map_err(|_| {
                AskError::Final(format!(
                    "the host's key is {} bytes, not {KEY_LEN}",
                    answer.key.len()
                ))
            })?;
            Ok(RootKey::Key(key))
        }
        Ok(_) => Err(AskError::Final(
            "the host answered with something other than KeyMint's root key".into(),
        )),
        Err(error) => Err(AskError::Final(format!(
            "the host's answer cannot be read ({error})"
        ))),
    }
}

/// How long and how often the daemon tries to reach the host.
#[derive(Debug, Clone, Copy)]
pub struct Retry {
    /// After this long, it gives up and writes `none`. KeyMint waits a little longer.
    pub give_up_after: Duration,
    /// The pause between attempts.
    pub every: Duration,
    /// The longest one step of an attempt (connecting, the handshake, the answer) may take.
    pub attempt: Duration,
}

impl Default for Retry {
    fn default() -> Retry {
        Retry {
            give_up_after: Duration::from_secs(25),
            every: Duration::from_millis(500),
            attempt: Duration::from_secs(5),
        }
    }
}

/// Gets the key through `connector`, trying again as `retry` says. Never fails: when there is
/// no key to be had, the answer is [`RootKey::None`] with the reason. `say` gets one line for
/// each attempt that failed.
pub fn fetch(
    connector: &mut dyn Connector,
    hello: &Hello,
    secret: &BootSecret,
    retry: &Retry,
    say: &dyn Fn(&str),
) -> RootKey {
    let start = Instant::now();
    let mut attempts = 0u32;
    let mut last = String::from("the host was not tried");
    loop {
        let left = retry.give_up_after.saturating_sub(start.elapsed());
        if left.is_zero() {
            return RootKey::None(format!(
                "the host could not be reached in {} seconds ({attempts} attempts; last: {last})",
                retry.give_up_after.as_secs()
            ));
        }
        attempts += 1;
        let wait = retry.attempt.min(left);
        let outcome = connector
            .connect(wait)
            .map_err(|e| AskError::Again(format!("the host's KeyMint port is not open ({e})")))
            .and_then(|mut connection| {
                let answer = ask(connection.as_mut(), hello, secret, wait);
                connection.close();
                answer
            });
        match outcome {
            Ok(key) => return key,
            Err(AskError::Final(why)) => return RootKey::None(why),
            Err(AskError::Again(why)) => {
                // Said once, not on every attempt of a host that is still starting.
                if attempts == 1 {
                    say(&format!("KeyMint's root key: {why}; trying again"));
                }
                last = why;
            }
        }
        let left = retry.give_up_after.saturating_sub(start.elapsed());
        std::thread::sleep(retry.every.min(left));
    }
}

/// Writes `key` to [`FILE_NAME`] in `dir`, readable only by the file's owner (the user the
/// daemon runs as, KeyMint's), replacing what was there in one rename.
pub fn store(dir: &Path, key: &RootKey) -> std::io::Result<()> {
    let new = dir.join(NEW_FILE_NAME);
    let _ = std::fs::remove_file(&new);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o400);
    }
    let mut text = key.file_text().into_bytes();
    let written = options.open(&new).and_then(|mut file| {
        file.write_all(&text)?;
        file.sync_all()
    });
    // The key's hex goes no further than the file.
    text.fill(0);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&new);
        return Err(error);
    }
    std::fs::rename(&new, dir.join(FILE_NAME)).inspect_err(|_| {
        let _ = std::fs::remove_file(&new);
    })
}

/// The whole of `--keymint-key`: gets the key (or why there is none) and stores it in `dir`.
/// 0 once the file is written, whatever it says; 1 when it cannot be.
pub fn run(
    connector: &mut dyn Connector,
    hello: &Hello,
    secret: Option<&BootSecret>,
    dir: &Path,
    retry: &Retry,
    say: &dyn Fn(&str),
) -> i32 {
    let key = match secret {
        Some(secret) => fetch(connector, hello, secret, retry, say),
        None => RootKey::None(
            "the boot has no secret, so the host cannot be asked; the host did not start it".into(),
        ),
    };
    match &key {
        RootKey::Key(_) => say("KeyMint has its root key from the host"),
        RootKey::None(why) => say(&format!(
            "KeyMint has no root key from the host, and keeps its keys as they are: {why}"
        )),
    }
    match store(dir, &key) {
        Ok(()) => 0,
        Err(error) => {
            say(&format!(
                "KeyMint's root key cannot be written to {}: {error}",
                dir.join(FILE_NAME).display()
            ));
            1
        }
    }
}

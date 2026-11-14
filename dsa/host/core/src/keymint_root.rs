//! The root key of the guest's KeyMint, kept by the host.
//!
//! Android's KeyMint wraps every key it makes (the keys of apps' Keystore, and the keys that
//! encrypt Android's own storage) under a root key. The image's KeyMint, a software one, has
//! that root key built in: the same for every copy, so a copy of a guest's disk would give up
//! every key on it. DSA's KeyMint takes its root key from the host instead. The host seals it
//! with the computer's own hardware where it has some that works (the TPM, the Secure
//! Enclave), and with the user's credential store only where it has none
//! ([`crate::host_keys`]), so it opens only for that user on that computer. The guest's disk never
//! holds it, and neither does any file the host writes in the clear.
//!
//! Each boot, before Android unlocks its storage, the guest's control daemon connects to
//! [`KEYMINT_PORT`], passes the same handshake as the control channel (the boot's secret,
//! which only the daemon can read), asks with [`control::KeyMintRootKeyRequest`] and gets
//! [`control::KeyMintRootKey`]. It hands the key to KeyMint in memory only. The host gives it
//! once per boot: a second request in the same boot is refused, since the guest's KeyMint asks
//! only once and anything else that asks is not it.
//!
//! The key is made the first time a user's Android starts, and an install that ran before has
//! keys wrapped under the built-in root key. DSA's KeyMint keeps reading those, and asks
//! Android to upgrade each one as it is used, which wraps it again under the host's key; no
//! key is lost on the way. When the host has no credential store (or it is off), the key comes
//! empty with the reason, and KeyMint goes on with its built-in root key until it has one.
//! A kept key that cannot be opened (the user's account or computer changed, the TPM was
//! cleared) is never replaced by a new one, which would make every key wrapped under it
//! unreadable for good; the reason is shown, and a data reset, which leaves nothing wrapped
//! under it, lets it go.

use std::sync::Arc;
use std::time::Duration;

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope};
use dsa_transport::Connection;

use crate::host_keys::{load_or_create, HostKeyStore, KeyError, KeyPurpose, Secret};

/// The vsock port the guest's control daemon asks for KeyMint's root key on.
pub const KEYMINT_PORT: u32 = 5604;

/// How long a connection on the port may take for the handshake and the request.
pub const KEYMINT_WAIT: Duration = Duration::from_secs(10);

/// The root key for this boot, or why there is none.
pub type RootKey = Result<Secret, String>;

/// The root key the user's credential store keeps, made on first use.
pub fn root_key(store: &Arc<dyn HostKeyStore>) -> RootKey {
    load_or_create(store.as_ref(), KeyPurpose::KeyMintRoot).map_err(|e| root_key_problem(&e))
}

/// What a person reads when the KeyMint root key cannot be had, with what to do.
pub fn root_key_problem(error: &KeyError) -> String {
    match error {
        KeyError::Unavailable(why) => format!(
            "the KeyMint root key cannot be had now ({why}); until it can, Android's keys made \
             under it do not open, and new ones are made under KeyMint's built-in root key"
        ),
        KeyError::Unreadable(why) => format!(
            "the KeyMint root key kept for this user cannot be opened here ({why}), so \
             Android's keys and its encrypted storage cannot be read on this computer; if what \
             sealed it cannot come back, reset Android's data (dsa reset-data) to start over \
             with a new key, and restore the apps' data from a backup if backup was on"
        ),
        KeyError::Failed(why) => format!("the KeyMint root key cannot be had: {why}"),
    }
}

/// Serves one connection on the KeyMint port, past the handshake: waits for the request and
/// answers it with `key`. `Err` says what the connection did wrong.
pub fn serve_root_key(connection: &mut dyn Connection, key: &RootKey) -> Result<(), String> {
    let frame = connection
        .recv(Some(KEYMINT_WAIT))
        .map_err(|e| format!("no request came: {e}"))?;
    match dsa_protocol::decode(&frame) {
        Ok(Envelope {
            body: Some(Body::KeymintRootKeyRequest(_)),
        }) => {}
        Ok(_) => return Err("it sent something other than a request for the root key".into()),
        Err(e) => return Err(format!("it sent an unreadable frame ({e})")),
    }
    let answer = match key {
        Ok(secret) => control::KeyMintRootKey {
            key: secret.bytes().to_vec(),
            message: String::new(),
        },
        Err(why) => control::KeyMintRootKey {
            key: Vec::new(),
            message: why.clone(),
        },
    };
    let frame = dsa_protocol::encode(&Envelope {
        body: Some(Body::KeymintRootKey(answer)),
    });
    connection
        .send(&frame)
        .map_err(|e| format!("the key did not go out: {e}"))?;
    Ok(())
}

/// The guest's side, for the control daemon and for tests: connects through `connection`,
/// passes the handshake with `secret` and returns the root key, or `None` with the host's
/// reason when it has none.
pub fn ask_root_key(
    connection: &mut dyn Connection,
    hello: &control::Hello,
    secret: &BootSecret,
) -> Result<Result<Vec<u8>, String>, String> {
    dsa_protocol::handshake::guest_connect(connection, hello, secret, KEYMINT_WAIT)
        .map_err(|e| format!("the host refused the handshake: {e}"))?;
    let request = dsa_protocol::encode(&Envelope {
        body: Some(Body::KeymintRootKeyRequest(
            control::KeyMintRootKeyRequest {},
        )),
    });
    connection
        .send(&request)
        .map_err(|e| format!("the request did not go out: {e}"))?;
    let frame = connection
        .recv(Some(KEYMINT_WAIT))
        .map_err(|e| format!("the host did not answer: {e}"))?;
    match dsa_protocol::decode(&frame) {
        Ok(Envelope {
            body: Some(Body::KeymintRootKey(answer)),
        }) if answer.key.len() == crate::host_keys::SECRET_LEN => Ok(Ok(answer.key)),
        Ok(Envelope {
            body: Some(Body::KeymintRootKey(answer)),
        }) if answer.key.is_empty() => Ok(Err(answer.message)),
        Ok(_) => Err("the host answered with something other than the root key".into()),
        Err(e) => Err(format!("the host's answer cannot be read ({e})")),
    }
}

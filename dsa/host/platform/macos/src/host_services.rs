//! The system services the user's session serves their Android on macOS: app data backups in
//! `~/DSA Backup`, the passkey check, and the root key of the guest's KeyMint.
//!
//! - Backup is off until the user turns it on; the choice is kept in the user's DSA settings
//!   folder (`users/<user>.backup.json`), and until then neither `~/DSA Backup` nor the backup
//!   key is made.
//! - The backup key is kept in the user's login keychain ([`crate::keychain`]).
//! - The KeyMint root key is sealed by the Mac's Secure Enclave ([`crate::secure_enclave`]) in
//!   a file of the user's DSA folder (`keys/`), and kept by the login keychain only where the
//!   Secure Enclave cannot be used (a Mac without one, a copy of DSA not signed for it), with
//!   the reason; once it can, the next start seals the same key with it and removes it from the
//!   keychain.
//! - The agent runs as the user, so the backup folder is the user's own; it is kept to them
//!   alone (mode `0700`).
//! - A passkey request is checked (the app's signature against the site's Digital Asset
//!   Links, read with `/usr/bin/curl`, no redirect followed) and handed to the user's device
//!   lender (`dsa-devices`), which performs it with the Mac's passkeys
//!   (`crate::devices::passkeys`). With no device lender running, it is unavailable.
//! - The KeyMint root key is given once per boot on its own vsock port, which the VM listens
//!   on with the others ([`listen_ports`]).
//! - The guest's DNS queries come on a vsock port of their own, which the VM listens on too, and
//!   are answered by the system's resolver ([`dsa_host_unix::dns_resolver`]): mDNSResponder,
//!   with the hosts file, a VPN's scoped resolvers and encrypted DNS.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dsa_host_core::backup::{folder_in, BackupService, BackupSwitch, FileSwitch, MemorySwitch};
use dsa_host_core::dns::DNS_PORT;
use dsa_host_core::host_keys::{HardwareOrStore, HostKeyStore, SealedKeys, StoredSealer};
use dsa_host_core::keymint_root::KEYMINT_PORT;
use dsa_host_core::passkeys::PasskeyService;
use dsa_host_core::session::Session;
use dsa_host_unix::backup_folder::own_rights;
use dsa_host_unix::https_get::CurlHttpsGet;
use dsa_transport::Listener;

use crate::keychain::KeychainKeys;
use crate::secure_enclave::SecureEnclaveSealer;

/// The vsock ports of these services the VM listens on.
pub fn listen_ports() -> [u32; 2] {
    [KEYMINT_PORT, DNS_PORT]
}

/// What a checked passkey request is told when no device lender performs it.
pub const NO_DEVICE_LENDER: &str = "this Mac's passkeys are used through DSA's device lender \
     (dsa-devices), which is not running; sign in to the Mac's desktop, or start DSA again";

/// What a person reads of the login keychain as a sealer.
pub const KEYCHAIN: &str = "your login keychain";

/// The folder the user's sealed keys are kept in, beside their settings folder.
pub fn keys_dir(settings_dir: &Path) -> PathBuf {
    settings_dir.with_file_name("keys")
}

/// Gives `session` the user's backup, the passkey check and KeyMint's root key. `home` is the
/// user's home folder; `keymint` the listener of the KeyMint port; `build` names this host in
/// that port's handshake; `settings_dir` is the user's settings folder and `user` their
/// identity there, which keep the backup choice.
pub fn serve(
    session: &Session,
    home: &Path,
    keymint: Box<dyn Listener>,
    build: &str,
    settings_dir: &Path,
    user: &str,
) {
    let keychain: Arc<dyn HostKeyStore> = Arc::new(KeychainKeys);

    let folder = folder_in(home);
    let computer = crate::computer_identity::read().computer_name;
    let switch: Arc<dyn BackupSwitch> = match FileSwitch::for_user(settings_dir, user) {
        Ok(switch) => Arc::new(switch),
        Err(why) => {
            eprintln!("backup: the choice cannot be kept, so it is off: {why}");
            Arc::new(MemorySwitch::default())
        }
    };
    let backup = BackupService::new(folder.clone(), Arc::clone(&keychain), &computer)
        .with_file_access(own_rights(folder))
        .with_switch(switch);
    session.serve_backup(Arc::new(backup));

    // The root certificates the administrator's and the user's trust settings added.
    session.serve_host_certificates(Arc::new(crate::host_certificates::MacRootStore));

    let enclave = Arc::new(SecureEnclaveSealer);
    let sealers = HardwareOrStore::new(
        enclave,
        Box::new(crate::secure_enclave::usable),
        Arc::new(StoredSealer::new("keychain", KEYCHAIN, keychain)),
    );
    session.set_keymint_keys(Arc::new(SealedKeys::new(
        keys_dir(settings_dir),
        Arc::new(sealers),
    )));
    session.serve_keymint_port(keymint, build);

    let fetch = CurlHttpsGet::new("/usr/bin/curl");
    session.serve_passkeys(Arc::new(
        PasskeyService::new(Arc::new(fetch)).with_no_authenticator_reason(NO_DEVICE_LENDER),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_the_sealed_keys_are_kept_beside_the_users_settings() {
        assert_eq!(
            keys_dir(Path::new(
                "/Users/ada/Library/Application Support/DSA/settings"
            )),
            PathBuf::from("/Users/ada/Library/Application Support/DSA/keys")
        );
    }
}

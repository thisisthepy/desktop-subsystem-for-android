//! The system services each user's session serves their Android on Linux: app data backups in
//! `~/DSA Backup`, the passkey check, and the root key of the guest's KeyMint.
//!
//! - Backup is off until the user turns it on; the choice is kept in the service's settings
//!   folder (`users/<user>.backup.json`), and until then neither `~/DSA Backup` nor the backup
//!   key is made.
//! - The backup key is kept in the user's Secret Service ([`crate::secret_service`]).
//! - The KeyMint root key is sealed by the computer's TPM 2.0 ([`crate::tpm2`]) in a file of
//!   the service's own folder (`keys/<uid>/`, root's alone), and kept by the user's Secret
//!   Service only where no TPM 2.0 works, with the reason; once one works, the next start
//!   seals the same key with it and removes it from the keyring.
//! - The service runs as root, so it uses the backup folder only with the user's own file
//!   identity, as the shared folders are served: every file the backup makes is the user's,
//!   nothing reaches past what the user could, and a link the user puts in their folder can
//!   send nothing elsewhere. The folder itself is kept to its owner alone (mode `0700`).
//! - Linux has no passkey store a program can use, so no desktop session watches for passkey
//!   ceremonies. A request is still checked (the app's signature against the site's Digital
//!   Asset Links) and then answered as unavailable, with that reason.
//! - The KeyMint root key is given once per boot on its own vsock port.
//! - The guest's DNS queries come on a vsock port of their own and are answered by the
//!   system's resolver ([`dsa_host_unix::dns_resolver`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dsa_host_core::backup::{
    folder_in, BackupService, BackupSwitch, FileAccess, FileSwitch, MemorySwitch,
};
use dsa_host_core::dns::DNS_PORT;
use dsa_host_core::file_share::ShareHost;
use dsa_host_core::host_keys::{HardwareOrStore, HostKeyStore, SealedKeys, StoredSealer};
use dsa_host_core::keymint_root::KEYMINT_PORT;
use dsa_host_core::passkeys::PasskeyService;
use dsa_host_core::session::Session;
use dsa_host_unix::backup_folder::keep_private;
use dsa_host_unix::dns_resolver::SystemResolver;
use dsa_host_unix::https_get::CurlHttpsGet;

use crate::devices::file_share::UserFolders;
use crate::secret_service::SecretServiceKeys;
use crate::users::Account;
use crate::vsock::VsockHub;

/// What an app that asks for a passkey is told on Linux.
pub const NO_PASSKEYS: &str = "Linux has no passkey store DSA can use, so apps cannot make or \
     use passkeys on this computer; sign in another way, such as with a password";

/// Uses `folder` with the file identity of `folders`' user, and keeps it theirs alone.
fn file_access(folders: UserFolders, folder: std::path::PathBuf) -> FileAccess {
    Arc::new(move |run: &mut dyn FnMut()| {
        folders.as_user(&mut || {
            run();
            keep_private(&folder);
        })
    })
}

/// The folder the service keeps the sealed keys of the user with `uid` in, beside its
/// settings folder `settings_dir`.
pub fn keys_dir(settings_dir: &Path, uid: u32) -> PathBuf {
    settings_dir.with_file_name("keys").join(uid.to_string())
}

/// What a person reads of the user's keyring as a sealer.
pub const KEYRING: &str = "your Secret Service keyring (GNOME Keyring, KWallet or KeePassXC)";

/// Whether the computer's TPM 2.0 can seal now.
fn tpm_usable() -> Result<(), String> {
    let version = std::fs::read_to_string(crate::tpm2::VERSION_FILE).ok();
    crate::tpm2::usable(version.as_deref(), Path::new(crate::tpm2::DEVICE).exists())?;
    let mut tpm =
        crate::tpm2::open_device().map_err(|why| format!("the TPM cannot be reached ({why})"))?;
    crate::tpm2::probe(tpm.as_mut()).map_err(|e| crate::tpm2::unusable_reason(&e))
}

/// The KeyMint root key's store of the user with `uid`: the TPM 2.0 first, their keyring
/// where none works.
fn keymint_keys(settings_dir: &Path, uid: u32, keyring: Arc<dyn HostKeyStore>) -> SealedKeys {
    let dir = keys_dir(settings_dir, uid);
    // Root's alone: the user's guest is given its key, and nobody else reads the file.
    if let Err(error) = std::fs::create_dir_all(&dir).and_then(|()| {
        use std::os::unix::fs::PermissionsExt;
        for folder in [dir.parent().unwrap_or(&dir), dir.as_path()] {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }) {
        eprintln!(
            "keys: {} cannot be kept to root alone: {error}",
            dir.display()
        );
    }
    let tpm = Arc::new(crate::tpm2::Tpm2Sealer::new(Box::new(
        crate::tpm2::open_device,
    )));
    let sealers = HardwareOrStore::new(
        tpm,
        Box::new(tpm_usable),
        Arc::new(StoredSealer::new("secret-service", KEYRING, keyring)),
    );
    SealedKeys::new(dir, Arc::new(sealers))
}

/// Gives `session` the user's backup, passkey check and KeyMint root key. `cid` is their
/// guest's vsock address; `build` names this host in the KeyMint port's handshake;
/// `settings_dir` is the service's settings folder, where the backup choice is kept.
pub fn serve(
    session: &Session,
    account: &Account,
    cid: u32,
    build: &str,
    user: &str,
    settings_dir: &Path,
) -> Result<(), String> {
    let keyring: Arc<dyn HostKeyStore> = Arc::new(SecretServiceKeys::new(account.clone()));

    let folder = folder_in(&account.home);
    let folders =
        UserFolders::for_uid(account.uid).map_err(|e| format!("the account of {user}: {e}"))?;
    let computer = crate::computer_identity::read().computer_name;
    let switch: Arc<dyn BackupSwitch> = match FileSwitch::for_user(settings_dir, user) {
        Ok(switch) => Arc::new(switch),
        Err(why) => {
            eprintln!("{user}: backup: the choice cannot be kept, so it is off: {why}");
            Arc::new(MemorySwitch::default())
        }
    };
    let backup = BackupService::new(folder.clone(), Arc::clone(&keyring), &computer)
        .with_file_access(file_access(folders, folder))
        .with_switch(switch);
    session.serve_backup(Arc::new(backup));

    // The root certificates an administrator added to the system's store.
    session.serve_host_certificates(Arc::new(crate::host_certificates::LinuxRootStore::system()));

    session.set_keymint_keys(Arc::new(keymint_keys(settings_dir, account.uid, keyring)));
    let listener = VsockHub::shared(KEYMINT_PORT)
        .map_err(|e| format!("vsock port {KEYMINT_PORT}: {e}"))?
        .listener(cid, false);
    session.serve_keymint_port(Box::new(listener), build);

    // The guest's DNS queries, answered by the system's resolver, so the hosts file,
    // systemd-resolved's per-link DNS (a VPN's split DNS) and DNS filters apply to Android's apps.
    let dns = VsockHub::shared(DNS_PORT)
        .map_err(|e| format!("vsock port {DNS_PORT}: {e}"))?
        .listener(cid, false);
    session.serve_dns_port(Box::new(dns), Arc::new(SystemResolver), build);

    // The asset links are read with the user's rights, not root's.
    let curl = crate::network::find_program("curl")
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/bin/curl"));
    let reader = account.clone();
    let fetch = CurlHttpsGet::new(curl).with_prepare(Box::new(move |command| {
        reader.run_as(command, None);
    }));
    session.serve_passkeys(Arc::new(
        PasskeyService::new(Arc::new(fetch)).with_no_authenticator_reason(NO_PASSKEYS),
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_each_users_sealed_keys_are_kept_beside_the_services_settings() {
        assert_eq!(
            keys_dir(Path::new("/var/lib/dsa/settings"), 1000),
            PathBuf::from("/var/lib/dsa/keys/1000")
        );
    }

    #[test]
    fn fr36_a_passkey_request_on_linux_says_why_it_is_unavailable() {
        assert!(NO_PASSKEYS.contains("Linux has no passkey store"));
        assert!(!NO_PASSKEYS.contains("Windows"));
    }
}

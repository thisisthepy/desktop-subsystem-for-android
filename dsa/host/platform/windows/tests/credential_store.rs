//! The backup and KeyMint root keys sealed by the computer's TPM and DPAPI, as the user this
//! test runs as (Windows only).
#![cfg(windows)]

use dsa_host_core::host_keys::{load_or_create, HostKeyStore, KeyError, KeyPurpose, Sealer};
use dsa_host_windows::credential_store::{
    for_user, sealer_of, DpapiSealer, TpmDpapiSealer, DPAPI, TPM12_DPAPI, TPM_DPAPI,
};
use dsa_host_windows::pipe::current_user_sid;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("keys {name} {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn fr36_dpapi_opens_what_it_sealed_only_for_the_same_purpose() {
    let sealer = DpapiSealer::new(&current_user_sid().unwrap());
    let sealed = sealer.seal(&[3u8; 32], "backup-key").unwrap();
    assert!(
        !sealed.windows(32).any(|w| w == [3u8; 32]),
        "sealed in the clear"
    );
    assert_eq!(sealer.unseal(&sealed, "backup-key").unwrap(), vec![3u8; 32]);
    assert!(matches!(
        sealer.unseal(&sealed, "keymint-root-key"),
        Err(KeyError::Unreadable(_))
    ));
}

#[test]
fn fr36_keymint_root_key_is_sealed_by_the_host() {
    let sid = current_user_sid().unwrap();
    let dir = scratch("keymint");
    let keys = for_user(&sid, &dir);
    let key = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
    assert_eq!(
        keys.load(KeyPurpose::KeyMintRoot).unwrap(),
        Some(key.clone())
    );
    let data = std::fs::read(keys.path(KeyPurpose::KeyMintRoot)).unwrap();
    // The TPM seals it wherever the computer has one that works; DPAPI only where it has none,
    // and then the reason is told.
    let tpm = TpmDpapiSealer::check(&sid);
    let protection = keys.protection(KeyPurpose::KeyMintRoot).unwrap().unwrap();
    match &tpm {
        Ok(sealer) => {
            let name = sealer_of(&data).unwrap();
            assert!(name == TPM_DPAPI || name == TPM12_DPAPI, "{name}");
            assert_eq!(name, sealer.name());
            assert!(protection.hardware);
            assert_eq!(protection.sealed_by, "tpm");
        }
        Err(why) => {
            assert_eq!(sealer_of(&data), Some(DPAPI), "TPM check: {why}");
            assert!(!protection.hardware);
            assert_eq!(protection.sealed_by, "dpapi");
            assert!(!protection.fallback_reason.is_empty());
        }
    }
    assert!(
        !data.windows(32).any(|w| w == key.bytes()),
        "the key is on disk in the clear"
    );
    // The same file, sealed for this user, opens for no other account: DPAPI under another
    // user's credentials is refused (here, a file whose seal was changed by a byte).
    let mut tampered = data.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    std::fs::write(keys.path(KeyPurpose::KeyMintRoot), &tampered).unwrap();
    assert!(keys.load(KeyPurpose::KeyMintRoot).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

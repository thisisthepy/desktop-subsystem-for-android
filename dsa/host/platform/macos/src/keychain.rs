//! The keys DSA keeps for the user in the user's login keychain: their backup key, and their
//! Android's KeyMint root key where the Secure Enclave cannot seal it
//! ([`crate::secure_enclave`]).
//!
//! Each key is a generic password item: service `io.github.thisisthepy.DSA`, account the key's
//! name (`backup-key`, `keymint-root-key`). It is made accessible after the first unlock and
//! on this device only (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`) and never
//! synchronizable, so it is not copied to iCloud Keychain or carried to another Mac by a
//! backup restore. The agent runs as the user, so it reaches the user's own keychain directly.
//!
//! A keychain that is locked and cannot prompt, a prompt the user refused, or a Mac with no
//! keychain for the user makes the key unavailable, with the reason; an item that is there but
//! not 32 bytes long is unreadable and is never replaced.

use dsa_host_core::host_keys::{KeyError, KeyPurpose};

/// The service of every item DSA keeps.
pub const SERVICE: &str = "io.github.thisisthepy.DSA";

/// The `OSStatus` values the keychain answers with that have a meaning here.
pub const OK: i32 = 0;
pub const ITEM_NOT_FOUND: i32 = -25300;
pub const DUPLICATE_ITEM: i32 = -25299;
pub const PARAM: i32 = -50;
pub const USER_CANCELED: i32 = -128;
pub const NOT_AVAILABLE: i32 = -25291;
pub const AUTH_FAILED: i32 = -25293;
pub const NO_SUCH_KEYCHAIN: i32 = -25294;
pub const NO_DEFAULT_KEYCHAIN: i32 = -25307;
pub const INTERACTION_NOT_ALLOWED: i32 = -25308;
pub const DECODE: i32 = -26275;
pub const MISSING_ENTITLEMENT: i32 = -34018;

/// What a failed keychain call means for the key of `purpose`. `doing` says what was asked
/// ("read", "keep"), `message` what Security.framework said of the status.
pub fn status_error(status: i32, purpose: KeyPurpose, doing: &str, message: &str) -> KeyError {
    let what = purpose.description();
    let said = if message.is_empty() {
        format!("OSStatus {status}")
    } else {
        format!("{message} (OSStatus {status})")
    };
    match status {
        INTERACTION_NOT_ALLOWED => KeyError::Unavailable(format!(
            "your login keychain is locked and cannot ask to be unlocked now, so the {what} \
             cannot be {doing}; unlock it (sign in again) and try again ({said})"
        )),
        USER_CANCELED | AUTH_FAILED => KeyError::Unavailable(format!(
            "the keychain was not allowed to give DSA the {what}: its prompt was refused \
             ({said})"
        )),
        NOT_AVAILABLE | NO_SUCH_KEYCHAIN | NO_DEFAULT_KEYCHAIN => KeyError::Unavailable(format!(
            "this user has no login keychain, so the {what} cannot be {doing} ({said})"
        )),
        MISSING_ENTITLEMENT => KeyError::Unavailable(format!(
            "this copy of DSA is not signed to use the keychain, so the {what} cannot be \
             {doing} ({said})"
        )),
        DECODE => KeyError::Unreadable(format!(
            "the {what} in the keychain cannot be read here ({said})"
        )),
        _ => KeyError::Failed(format!(
            "the keychain failed to {} the {what}: {said}",
            match doing {
                "kept" => "keep",
                "removed" => "remove",
                _ => "read",
            }
        )),
    }
}

#[cfg(target_os = "macos")]
pub use mac::KeychainKeys;

#[cfg(target_os = "macos")]
mod mac {
    use dsa_host_core::host_keys::{HostKeyStore, KeyError, KeyPurpose, Secret, SECRET_LEN};

    use super::*;
    use crate::security::*;

    /// The user's keys in their login keychain.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct KeychainKeys;

    /// The attributes that name the item of `purpose`, with the strings they point into.
    fn item(purpose: KeyPurpose) -> (Cf, Cf) {
        (Cf::string(SERVICE), Cf::string(purpose.name()))
    }

    fn fail(status: i32, purpose: KeyPurpose, doing: &str) -> KeyError {
        status_error(status, purpose, doing, &status_message(status))
    }

    impl HostKeyStore for KeychainKeys {
        fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
            let (service, account) = item(purpose);
            // SAFETY: the keychain's constants are CFStrings; every value is a live object.
            let query = unsafe {
                Cf::dictionary(&[
                    (kSecClass, kSecClassGenericPassword),
                    (kSecAttrService, service.get()),
                    (kSecAttrAccount, account.get()),
                    (kSecReturnData, kCFBooleanTrue),
                    (kSecMatchLimit, kSecMatchLimitOne),
                ])
            };
            let mut result: CFTypeRef = std::ptr::null();
            // SAFETY: a query dictionary and a place for the copy the call makes.
            let status = unsafe { SecItemCopyMatching(query.get(), &mut result) };
            // SAFETY: the call gave this code the reference it copied, or null.
            let result = unsafe { Cf::owned(result) };
            match status {
                OK => {}
                ITEM_NOT_FOUND => return Ok(None),
                status => return Err(fail(status, purpose, "read")),
            }
            let mut bytes = result.bytes().ok_or_else(|| {
                KeyError::Unreadable(format!(
                    "the keychain gave the {} as something other than data",
                    purpose.description()
                ))
            })?;
            let secret = Secret::from_slice(&bytes);
            let length = bytes.len();
            bytes.iter_mut().for_each(|b| *b = 0);
            secret.map(Some).ok_or_else(|| {
                KeyError::Unreadable(format!(
                    "the {} in the keychain is {length} bytes long, not {SECRET_LEN}; it was not \
                     made by DSA",
                    purpose.description()
                ))
            })
        }

        fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
            let (service, account) = item(purpose);
            let label = Cf::string(purpose.description());
            let value = Cf::data(secret.bytes());
            // SAFETY: the keychain's constants are CFStrings; every value is a live object.
            let (query, update) = unsafe {
                (
                    Cf::dictionary(&[
                        (kSecClass, kSecClassGenericPassword),
                        (kSecAttrService, service.get()),
                        (kSecAttrAccount, account.get()),
                    ]),
                    Cf::dictionary(&[(kSecValueData, value.get()), (kSecAttrLabel, label.get())]),
                )
            };
            // SAFETY: two dictionaries of live objects.
            let status = unsafe { SecItemUpdate(query.get(), update.get()) };
            match status {
                OK => return Ok(()),
                ITEM_NOT_FOUND => {}
                status => return Err(fail(status, purpose, "kept")),
            }
            let add = |accessible: bool| -> i32 {
                // SAFETY: as above.
                unsafe {
                    let mut pairs = vec![
                        (kSecClass, kSecClassGenericPassword),
                        (kSecAttrService, service.get()),
                        (kSecAttrAccount, account.get()),
                        (kSecAttrLabel, label.get()),
                        (kSecValueData, value.get()),
                        (kSecAttrSynchronizable, kCFBooleanFalse),
                    ];
                    if accessible {
                        pairs.push((
                            kSecAttrAccessible,
                            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                        ));
                    }
                    let attributes = Cf::dictionary(&pairs);
                    SecItemAdd(attributes.get(), std::ptr::null_mut())
                }
            };
            let mut status = add(true);
            if status == PARAM {
                // A keychain that takes no accessibility class keeps the item without one;
                // the login keychain never synchronizes it either way.
                eprintln!(
                    "keychain: the {} is kept without an accessibility class, which this \
                     keychain does not take",
                    purpose.description()
                );
                status = add(false);
            }
            match status {
                OK => Ok(()),
                // Made at the same moment by another call: update that one instead.
                DUPLICATE_ITEM => {
                    // SAFETY: two dictionaries of live objects.
                    match unsafe { SecItemUpdate(query.get(), update.get()) } {
                        OK => Ok(()),
                        status => Err(fail(status, purpose, "kept")),
                    }
                }
                status => Err(fail(status, purpose, "kept")),
            }
        }

        fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
            let (service, account) = item(purpose);
            // SAFETY: the keychain's constants are CFStrings; every value is a live object.
            let query = unsafe {
                Cf::dictionary(&[
                    (kSecClass, kSecClassGenericPassword),
                    (kSecAttrService, service.get()),
                    (kSecAttrAccount, account.get()),
                ])
            };
            // SAFETY: a query dictionary of live objects.
            match unsafe { SecItemDelete(query.get()) } {
                OK | ITEM_NOT_FOUND => Ok(()),
                status => Err(fail(status, purpose, "removed")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_a_locked_or_refused_keychain_makes_the_key_unavailable() {
        for status in [
            INTERACTION_NOT_ALLOWED,
            USER_CANCELED,
            AUTH_FAILED,
            NO_SUCH_KEYCHAIN,
            NO_DEFAULT_KEYCHAIN,
            NOT_AVAILABLE,
            MISSING_ENTITLEMENT,
        ] {
            match status_error(status, KeyPurpose::Backup, "read", "") {
                KeyError::Unavailable(why) => {
                    assert!(why.contains("backup key"), "{why}");
                    assert!(why.contains(&status.to_string()), "{why}");
                }
                other => panic!("{status}: expected unavailable, got {other:?}"),
            }
        }
    }

    #[test]
    fn fr36_a_key_the_keychain_cannot_decode_is_unreadable_and_others_fail() {
        assert!(matches!(
            status_error(DECODE, KeyPurpose::KeyMintRoot, "read", "decode failed"),
            KeyError::Unreadable(_)
        ));
        match status_error(-1, KeyPurpose::KeyMintRoot, "kept", "boom") {
            KeyError::Failed(why) => {
                assert!(why.contains("keep the"), "{why}");
                assert!(why.contains("boom (OSStatus -1)"), "{why}");
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn fr36_items_are_named_by_the_service_and_the_keys_name() {
        assert_eq!(SERVICE, "io.github.thisisthepy.DSA");
        assert_eq!(KeyPurpose::Backup.name(), "backup-key");
        assert_eq!(KeyPurpose::KeyMintRoot.name(), "keymint-root-key");
    }
}

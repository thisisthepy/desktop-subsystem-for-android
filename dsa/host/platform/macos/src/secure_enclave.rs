//! The KeyMint root key sealed by the Mac's Secure Enclave.
//!
//! The Secure Enclave keeps a P-256 private key for DSA that never leaves it (made with
//! `kSecAttrTokenIDSecureEnclave`, permanent, tagged `io.github.thisisthepy.DSA.keymint-sealing`,
//! usable after the first unlock and on this Mac only). A secret is sealed by encrypting it,
//! with what it is for after it, to that key's public half (ECIES: cofactor ECDH, X9.63 KDF
//! with SHA-256, AES-GCM with a variable IV); only the Secure Enclave opens it again. The
//! sealed bytes are kept in a file of the user's DSA folder.
//!
//! A Mac without a Secure Enclave (an Intel Mac without a T2 chip), or a copy of DSA not signed
//! with the keychain entitlement the data protection keychain needs, cannot make the key; the
//! KeyMint root key is then kept by the login keychain, with that reason. A Mac that was erased
//! or restored to another Mac has another Secure Enclave, and a key sealed by the old one is
//! told to be gone.

use dsa_host_core::host_keys::KeyError;

/// The sealer's name, as sealed files record it.
pub const SEALER: &str = "secure-enclave";

/// The application tag of DSA's key in the Secure Enclave.
pub const KEY_TAG: &str = "io.github.thisisthepy.DSA.keymint-sealing";

/// `errSecMissingEntitlement`.
pub const MISSING_ENTITLEMENT: i64 = -34018;
/// `errSecItemNotFound`.
pub const ITEM_NOT_FOUND: i64 = -25300;
/// `errSecUnimplemented`, `errSecNotAvailable`, `errSecParam`: no Secure Enclave to make a key in.
pub const NO_ENCLAVE: [i64; 3] = [-4, -25291, -50];
/// `errSecInteractionNotAllowed`: the Mac has not been unlocked since it started.
pub const INTERACTION_NOT_ALLOWED: i64 = -25308;

/// Why the Secure Enclave cannot seal, after `step` failed with `code` (an OSStatus) and
/// `message`, for a person.
pub fn unusable_reason(step: &str, code: i64, message: &str) -> String {
    let said = if message.is_empty() {
        format!("OSStatus {code}")
    } else {
        format!("{message} (OSStatus {code})")
    };
    if code == MISSING_ENTITLEMENT {
        format!(
            "this copy of DSA is not signed with the keychain entitlement the Secure Enclave \
             needs ({step}: {said})"
        )
    } else if NO_ENCLAVE.contains(&code) {
        format!("this Mac has no Secure Enclave DSA can use ({step}: {said})")
    } else if code == INTERACTION_NOT_ALLOWED {
        format!(
            "the Mac has not been unlocked since it started ({step}: {said}); DSA seals the key \
             with the Secure Enclave at the next start"
        )
    } else {
        format!("the Secure Enclave failed ({step}: {said}); DSA tries it again at the next start")
    }
}

/// What a failed unseal means for the key labelled `label`.
pub fn unseal_error(label: &str, step: &str, code: i64, message: &str) -> KeyError {
    let said = if message.is_empty() {
        format!("OSStatus {code}")
    } else {
        format!("{message} (OSStatus {code})")
    };
    if code == ITEM_NOT_FOUND {
        KeyError::Unreadable(format!(
            "the Secure Enclave key the {label} was sealed with is gone: the Mac was erased, or \
             this is not the Mac it was sealed on ({step}: {said})"
        ))
    } else if code == INTERACTION_NOT_ALLOWED {
        KeyError::Unavailable(format!(
            "the Mac has not been unlocked since it started, so the Secure Enclave cannot open \
             the {label} yet ({step}: {said}); it opens at a later start"
        ))
    } else if code == MISSING_ENTITLEMENT {
        KeyError::Unavailable(format!(
            "the {label} was sealed by the Secure Enclave, which this copy of DSA is not signed \
             to use ({step}: {said}); install DSA's signed release again"
        ))
    } else {
        KeyError::Unreadable(format!(
            "the Secure Enclave cannot open the {label} ({step}: {said})"
        ))
    }
}

/// The bytes sealed for `label`: the secret, then what it is for.
pub fn with_label(plain: &[u8], label: &str) -> Vec<u8> {
    [plain, label.as_bytes()].concat()
}

/// The secret in `opened` if it was sealed for `label`.
pub fn without_label(opened: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
    opened
        .strip_suffix(label.as_bytes())
        .map(<[u8]>::to_vec)
        .ok_or_else(|| KeyError::Unreadable(format!("it was not sealed as the {label}")))
}

#[cfg(target_os = "macos")]
pub use mac::{usable, SecureEnclaveSealer};

#[cfg(target_os = "macos")]
mod mac {
    use dsa_host_core::host_keys::{KeyError, Sealer};

    use super::*;
    use crate::security::*;

    /// `kSecAccessControlPrivateKeyUsage`: the key may be used, with no prompt.
    const PRIVATE_KEY_USAGE: usize = 1 << 30;

    /// The query that finds DSA's key, with the objects it points into.
    fn key_query() -> (Cf, Cf) {
        let tag = Cf::data(KEY_TAG.as_bytes());
        // SAFETY: the keychain's constants are live CFTypes; so is the tag.
        let query = unsafe {
            Cf::dictionary(&[
                (kSecClass, kSecClassKey),
                (kSecAttrApplicationTag, tag.get()),
                (kSecAttrKeyType, kSecAttrKeyTypeECSECPrimeRandom),
                (kSecAttrTokenID, kSecAttrTokenIDSecureEnclave),
                (kSecUseDataProtectionKeychain, kCFBooleanTrue),
                (kSecReturnRef, kCFBooleanTrue),
            ])
        };
        (query, tag)
    }

    /// DSA's private key in the Secure Enclave; `Err` with the OSStatus when there is none
    /// (`ITEM_NOT_FOUND`) or it cannot be had.
    fn find_key() -> Result<Cf, (i64, String)> {
        let (query, _tag) = key_query();
        let mut result: CFTypeRef = std::ptr::null();
        // SAFETY: a query dictionary and a place for the reference the call copies.
        let status = unsafe { SecItemCopyMatching(query.get(), &mut result) };
        // SAFETY: the call gave this code the reference it copied, or null.
        let key = unsafe { Cf::owned(result) };
        if status != 0 || key.is_null() {
            return Err((status as i64, status_message(status)));
        }
        Ok(key)
    }

    /// Makes DSA's private key in the Secure Enclave.
    fn make_key() -> Result<Cf, (i64, String)> {
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: the constants are live CFTypes; the call returns an object this code owns,
        // or null with an error it owns.
        let access = unsafe {
            Cf::owned(SecAccessControlCreateWithFlags(
                std::ptr::null(),
                kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                PRIVATE_KEY_USAGE,
                &mut error,
            ))
        };
        // SAFETY: the call gave this code the error it made, or null.
        let error = unsafe { Cf::owned(error) };
        if access.is_null() {
            return Err(error_of(&error));
        }
        let tag = Cf::data(KEY_TAG.as_bytes());
        let bits = Cf::number(256);
        // SAFETY: the constants and objects are live CFTypes.
        let parameters = unsafe {
            let private = Cf::dictionary(&[
                (kSecAttrIsPermanent, kCFBooleanTrue),
                (kSecAttrApplicationTag, tag.get()),
                (kSecAttrAccessControl, access.get()),
            ]);
            let parameters = Cf::dictionary(&[
                (kSecAttrKeyType, kSecAttrKeyTypeECSECPrimeRandom),
                (kSecAttrKeySizeInBits, bits.get()),
                (kSecAttrTokenID, kSecAttrTokenIDSecureEnclave),
                (kSecUseDataProtectionKeychain, kCFBooleanTrue),
                (kSecPrivateKeyAttrs, private.get()),
            ]);
            drop(private);
            parameters
        };
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: a parameter dictionary; the call returns a key this code owns, or null with
        // an error it owns.
        let key = unsafe { Cf::owned(SecKeyCreateRandomKey(parameters.get(), &mut error)) };
        // SAFETY: as above.
        let error = unsafe { Cf::owned(error) };
        if key.is_null() {
            return Err(error_of(&error));
        }
        Ok(key)
    }

    fn find_or_make_key() -> Result<Cf, (i64, String)> {
        match find_key() {
            Err((code, _)) if code == ITEM_NOT_FOUND => make_key(),
            found => found,
        }
    }

    /// Encrypts `plain` to the public half of `key`.
    fn encrypt(key: &Cf, plain: &[u8]) -> Result<Vec<u8>, (i64, String)> {
        // SAFETY: a live key; the call returns its public half, which this code owns.
        let public = unsafe { Cf::owned(SecKeyCopyPublicKey(key.get())) };
        if public.is_null() {
            return Err((-50, "the key has no public half".into()));
        }
        let input = Cf::data(plain);
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: a public key, the algorithm constant and a CFData; the call returns a
        // CFData this code owns, or null with an error it owns.
        let sealed = unsafe {
            Cf::owned(SecKeyCreateEncryptedData(
                public.get(),
                kSecKeyAlgorithmECIESEncryptionCofactorVariableIVX963SHA256AESGCM,
                input.get(),
                &mut error,
            ))
        };
        // SAFETY: as above.
        let error = unsafe { Cf::owned(error) };
        sealed.bytes().ok_or_else(|| error_of(&error))
    }

    /// Decrypts `sealed` with `key`, in the Secure Enclave.
    fn decrypt(key: &Cf, sealed: &[u8]) -> Result<Vec<u8>, (i64, String)> {
        let input = Cf::data(sealed);
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: a private key, the algorithm constant and a CFData; the call returns a
        // CFData this code owns, or null with an error it owns.
        let opened = unsafe {
            Cf::owned(SecKeyCreateDecryptedData(
                key.get(),
                kSecKeyAlgorithmECIESEncryptionCofactorVariableIVX963SHA256AESGCM,
                input.get(),
                &mut error,
            ))
        };
        // SAFETY: as above.
        let error = unsafe { Cf::owned(error) };
        opened.bytes().ok_or_else(|| error_of(&error))
    }

    /// Whether the Secure Enclave can seal and open for DSA now: its key is there or can be
    /// made, and a test secret goes through it and back. `Err` says why not.
    pub fn usable() -> Result<(), String> {
        let key = find_or_make_key()
            .map_err(|(code, message)| unusable_reason("making the key", code, &message))?;
        let probe = [0x5Au8; 32];
        let sealed = encrypt(&key, &probe)
            .map_err(|(code, message)| unusable_reason("sealing", code, &message))?;
        let opened = decrypt(&key, &sealed)
            .map_err(|(code, message)| unusable_reason("opening", code, &message))?;
        if opened != probe {
            return Err("the Secure Enclave returned other bytes than it was given".into());
        }
        Ok(())
    }

    /// Seals with the Mac's Secure Enclave.
    pub struct SecureEnclaveSealer;

    impl Sealer for SecureEnclaveSealer {
        fn name(&self) -> &str {
            SEALER
        }

        fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let failed = |(code, message): (i64, String)| {
                KeyError::Failed(format!(
                    "the Secure Enclave could not seal the {label}: {}",
                    unusable_reason("sealing", code, &message)
                ))
            };
            let key = find_or_make_key().map_err(failed)?;
            encrypt(&key, &with_label(plain, label)).map_err(failed)
        }

        fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let key = find_key().map_err(|(code, message)| {
                unseal_error(label, "finding the key", code, &message)
            })?;
            let opened = decrypt(&key, sealed)
                .map_err(|(code, message)| unseal_error(label, "opening", code, &message))?;
            without_label(&opened, label)
        }

        fn kind(&self) -> &str {
            SEALER
        }

        fn hardware(&self) -> bool {
            true
        }

        fn description(&self) -> String {
            "the Mac's Secure Enclave".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_a_mac_that_cannot_use_its_secure_enclave_says_why() {
        assert!(unusable_reason("making the key", MISSING_ENTITLEMENT, "")
            .contains("not signed with the keychain entitlement"));
        assert!(unusable_reason("making the key", -4, "").contains("no Secure Enclave"));
        assert!(
            unusable_reason("making the key", INTERACTION_NOT_ALLOWED, "")
                .contains("not been unlocked")
        );
        assert!(unusable_reason("sealing", -1, "boom").contains("boom (OSStatus -1)"));
    }

    #[test]
    fn fr36_a_key_whose_secure_enclave_is_gone_is_unreadable_and_a_locked_mac_is_waited_for() {
        assert!(matches!(
            unseal_error("KeyMint root key", "finding the key", ITEM_NOT_FOUND, ""),
            KeyError::Unreadable(why) if why.contains("erased")
        ));
        assert!(matches!(
            unseal_error("KeyMint root key", "opening", INTERACTION_NOT_ALLOWED, ""),
            KeyError::Unavailable(_)
        ));
    }

    #[test]
    fn fr36_a_secret_sealed_for_one_purpose_does_not_open_as_another() {
        let sealed = with_label(&[7; 32], "keymint-root-key");
        assert_eq!(
            without_label(&sealed, "keymint-root-key").unwrap(),
            vec![7; 32]
        );
        assert!(without_label(&sealed, "backup-key").is_err());
    }
}

//! Each user's secrets on Windows: the key their backup folder is encrypted with, and the root
//! key of their Android's KeyMint.
//!
//! Each secret is sealed in a file of the user's DSA folder (`%LOCALAPPDATA%\DSA\keys`),
//! never on the guest's disk, by [`SealedKeys`] with the sealers this computer has:
//!
//! - `tpm+dpapi` (a TPM 2.0) and `tpm12+dpapi` (a TPM 1.2): first encrypted (RSA-OAEP, with
//!   SHA-256 on a TPM 2.0 and SHA-1, the only hash a TPM 1.2 takes, on a TPM 1.2) to a key the
//!   computer's TPM keeps through the Microsoft Platform Crypto Provider (a persisted 2048-bit
//!   RSA key named for DSA and the user's SID, which never leaves the TPM), then sealed by
//!   DPAPI as below. It opens only for that user on this computer: a copy of the file and of
//!   the user's credentials is not enough without this TPM.
//! - `dpapi`: DPAPI under the user's own account (`CryptProtectData`, with what the secret is
//!   for as its entropy). Only that user, on this computer or wherever their account's
//!   credentials roam, opens it. The service runs as SYSTEM in session 0, so it seals and
//!   opens while impersonating the user, with the token of a session they are signed in to;
//!   when they are signed in nowhere there is no token, and the secret is unavailable.
//!
//! The KeyMint root key is sealed by the TPM whenever the computer has one that works: one
//! that is there, turned on, ready, not locked out, and seals and opens a test secret. Only
//! where none works is it kept by DPAPI alone, and the reason is recorded in the log and shown
//! in the settings app and `dsa status`. Once a TPM works, the next start of Android seals the
//! same key with it, so nothing is lost. The backup key is always `dpapi`: a backup is restored
//! on another computer with its recovery key.
//!
//! A key the TPM sealed opens only with that TPM. One that is locked out says so and is tried
//! again at the next start; one that was turned off says to turn it on again; one that was
//! cleared (or a disk moved to another computer) says the key is gone with it. The key is never
//! replaced, since Android's keys are wrapped under it; a data reset lets it go.

#[cfg(any(windows, test))]
use std::path::Path;
use std::sync::Arc;

pub use dsa_host_core::host_keys::{sealer_of, SealedKeys};
use dsa_host_core::host_keys::{Hardware, KeyError, KeyPurpose, Sealer, SealerSource};

/// The name of the DPAPI sealer, as sealed files record it.
pub const DPAPI: &str = "dpapi";
/// The name of the sealer of a TPM 2.0 and DPAPI, as sealed files record it.
pub const TPM_DPAPI: &str = "tpm+dpapi";
/// The name of the sealer of a TPM 1.2 and DPAPI, as sealed files record it.
pub const TPM12_DPAPI: &str = "tpm12+dpapi";

/// Which TPM the computer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpmVersion {
    V12,
    V20,
}

impl TpmVersion {
    /// The name of the sealer of this TPM and DPAPI.
    pub fn sealer(self) -> &'static str {
        match self {
            TpmVersion::V12 => TPM12_DPAPI,
            TpmVersion::V20 => TPM_DPAPI,
        }
    }

    /// The TPM a sealer's name names, if it names one.
    pub fn of_sealer(name: &str) -> Option<TpmVersion> {
        match name {
            TPM_DPAPI => Some(TpmVersion::V20),
            TPM12_DPAPI => Some(TpmVersion::V12),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TpmVersion::V12 => "TPM 1.2",
            TpmVersion::V20 => "TPM 2.0",
        }
    }
}

/// The TPM version the Platform Crypto Provider reports in `PCP_PLATFORM_TYPE`, such as
/// `TPM-Version:2.0 -Level:0-Revision:1.38-VendorID:'INTC'-Firmware:...`.
pub fn tpm_version(platform_type: &str) -> Option<TpmVersion> {
    let rest = platform_type.split("TPM-Version:").nth(1)?;
    let version: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    match version.as_str() {
        "1.2" => Some(TpmVersion::V12),
        v if v.starts_with("2.") => Some(TpmVersion::V20),
        _ => None,
    }
}

/// What an NCrypt or TPM error means for the TPM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpmProblem {
    /// There is no TPM that can be used: none, turned off in the firmware, not provisioned,
    /// or one that cannot do what is asked.
    Absent,
    /// The TPM refuses for a while after too many wrong authorizations.
    LockedOut,
    /// The TPM is busy or testing itself; it works again soon.
    NotReady,
    /// The key the secret was sealed with is not in the TPM: the TPM was cleared, or this is
    /// another computer.
    KeyGone,
    /// Anything else the provider or the TPM failed with.
    Failed,
}

const NTE_BAD_KEYSET: u32 = 0x8009_0016;

/// What `hr`, an error of NCrypt, the Platform Crypto Provider, TPM Base Services or the TPM,
/// means for the TPM.
pub fn tpm_problem(hr: i32) -> TpmProblem {
    let code = hr as u32;
    match code {
        NTE_BAD_KEYSET => TpmProblem::KeyGone,
        // TPM 2.0 TPM_RC_LOCKOUT, TPM 1.2 TPM_DEFEND_LOCK_RUNNING.
        0x8028_0921 | 0x8028_0803 => TpmProblem::LockedOut,
        // TPM 2.0 TPM_RC_RETRY, TPM_RC_TESTING, TPM_RC_YIELDED; TPM 1.2 TPM_RETRY,
        // TPM_NEEDS_SELFTEST, TPM_DOING_SELFTEST; the provider's device not ready; TBS busy.
        0x8028_0922 | 0x8028_090A | 0x8028_0908 | 0x8028_0800 | 0x8028_0801 | 0x8028_0802
        | 0x8029_0101 | 0x8028_4002 => TpmProblem::NotReady,
        // NTE_BAD_PROVIDER, NTE_PROVIDER_DLL_FAIL, NTE_PROV_DLL_NOT_FOUND, NTE_NOT_SUPPORTED,
        // NTE_DEVICE_NOT_READY (a TPM that is not provisioned), NTE_DEVICE_NOT_FOUND,
        // NTE_BAD_ALGID (a TPM that cannot do the padding), the provider's not supported.
        0x8009_0013 | 0x8009_001D | 0x8009_001E | 0x8009_0029 | 0x8009_0030 | 0x8009_0035
        | 0x8009_0008 | 0x8029_0105 => TpmProblem::Absent,
        // TPM Base Services: no TPM, its service turned off or not running, TPM deactivated.
        code if (code & 0xFFFF_F000) == 0x8028_4000 => TpmProblem::Absent,
        // TPM 1.2: deactivated, disabled, no owner (no storage root key).
        0x8028_0006 | 0x8028_0007 | 0x8028_0012 => TpmProblem::Absent,
        _ => TpmProblem::Failed,
    }
}

/// Whether `hr` means the computer has no TPM that can be used now, rather than a TPM that
/// works but failed this once.
pub fn tpm_unusable(hr: i32) -> bool {
    !matches!(tpm_problem(hr), TpmProblem::Failed | TpmProblem::KeyGone)
}

/// Why the TPM cannot seal for a user, after `step` failed with `hr`, in words a person can act
/// on.
pub fn unusable_reason(step: &str, hr: i32) -> String {
    let code = hresult_text(hr);
    match tpm_problem(hr) {
        TpmProblem::Absent => format!(
            "this computer has no TPM that DSA can use ({step}: {code}); turn the TPM on in the \
             computer's firmware settings, and DSA seals the key with it at the next start"
        ),
        TpmProblem::LockedOut => format!(
            "the TPM is locked out after too many wrong authorizations ({step}: {code}); \
             Windows lifts the lockout after a while, and DSA seals the key with it at the next \
             start"
        ),
        TpmProblem::NotReady => format!(
            "the TPM was not ready ({step}: {code}); DSA seals the key with it at the next start"
        ),
        TpmProblem::KeyGone | TpmProblem::Failed => {
            format!("the TPM failed ({step}: {code}); DSA tries it again at the next start")
        }
    }
}

/// Why a secret the TPM sealed cannot be opened, after `step` failed with `hr`. A TPM that is
/// locked out or busy is a wait (`Unavailable`); one that is off or cleared is `Unreadable`.
pub fn unseal_error(label: &str, step: &str, hr: i32) -> KeyError {
    let code = hresult_text(hr);
    match tpm_problem(hr) {
        TpmProblem::LockedOut => KeyError::Unavailable(format!(
            "the TPM that sealed the {label} is locked out after too many wrong authorizations \
             ({step}: {code}); Windows lifts the lockout after a while (or clear it in the TPM \
             management console, tpm.msc), and the key opens again at the next start"
        )),
        TpmProblem::NotReady => KeyError::Unavailable(format!(
            "the TPM that sealed the {label} was not ready ({step}: {code}); it is tried again \
             at the next start"
        )),
        TpmProblem::Absent => KeyError::Unreadable(format!(
            "the {label} was sealed by this computer's TPM, which cannot be used now ({step}: \
             {code}); turn the TPM on again in the computer's firmware settings"
        )),
        TpmProblem::KeyGone => KeyError::Unreadable(format!(
            "the TPM key the {label} was sealed with is gone: the TPM was cleared, or this is \
             not the computer it was sealed on ({step}: {code})"
        )),
        TpmProblem::Failed => KeyError::Unavailable(format!(
            "the computer's TPM could not open the {label} ({step}: {code}); it is tried again \
             at the next start"
        )),
    }
}

/// The TPM key's name for the user with SID `sid`.
pub fn tpm_key_name(sid: &str) -> String {
    format!("Desktop Subsystem for Android key sealing {sid}")
}

/// The entropy DPAPI binds into a seal: the sealer and what is sealed, so a seal of one
/// purpose (or of one sealer's inner bytes) never opens as another.
pub fn dpapi_entropy(sealer: &str, label: &str) -> Vec<u8> {
    format!("Desktop Subsystem for Android {sealer} {label}").into_bytes()
}

/// A text for HRESULT `hr`: Windows' message and the code.
pub fn hresult_text(hr: i32) -> String {
    let message = std::io::Error::from_raw_os_error(hr).to_string();
    format!("{message} (0x{:08X})", hr as u32)
}

/// The sealer to open a secret `name` sealed, from the sealers `dpapi` and `tpm` (of the TPM
/// version given) make, or why this computer cannot.
pub fn sealer_named(
    name: &str,
    purpose: KeyPurpose,
    dpapi: impl FnOnce() -> Arc<dyn Sealer>,
    tpm: impl FnOnce(TpmVersion) -> Arc<dyn Sealer>,
) -> Result<Arc<dyn Sealer>, KeyError> {
    match (name, TpmVersion::of_sealer(name)) {
        (DPAPI, _) => Ok(dpapi()),
        (_, Some(version)) => Ok(tpm(version)),
        (other, None) => Err(KeyError::Unreadable(format!(
            "the {} was sealed by {other}, which this version of DSA does not know",
            purpose.description()
        ))),
    }
}

#[cfg(windows)]
pub use self::sealers::{DpapiSealer, TpmDpapiSealer, WindowsSealers};

#[cfg(windows)]
mod sealers {
    use std::io;
    use std::sync::Arc;

    use dsa_host_core::host_keys::{Hardware, KeyError, KeyPurpose, Sealer, SealerSource};
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, NCryptCreatePersistedKey, NCryptDecrypt,
        NCryptEncrypt, NCryptFinalizeKey, NCryptFreeObject, NCryptGetProperty, NCryptOpenKey,
        NCryptOpenStorageProvider, NCryptSetProperty, BCRYPT_OAEP_PADDING_INFO,
        BCRYPT_SHA1_ALGORITHM, BCRYPT_SHA256_ALGORITHM, CRYPTPROTECT_UI_FORBIDDEN,
        CRYPT_INTEGER_BLOB, MS_PLATFORM_CRYPTO_PROVIDER, NCRYPT_ALLOW_DECRYPT_FLAG,
        NCRYPT_KEY_USAGE_PROPERTY, NCRYPT_LENGTH_PROPERTY, NCRYPT_MACHINE_KEY_FLAG,
        NCRYPT_PAD_OAEP_FLAG, NCRYPT_PCP_PLATFORM_TYPE_PROPERTY, NCRYPT_RSA_ALGORITHM,
        NCRYPT_SILENT_FLAG,
    };

    use super::{
        dpapi_entropy, sealer_named, tpm_key_name, tpm_version, unseal_error, unusable_reason,
        TpmVersion, DPAPI,
    };
    use crate::pipe::current_user_sid;

    const NTE_BAD_KEYSET: i32 = super::NTE_BAD_KEYSET as i32;
    const NTE_EXISTS: i32 = 0x8009_000F_u32 as i32;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Runs `work` as the user with SID `sid`: on this thread, impersonating them with the
    /// token of a session they are signed in to, or as this process when it is the user's own.
    fn as_user<T>(sid: &str, work: impl FnOnce() -> T) -> Result<T, KeyError> {
        crate::safe_fs::as_user(sid, work).map_err(|why| {
            KeyError::Unavailable(format!(
                "Windows opens this user's credentials only while they are signed in, and they \
                 are not ({why})"
            ))
        })
    }

    /// Takes the bytes of a blob DPAPI allocated, and frees it.
    fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if blob.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: DPAPI returned `cbData` bytes at `pbData`, freed once below.
        let bytes =
            unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
        // SAFETY: a LocalAlloc'd buffer DPAPI handed over.
        unsafe { LocalFree(blob.pbData as HLOCAL) };
        bytes
    }

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    /// DPAPI under the user's account, which never shows a prompt.
    fn dpapi_protect(
        sid: &str,
        plain: &[u8],
        entropy: &[u8],
        what: &str,
    ) -> Result<Vec<u8>, KeyError> {
        let description = wide(what);
        as_user(sid, || {
            let (input, entropy) = (blob(plain), blob(entropy));
            let mut output = CRYPT_INTEGER_BLOB::default();
            // SAFETY: blobs over buffers that outlive the call, and an out blob DPAPI fills.
            let ok = unsafe {
                CryptProtectData(
                    &input,
                    description.as_ptr(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            };
            if ok == 0 {
                return Err(KeyError::Failed(format!(
                    "Windows could not seal the {what} for this user: {}",
                    io::Error::last_os_error()
                )));
            }
            Ok(take_blob(output))
        })?
    }

    fn dpapi_unprotect(
        sid: &str,
        sealed: &[u8],
        entropy: &[u8],
        what: &str,
    ) -> Result<Vec<u8>, KeyError> {
        as_user(sid, || {
            let (input, entropy) = (blob(sealed), blob(entropy));
            let mut output = CRYPT_INTEGER_BLOB::default();
            // SAFETY: blobs over buffers that outlive the call, and an out blob DPAPI fills.
            let ok = unsafe {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            };
            if ok == 0 {
                return Err(KeyError::Unreadable(format!(
                    "Windows cannot open the {what} for this user: it was sealed for another \
                     user or on another computer ({})",
                    io::Error::last_os_error()
                )));
            }
            Ok(take_blob(output))
        })?
    }

    /// Seals with DPAPI under the account of the user with SID `sid`.
    pub struct DpapiSealer {
        sid: String,
    }

    impl DpapiSealer {
        pub fn new(sid: &str) -> DpapiSealer {
            DpapiSealer {
                sid: sid.to_string(),
            }
        }
    }

    impl Sealer for DpapiSealer {
        fn name(&self) -> &str {
            DPAPI
        }
        fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            dpapi_protect(&self.sid, plain, &dpapi_entropy(DPAPI, label), label)
        }
        fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            dpapi_unprotect(&self.sid, sealed, &dpapi_entropy(DPAPI, label), label)
        }
        fn kind(&self) -> &str {
            "dpapi"
        }
        fn description(&self) -> String {
            "Windows DPAPI under your account".into()
        }
    }

    /// An NCrypt handle, freed when dropped.
    struct NcryptObject(usize);

    impl Drop for NcryptObject {
        fn drop(&mut self) {
            // SAFETY: an NCrypt handle, freed once.
            unsafe { NCryptFreeObject(self.0) };
        }
    }

    fn hr_ok(hr: i32) -> Result<(), i32> {
        if hr >= 0 {
            Ok(())
        } else {
            Err(hr)
        }
    }

    fn provider() -> Result<NcryptObject, i32> {
        let mut provider = 0usize;
        // SAFETY: an out handle and the provider's NUL-terminated name.
        hr_ok(unsafe { NCryptOpenStorageProvider(&mut provider, MS_PLATFORM_CRYPTO_PROVIDER, 0) })?;
        Ok(NcryptObject(provider))
    }

    /// The TPM version the provider reports; `None` when it reports none it knows.
    fn platform_version(provider: &NcryptObject) -> Option<TpmVersion> {
        let mut buffer = [0u16; 256];
        let mut written = 0u32;
        // SAFETY: a provider handle, a NUL-terminated property name, and a buffer of the size
        // given, which the call fills with a NUL-terminated wide string.
        let hr = unsafe {
            NCryptGetProperty(
                provider.0,
                NCRYPT_PCP_PLATFORM_TYPE_PROPERTY,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 2) as u32,
                &mut written,
                0,
            )
        };
        if hr < 0 {
            return None;
        }
        let chars = (written as usize / 2).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..chars]);
        tpm_version(text.trim_end_matches('\0'))
    }

    /// The TPM key of one user, through the Microsoft Platform Crypto Provider.
    struct TpmKey {
        name: Vec<u16>,
        /// Whether the key is the machine's (named for the user) rather than in the user
        /// profile of this process's account: the service, as SYSTEM, keeps machine keys.
        machine: bool,
    }

    impl TpmKey {
        fn of(sid: &str) -> TpmKey {
            TpmKey {
                name: wide(&tpm_key_name(sid)),
                machine: !current_user_sid().is_ok_and(|me| me == sid),
            }
        }

        fn flags(&self) -> u32 {
            NCRYPT_SILENT_FLAG
                | if self.machine {
                    NCRYPT_MACHINE_KEY_FLAG
                } else {
                    0
                }
        }

        /// The key, `Err(NTE_BAD_KEYSET)` when the TPM has none by that name.
        fn open(&self, provider: &NcryptObject) -> Result<NcryptObject, i32> {
            let mut key = 0usize;
            // SAFETY: a provider handle, a NUL-terminated name and an out handle.
            hr_ok(unsafe {
                NCryptOpenKey(provider.0, &mut key, self.name.as_ptr(), 0, self.flags())
            })?;
            Ok(NcryptObject(key))
        }

        fn open_or_create(&self, provider: &NcryptObject) -> Result<NcryptObject, i32> {
            match self.open(provider) {
                Err(NTE_BAD_KEYSET) => {}
                other => return other,
            }
            let mut key = 0usize;
            // SAFETY: a provider handle, the algorithm's and key's NUL-terminated names and an
            // out handle.
            let hr = unsafe {
                NCryptCreatePersistedKey(
                    provider.0,
                    &mut key,
                    NCRYPT_RSA_ALGORITHM,
                    self.name.as_ptr(),
                    0,
                    if self.machine {
                        NCRYPT_MACHINE_KEY_FLAG
                    } else {
                        0
                    },
                )
            };
            if hr == NTE_EXISTS {
                // Made by another thread just now.
                return self.open(provider);
            }
            hr_ok(hr)?;
            let key = NcryptObject(key);
            let length: u32 = 2048;
            let usage: u32 = NCRYPT_ALLOW_DECRYPT_FLAG;
            for (property, value) in [
                (NCRYPT_LENGTH_PROPERTY, length),
                (NCRYPT_KEY_USAGE_PROPERTY, usage),
            ] {
                // SAFETY: a key handle, a property name and a u32 value of the size given.
                hr_ok(unsafe {
                    NCryptSetProperty(
                        key.0,
                        property,
                        (&value as *const u32).cast(),
                        std::mem::size_of::<u32>() as u32,
                        0,
                    )
                })?;
            }
            // SAFETY: a key handle whose properties are set.
            hr_ok(unsafe { NCryptFinalizeKey(key.0, NCRYPT_SILENT_FLAG) })?;
            Ok(key)
        }
    }

    /// RSA-OAEP's padding: SHA-256 on a TPM 2.0, SHA-1 (the only hash a TPM 1.2 pads with).
    fn oaep(version: TpmVersion) -> BCRYPT_OAEP_PADDING_INFO {
        BCRYPT_OAEP_PADDING_INFO {
            pszAlgId: match version {
                TpmVersion::V20 => BCRYPT_SHA256_ALGORITHM,
                TpmVersion::V12 => BCRYPT_SHA1_ALGORITHM,
            },
            pbLabel: std::ptr::null_mut(),
            cbLabel: 0,
        }
    }

    /// Encrypts (`encrypt`) or decrypts `input` with `key`, RSA-OAEP.
    fn rsa(
        key: &NcryptObject,
        version: TpmVersion,
        input: &[u8],
        encrypt: bool,
    ) -> Result<Vec<u8>, i32> {
        let padding = oaep(version);
        let call = |output: *mut u8, capacity: u32, written: &mut u32| -> i32 {
            // SAFETY: a key handle, the input, the padding info and an output buffer of
            // `capacity` bytes (or none, to ask for the size).
            unsafe {
                if encrypt {
                    NCryptEncrypt(
                        key.0,
                        input.as_ptr(),
                        input.len() as u32,
                        (&padding as *const BCRYPT_OAEP_PADDING_INFO).cast(),
                        output,
                        capacity,
                        written,
                        NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG,
                    )
                } else {
                    NCryptDecrypt(
                        key.0,
                        input.as_ptr(),
                        input.len() as u32,
                        (&padding as *const BCRYPT_OAEP_PADDING_INFO).cast(),
                        output,
                        capacity,
                        written,
                        NCRYPT_PAD_OAEP_FLAG | NCRYPT_SILENT_FLAG,
                    )
                }
            }
        };
        let mut size = 0u32;
        hr_ok(call(std::ptr::null_mut(), 0, &mut size))?;
        let mut output = vec![0u8; size as usize];
        let mut written = 0u32;
        hr_ok(call(output.as_mut_ptr(), output.len() as u32, &mut written))?;
        output.truncate(written as usize);
        Ok(output)
    }

    /// Encrypts to the TPM key of the user with SID `sid`, then seals that with DPAPI under
    /// their account.
    pub struct TpmDpapiSealer {
        sid: String,
        version: TpmVersion,
    }

    impl TpmDpapiSealer {
        pub fn new(sid: &str, version: TpmVersion) -> TpmDpapiSealer {
            TpmDpapiSealer {
                sid: sid.to_string(),
                version,
            }
        }

        /// The sealer of the computer's TPM, if it can seal and open for this user now: its
        /// provider opens, it reports a TPM version DSA knows, the user's key is there or can
        /// be made, and a test secret goes through it and back. `Err` says why not.
        pub fn check(sid: &str) -> Result<TpmDpapiSealer, String> {
            let provider =
                provider().map_err(|hr| unusable_reason("opening the TPM's key provider", hr))?;
            let version = platform_version(&provider).ok_or_else(|| {
                "the TPM's key provider reports no TPM version DSA knows, so this computer has \
                 no TPM that DSA can use"
                    .to_string()
            })?;
            let key = TpmKey::of(sid)
                .open_or_create(&provider)
                .map_err(|hr| unusable_reason("making the TPM key", hr))?;
            let probe = [0x5Au8; 32];
            let sealed = rsa(&key, version, &probe, true)
                .map_err(|hr| unusable_reason("encrypting to the TPM key", hr))?;
            let opened = rsa(&key, version, &sealed, false)
                .map_err(|hr| unusable_reason("decrypting with the TPM key", hr))?;
            if opened != probe {
                return Err(format!(
                    "the {} returned other bytes than it was given, so DSA does not use it",
                    version.label()
                ));
            }
            Ok(TpmDpapiSealer::new(sid, version))
        }
    }

    impl Sealer for TpmDpapiSealer {
        fn name(&self) -> &str {
            self.version.sealer()
        }

        fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let failed = |step: &str, hr: i32| {
                KeyError::Failed(format!(
                    "the computer's {} could not seal the {label}: {}",
                    self.version.label(),
                    unusable_reason(step, hr)
                ))
            };
            let provider = provider().map_err(|hr| failed("opening the TPM's key provider", hr))?;
            let key = TpmKey::of(&self.sid)
                .open_or_create(&provider)
                .map_err(|hr| failed("making the TPM key", hr))?;
            let inner = rsa(&key, self.version, plain, true)
                .map_err(|hr| failed("encrypting to the TPM key", hr))?;
            dpapi_protect(&self.sid, &inner, &dpapi_entropy(self.name(), label), label)
        }

        fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let inner =
                dpapi_unprotect(&self.sid, sealed, &dpapi_entropy(self.name(), label), label)?;
            let provider = provider()
                .map_err(|hr| unseal_error(label, "opening the TPM's key provider", hr))?;
            let key = TpmKey::of(&self.sid)
                .open(&provider)
                .map_err(|hr| unseal_error(label, "opening the TPM key", hr))?;
            rsa(&key, self.version, &inner, false)
                .map_err(|hr| unseal_error(label, "decrypting with the TPM key", hr))
        }

        fn kind(&self) -> &str {
            "tpm"
        }

        fn hardware(&self) -> bool {
            true
        }

        fn description(&self) -> String {
            format!(
                "the computer's {} and Windows DPAPI under your account",
                self.version.label()
            )
        }
    }

    /// The sealers of the user with SID `sid` on this computer.
    pub struct WindowsSealers {
        sid: String,
    }

    impl WindowsSealers {
        pub fn new(sid: &str) -> WindowsSealers {
            WindowsSealers {
                sid: sid.to_string(),
            }
        }
    }

    impl SealerSource for WindowsSealers {
        fn hardware(&self, _: KeyPurpose) -> Hardware {
            match TpmDpapiSealer::check(&self.sid) {
                Ok(sealer) => Hardware::Usable(Arc::new(sealer)),
                Err(why) => Hardware::Unusable(why),
            }
        }

        fn fallback(&self, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
            Ok(Arc::new(DpapiSealer::new(&self.sid)))
        }

        fn named(&self, name: &str, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
            sealer_named(
                name,
                purpose,
                || Arc::new(DpapiSealer::new(&self.sid)),
                |version| Arc::new(TpmDpapiSealer::new(&self.sid, version)),
            )
        }
    }
}

/// A user's [`SealedKeys`] in their own folder `dir`: every folder down to `dir` is held and
/// the sealed files are checked as plain files while the service reads or writes them (see
/// `crate::safe_fs`).
#[cfg(windows)]
pub struct UserKeys {
    inner: SealedKeys,
}

#[cfg(windows)]
impl UserKeys {
    pub fn dir(&self) -> &Path {
        self.inner.dir()
    }

    /// The file the secret for `purpose` is sealed in.
    pub fn path(&self, purpose: KeyPurpose) -> std::path::PathBuf {
        self.inner.path(purpose)
    }

    fn guard(
        &self,
        purpose: KeyPurpose,
        create_dir: bool,
    ) -> Result<Vec<crate::safe_fs::GuardedDir>, KeyError> {
        let dir = self.inner.dir();
        if !create_dir && std::fs::symlink_metadata(dir).is_err() {
            if let Some(root) = dir
                .parent()
                .filter(|root| std::fs::symlink_metadata(root).is_ok())
            {
                crate::safe_fs::guard_below(root, root).map_err(KeyError::Failed)?;
            }
            return Ok(Vec::new());
        }
        if create_dir {
            if let Some(root) = dir
                .parent()
                .filter(|root| std::fs::symlink_metadata(root).is_ok())
            {
                crate::safe_fs::guard_below(root, root).map_err(KeyError::Failed)?;
            }
            std::fs::create_dir_all(dir).map_err(|e| {
                KeyError::Failed(format!(
                    "the {} cannot be kept in {}: {e}",
                    purpose.description(),
                    dir.display()
                ))
            })?;
        }
        let root = dir
            .parent()
            .filter(|root| std::fs::symlink_metadata(root).is_ok())
            .unwrap_or(dir);
        let path = self.inner.path(purpose);
        let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
        temp_name.push(".new");
        let temporary = path.with_file_name(temp_name);
        crate::safe_fs::guard_files(root, &[&path, &temporary]).map_err(KeyError::Failed)
    }
}

#[cfg(windows)]
impl dsa_host_core::host_keys::HostKeyStore for UserKeys {
    fn load(
        &self,
        purpose: KeyPurpose,
    ) -> Result<Option<dsa_host_core::host_keys::Secret>, KeyError> {
        let _guard = self.guard(purpose, false)?;
        self.inner.load(purpose)
    }

    fn store(
        &self,
        purpose: KeyPurpose,
        secret: &dsa_host_core::host_keys::Secret,
    ) -> Result<(), KeyError> {
        let _guard = self.guard(purpose, true)?;
        self.inner.store(purpose, secret)
    }

    fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
        let _guard = self.guard(purpose, false)?;
        self.inner.remove(purpose)
    }

    fn protection(
        &self,
        purpose: KeyPurpose,
    ) -> Result<Option<dsa_host_core::host_keys::KeyProtection>, KeyError> {
        let _guard = self.guard(purpose, false)?;
        self.inner.protection(purpose)
    }
}

/// The store of the user with SID `sid`, whose sealed files live in `dir`.
#[cfg(windows)]
pub fn for_user(sid: &str, dir: &Path) -> UserKeys {
    UserKeys {
        inner: SealedKeys::new(dir, Arc::new(WindowsSealers::new(sid))),
    }
}

/// A [`SealerSource`] of fixed sealers, for a computer whose TPM a test decides.
pub struct FixedSealers {
    pub tpm: Result<Arc<dyn Sealer>, String>,
    pub dpapi: Arc<dyn Sealer>,
}

impl SealerSource for FixedSealers {
    fn hardware(&self, _: KeyPurpose) -> Hardware {
        match &self.tpm {
            Ok(sealer) => Hardware::Usable(Arc::clone(sealer)),
            Err(why) => Hardware::Unusable(why.clone()),
        }
    }
    fn fallback(&self, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
        Ok(Arc::clone(&self.dpapi))
    }
    fn named(&self, name: &str, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
        let tpm = self.tpm.clone();
        sealer_named(
            name,
            purpose,
            || Arc::clone(&self.dpapi),
            move |_| match tpm {
                Ok(sealer) => sealer,
                Err(why) => Arc::new(GoneTpm(why)),
            },
        )
    }
}

/// A TPM that is not there any more: what it sealed does not open.
struct GoneTpm(String);

impl Sealer for GoneTpm {
    fn name(&self) -> &str {
        TPM_DPAPI
    }
    fn seal(&self, _: &[u8], _: &str) -> Result<Vec<u8>, KeyError> {
        Err(KeyError::Unavailable(self.0.clone()))
    }
    fn unseal(&self, _: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        Err(unseal_error(
            label,
            "opening the TPM key",
            NTE_BAD_KEYSET as i32,
        ))
    }
    fn hardware(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::host_keys::{load_or_create, HostKeyStore, Secret};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// A sealer that records its name in what it seals, and opens only its own.
    struct NamedSealer {
        name: &'static str,
        hardware: bool,
    }

    impl Sealer for NamedSealer {
        fn name(&self) -> &str {
            self.name
        }
        fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let mut out = format!("{}:{label}:", self.name).into_bytes();
            out.extend_from_slice(plain);
            Ok(out)
        }
        fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
            let head = format!("{}:{label}:", self.name).into_bytes();
            sealed
                .strip_prefix(head.as_slice())
                .map(<[u8]>::to_vec)
                .ok_or_else(|| KeyError::Unreadable("not sealed here".into()))
        }
        fn hardware(&self) -> bool {
            self.hardware
        }
    }

    /// A computer whose TPM a test turns on, off, or clears.
    struct FakeComputer {
        tpm: Mutex<Result<(), String>>,
    }

    impl FakeComputer {
        fn sealers(&self) -> FixedSealers {
            FixedSealers {
                tpm: self.tpm.lock().unwrap().clone().map(|()| {
                    Arc::new(NamedSealer {
                        name: TPM_DPAPI,
                        hardware: true,
                    }) as Arc<dyn Sealer>
                }),
                dpapi: Arc::new(NamedSealer {
                    name: DPAPI,
                    hardware: false,
                }),
            }
        }
    }

    impl SealerSource for FakeComputer {
        fn hardware(&self, purpose: KeyPurpose) -> Hardware {
            self.sealers().hardware(purpose)
        }
        fn fallback(&self, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
            self.sealers().fallback(purpose)
        }
        fn named(&self, name: &str, purpose: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
            self.sealers().named(name, purpose)
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-keys-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn store(dir: &Path, tpm: Result<(), String>) -> (SealedKeys, Arc<FakeComputer>) {
        let computer = Arc::new(FakeComputer {
            tpm: Mutex::new(tpm),
        });
        (SealedKeys::new(dir, computer.clone()), computer)
    }

    fn sealer(keys: &SealedKeys, purpose: KeyPurpose) -> String {
        let data = std::fs::read(keys.path(purpose)).unwrap();
        sealer_of(&data).unwrap().to_string()
    }

    #[test]
    fn fr36_keymint_root_key_is_sealed_by_the_tpm_first_and_the_backup_key_by_dpapi() {
        let dir = scratch("choice");
        let (keys, _) = store(&dir, Ok(()));
        load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        load_or_create(&keys, KeyPurpose::Backup).unwrap();
        assert_eq!(sealer(&keys, KeyPurpose::KeyMintRoot), TPM_DPAPI);
        assert_eq!(sealer(&keys, KeyPurpose::Backup), DPAPI);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_without_a_usable_tpm_dpapi_seals_and_the_tpm_takes_it_over_later() {
        let dir = scratch("appears");
        let no_tpm = unusable_reason("opening the TPM's key provider", 0x8028_400F_u32 as i32);
        let (keys, computer) = store(&dir, Err(no_tpm.clone()));
        let first = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        assert_eq!(sealer(&keys, KeyPurpose::KeyMintRoot), DPAPI);
        let protection = keys.protection(KeyPurpose::KeyMintRoot).unwrap().unwrap();
        assert!(!protection.hardware);
        assert_eq!(protection.fallback_reason, no_tpm);
        *computer.tpm.lock().unwrap() = Ok(());
        let again = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        assert_eq!(first, again, "the kept key, sealed by the TPM now");
        assert_eq!(sealer(&keys, KeyPurpose::KeyMintRoot), TPM_DPAPI);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_a_locked_out_tpm_is_waited_for_and_a_cleared_one_is_told_never_replaced() {
        let dir = scratch("gone");
        let (keys, computer) = store(&dir, Ok(()));
        load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap();
        let before = std::fs::read(keys.path(KeyPurpose::KeyMintRoot)).unwrap();
        *computer.tpm.lock().unwrap() = Err("cleared".into());
        let error = load_or_create(&keys, KeyPurpose::KeyMintRoot).unwrap_err();
        match &error {
            KeyError::Unreadable(why) => assert!(why.contains("cleared"), "{why}"),
            other => panic!("expected unreadable, got {other:?}"),
        }
        assert!(keys
            .store(KeyPurpose::KeyMintRoot, &Secret::from_bytes([1; 32]))
            .is_err());
        assert_eq!(
            std::fs::read(keys.path(KeyPurpose::KeyMintRoot)).unwrap(),
            before
        );
        // What each TPM failure becomes when a sealed key is opened.
        let locked = unseal_error("keymint-root-key", "decrypting", 0x8028_0921_u32 as i32);
        assert!(matches!(&locked, KeyError::Unavailable(why) if why.contains("locked out")));
        let locked12 = unseal_error("keymint-root-key", "decrypting", 0x8028_0803_u32 as i32);
        assert!(matches!(locked12, KeyError::Unavailable(_)));
        let off = unseal_error("keymint-root-key", "opening", 0x8028_400F_u32 as i32);
        assert!(matches!(&off, KeyError::Unreadable(why) if why.contains("turn the TPM on")));
        let busy = unseal_error("keymint-root-key", "decrypting", 0x8028_0922_u32 as i32);
        assert!(matches!(busy, KeyError::Unavailable(_)));
        let odd = unseal_error("keymint-root-key", "decrypting", 0x8007_0005_u32 as i32);
        assert!(
            matches!(odd, KeyError::Unavailable(_)),
            "never taken for gone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr36_tpm_errors_are_told_apart() {
        for code in [
            0x8009_0035u32,
            0x8009_0030,
            0x8009_001E,
            0x8028_400F,
            0x8028_4010,
        ] {
            assert_eq!(tpm_problem(code as i32), TpmProblem::Absent, "{code:08X}");
            assert!(tpm_unusable(code as i32), "{code:08X}");
        }
        assert_eq!(tpm_problem(0x8028_0921_u32 as i32), TpmProblem::LockedOut);
        assert_eq!(tpm_problem(0x8028_0803_u32 as i32), TpmProblem::LockedOut);
        assert_eq!(tpm_problem(0x8028_0922_u32 as i32), TpmProblem::NotReady);
        assert_eq!(tpm_problem(0x8009_0016_u32 as i32), TpmProblem::KeyGone);
        for code in [0x8007_0005u32, 0x8009_0036] {
            assert_eq!(tpm_problem(code as i32), TpmProblem::Failed, "{code:08X}");
            assert!(!tpm_unusable(code as i32), "{code:08X}");
        }
        assert!(unusable_reason("x", 0x8028_0921_u32 as i32).contains("locked out"));
        assert!(unusable_reason("x", 0x8028_400F_u32 as i32).contains("no TPM"));
    }

    #[test]
    fn fr36_a_tpm_12_is_used_with_its_own_padding_and_named_apart() {
        assert_eq!(
            tpm_version("TPM-Version:2.0 -Level:0-Revision:1.38-VendorID:'INTC'-Firmware:600.18"),
            Some(TpmVersion::V20)
        );
        assert_eq!(
            tpm_version("TPM-Version:1.2 -Level:2-Revision:116-VendorID:'IFX '"),
            Some(TpmVersion::V12)
        );
        assert_eq!(tpm_version("something else"), None);
        assert_eq!(TpmVersion::V12.sealer(), TPM12_DPAPI);
        assert_eq!(TpmVersion::of_sealer(TPM12_DPAPI), Some(TpmVersion::V12));
        assert_eq!(TpmVersion::of_sealer(TPM_DPAPI), Some(TpmVersion::V20));
        assert_eq!(TpmVersion::of_sealer(DPAPI), None);
        let dpapi = || -> Arc<dyn Sealer> {
            Arc::new(NamedSealer {
                name: DPAPI,
                hardware: false,
            })
        };
        let opened = sealer_named(TPM12_DPAPI, KeyPurpose::KeyMintRoot, dpapi, |version| {
            assert_eq!(version, TpmVersion::V12);
            Arc::new(NamedSealer {
                name: TPM12_DPAPI,
                hardware: true,
            })
        })
        .unwrap();
        assert_eq!(opened.name(), TPM12_DPAPI);
        assert!(sealer_named("tpm3", KeyPurpose::KeyMintRoot, dpapi, |_| dpapi()).is_err());
    }

    #[test]
    fn fr36_each_user_has_a_tpm_key_of_their_own_and_each_seal_its_own_entropy() {
        assert_ne!(
            tpm_key_name("S-1-5-21-1-2-3-1001"),
            tpm_key_name("S-1-5-21-1-2-3-1002")
        );
        assert!(tpm_key_name("S-1-5-21-1-2-3-1001").ends_with("S-1-5-21-1-2-3-1001"));
        assert_ne!(
            dpapi_entropy(DPAPI, "backup-key"),
            dpapi_entropy(TPM_DPAPI, "backup-key")
        );
        assert_ne!(
            dpapi_entropy(TPM_DPAPI, "keymint-root-key"),
            dpapi_entropy(TPM12_DPAPI, "keymint-root-key")
        );
        assert_ne!(
            dpapi_entropy(DPAPI, "backup-key"),
            dpapi_entropy(DPAPI, "keymint-root-key")
        );
    }
}

//! The KeyMint root key sealed by the computer's TPM 2.0 on Linux.
//!
//! The service runs as root and talks to the TPM through the kernel's resource manager
//! (`/dev/tpmrm0`), which gives each open file its own handles and flushes them when it closes,
//! so no other TPM user is disturbed. Each seal or unseal:
//!
//! 1. makes the storage root key under the owner hierarchy (`TPM2_CreatePrimary` with the ECC
//!    NIST P-256 template of the TCG's provisioning guidance, the same each time, so the TPM
//!    makes the same key again from its owner seed; nothing is kept in the TPM's memory);
//! 2. seals: `TPM2_Create` of a sealed data object under it, `fixedTPM` and `fixedParent` (it
//!    never leaves this TPM, under this key), `userWithAuth` with an empty password and `noDA`
//!    (so the TPM's dictionary attack lockout never locks DSA's key away). The secret, with
//!    what it is for after it, is the object's data; the object's private and public parts are
//!    what the sealed file keeps. The private part is encrypted by the TPM under the storage
//!    root key, so the file opens on no other TPM, and on this one no more once it is cleared
//!    (a clear replaces the owner seed);
//! 3. unseals: `TPM2_Load` of the two parts under the storage root key and `TPM2_Unseal`.
//!
//! The commands pass in the clear on the bus between the processor and the TPM: someone who
//! can watch that bus while the key is sealed or unsealed reads it. Root on the same computer
//! reads it anyway.
//!
//! The file is in the service's own folder, which only root reads, under the user's ID, so the
//! guest of one user is never given another's key.
//!
//! A TPM 1.2 speaks another protocol, which this host does not; it is told apart (from
//! `/sys/class/tpm/tpm0/tpm_version_major`) and the key is then kept by the user's Secret
//! Service, with that reason.
//!
//! The commands and answers are built and read by plain functions over [`TpmTransport`],
//! tested against a TPM in memory; the device builds on Linux only.

use dsa_host_core::host_keys::{KeyError, Sealer};

/// The kernel's TPM resource manager.
pub const DEVICE: &str = "/dev/tpmrm0";

/// Where the kernel says which TPM the computer has.
pub const VERSION_FILE: &str = "/sys/class/tpm/tpm0/tpm_version_major";

/// The sealer's name, as sealed files record it.
pub const SEALER: &str = "tpm2";

pub const ST_NO_SESSIONS: u16 = 0x8001;
pub const ST_SESSIONS: u16 = 0x8002;
pub const CC_CREATE_PRIMARY: u32 = 0x0000_0131;
pub const CC_CREATE: u32 = 0x0000_0153;
pub const CC_LOAD: u32 = 0x0000_0157;
pub const CC_UNSEAL: u32 = 0x0000_015E;
pub const CC_FLUSH_CONTEXT: u32 = 0x0000_0165;
pub const RH_OWNER: u32 = 0x4000_0001;
pub const RS_PW: u32 = 0x4000_0009;
pub const ALG_ECC: u16 = 0x0023;
pub const ALG_KEYEDHASH: u16 = 0x0008;
pub const ALG_SHA256: u16 = 0x000B;
pub const ALG_AES: u16 = 0x0006;
pub const ALG_CFB: u16 = 0x0043;
pub const ALG_NULL: u16 = 0x0010;
pub const ECC_NIST_P256: u16 = 0x0003;

/// `fixedTPM`, `fixedParent`, `sensitiveDataOrigin`, `userWithAuth`, `noDA`, `restricted`,
/// `decrypt`: a storage key.
pub const STORAGE_KEY_ATTRIBUTES: u32 =
    (1 << 1) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 10) | (1 << 16) | (1 << 17);
/// `fixedTPM`, `fixedParent`, `userWithAuth`, `noDA`: sealed data that never leaves this TPM.
pub const SEALED_DATA_ATTRIBUTES: u32 = (1 << 1) | (1 << 4) | (1 << 6) | (1 << 10);

/// The most a sealed data object holds.
pub const MAX_SEALED: usize = 128;

/// TPM_RC_LOCKOUT.
pub const RC_LOCKOUT: u32 = 0x0921;
/// TPM_RC_INITIALIZE: the TPM was not started.
pub const RC_INITIALIZE: u32 = 0x0100;

/// What a TPM answer means for DSA's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcMeaning {
    /// The TPM is locked out after too many wrong authorizations.
    LockedOut,
    /// Busy, testing itself, or not started: it works again soon.
    NotReady,
    /// The owner hierarchy has a password DSA does not know.
    OwnerPassword,
    /// What was sealed does not load under this TPM's storage root key: the TPM was cleared,
    /// or this is another computer.
    NotThisTpm,
    /// Anything else.
    Failed,
}

/// What response code `rc` means.
pub fn meaning(rc: u32) -> RcMeaning {
    if rc == RC_LOCKOUT {
        return RcMeaning::LockedOut;
    }
    if rc == RC_INITIALIZE {
        return RcMeaning::NotReady;
    }
    let format_one = rc & 0x80 != 0;
    if !format_one && rc & 0x900 == 0x900 {
        // The warnings: retry, testing, yielded, NV rate and the like.
        return RcMeaning::NotReady;
    }
    if format_one {
        match rc & 0xBF {
            // TPM_RC_AUTH_FAIL, TPM_RC_BAD_AUTH.
            0x8E | 0xA2 => return RcMeaning::OwnerPassword,
            // TPM_RC_INTEGRITY, TPM_RC_KEY, TPM_RC_BINDING, TPM_RC_SIZE of a sealed part.
            0x9F | 0x9C | 0xA5 | 0x95 => return RcMeaning::NotThisTpm,
            _ => {}
        }
    }
    RcMeaning::Failed
}

/// The text of a response code, for a person.
pub fn rc_text(rc: u32) -> String {
    let what = match meaning(rc) {
        RcMeaning::LockedOut => "locked out",
        RcMeaning::NotReady => "not ready",
        RcMeaning::OwnerPassword => "wrong owner authorization",
        RcMeaning::NotThisTpm => "not sealed by this TPM",
        RcMeaning::Failed => "failed",
    };
    format!("{what}, TPM response 0x{rc:03X}")
}

/// Sends one command to the TPM and gives its answer.
pub trait TpmTransport {
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, String>;
}

/// Why a TPM operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TpmError {
    /// The TPM answered with this response code to the command named.
    Rc(&'static str, u32),
    /// The device or the answer failed; the text says how.
    Io(String),
}

impl std::fmt::Display for TpmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TpmError::Rc(command, rc) => write!(f, "{command}: {}", rc_text(*rc)),
            TpmError::Io(why) => f.write_str(why),
        }
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// A sized buffer: its length in two bytes, then the bytes.
fn put_sized(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u16(out, bytes.len() as u16);
    out.extend_from_slice(bytes);
}

/// A command: the header, the handles, the password session (when `password`), the
/// parameters.
pub fn command(code: u32, handles: &[u32], password: bool, parameters: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(
        &mut out,
        if password {
            ST_SESSIONS
        } else {
            ST_NO_SESSIONS
        },
    );
    put_u32(&mut out, 0);
    put_u32(&mut out, code);
    for handle in handles {
        put_u32(&mut out, *handle);
    }
    if password {
        // TPMS_AUTH_COMMAND: the password session, no nonce, no attributes, the empty
        // password.
        let mut auth = Vec::new();
        put_u32(&mut auth, RS_PW);
        put_sized(&mut auth, &[]);
        auth.push(0);
        put_sized(&mut auth, &[]);
        put_u32(&mut out, auth.len() as u32);
        out.extend_from_slice(&auth);
    }
    out.extend_from_slice(parameters);
    let size = out.len() as u32;
    out[2..6].copy_from_slice(&size.to_be_bytes());
    out
}

/// The public part of the storage root key: ECC NIST P-256, AES-128-CFB, the TCG's template.
pub fn storage_key_template() -> Vec<u8> {
    let mut public = Vec::new();
    put_u16(&mut public, ALG_ECC);
    put_u16(&mut public, ALG_SHA256);
    put_u32(&mut public, STORAGE_KEY_ATTRIBUTES);
    put_sized(&mut public, &[]);
    put_u16(&mut public, ALG_AES);
    put_u16(&mut public, 128);
    put_u16(&mut public, ALG_CFB);
    put_u16(&mut public, ALG_NULL);
    put_u16(&mut public, ECC_NIST_P256);
    put_u16(&mut public, ALG_NULL);
    put_sized(&mut public, &[]);
    put_sized(&mut public, &[]);
    public
}

/// The public part of a sealed data object.
pub fn sealed_data_template() -> Vec<u8> {
    let mut public = Vec::new();
    put_u16(&mut public, ALG_KEYEDHASH);
    put_u16(&mut public, ALG_SHA256);
    put_u32(&mut public, SEALED_DATA_ATTRIBUTES);
    put_sized(&mut public, &[]);
    put_u16(&mut public, ALG_NULL);
    put_sized(&mut public, &[]);
    public
}

/// `inSensitive`, `inPublic`, `outsideInfo` and `creationPCR` of a create command.
fn create_parameters(data: &[u8], public: &[u8]) -> Vec<u8> {
    let mut sensitive = Vec::new();
    put_sized(&mut sensitive, &[]);
    put_sized(&mut sensitive, data);
    let mut parameters = Vec::new();
    put_sized(&mut parameters, &sensitive);
    put_sized(&mut parameters, public);
    put_sized(&mut parameters, &[]);
    put_u32(&mut parameters, 0);
    parameters
}

/// Reads an answer: its response code, and after the header what follows.
struct Answer<'a> {
    rest: &'a [u8],
}

impl<'a> Answer<'a> {
    fn of(command: &'static str, data: &'a [u8]) -> Result<Answer<'a>, TpmError> {
        if data.len() < 10 {
            return Err(TpmError::Io(format!(
                "{command}: the TPM's answer is cut short"
            )));
        }
        let size = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as usize;
        if size != data.len() {
            return Err(TpmError::Io(format!(
                "{command}: the TPM's answer says {size} bytes and has {}",
                data.len()
            )));
        }
        let rc = u32::from_be_bytes([data[6], data[7], data[8], data[9]]);
        if rc != 0 {
            return Err(TpmError::Rc(command, rc));
        }
        Ok(Answer { rest: &data[10..] })
    }

    fn u32(&mut self) -> Result<u32, TpmError> {
        let (head, rest) = self
            .rest
            .split_first_chunk::<4>()
            .ok_or_else(|| TpmError::Io("the TPM's answer is cut short".into()))?;
        self.rest = rest;
        Ok(u32::from_be_bytes(*head))
    }

    /// A sized buffer, with its two length bytes.
    fn sized_whole(&mut self) -> Result<&'a [u8], TpmError> {
        let short = || TpmError::Io("the TPM's answer is cut short".into());
        let length = u16::from_be_bytes(*self.rest.first_chunk::<2>().ok_or_else(short)?) as usize;
        if self.rest.len() < 2 + length {
            return Err(short());
        }
        let (whole, rest) = self.rest.split_at(2 + length);
        self.rest = rest;
        Ok(whole)
    }
}

/// Splits a sealed blob into the private and public parts, each with its length.
fn split_blob(blob: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let mut reader = Answer { rest: blob };
    let private = reader.sized_whole().map_err(|e| e.to_string())?;
    let public = reader.sized_whole().map_err(|e| e.to_string())?;
    if !reader.rest.is_empty() {
        return Err("the sealed key has bytes after its two parts".into());
    }
    Ok((private, public))
}

/// One conversation with the TPM, which flushes the handles it made.
struct Session<'t> {
    tpm: &'t mut dyn TpmTransport,
    loaded: Vec<u32>,
}

impl Session<'_> {
    fn ask(&mut self, name: &'static str, command: &[u8]) -> Result<Vec<u8>, TpmError> {
        self.tpm
            .transmit(command)
            .map_err(|e| TpmError::Io(format!("{name}: {e}")))
    }

    /// The storage root key's handle.
    fn storage_key(&mut self) -> Result<u32, TpmError> {
        let parameters = create_parameters(&[], &storage_key_template());
        let answer = self.ask(
            "making the storage root key",
            &command(CC_CREATE_PRIMARY, &[RH_OWNER], true, &parameters),
        )?;
        let handle = Answer::of("making the storage root key", &answer)?.u32()?;
        self.loaded.push(handle);
        Ok(handle)
    }

    fn seal(&mut self, data: &[u8]) -> Result<Vec<u8>, TpmError> {
        let parent = self.storage_key()?;
        let parameters = create_parameters(data, &sealed_data_template());
        let answer = self.ask(
            "sealing the key",
            &command(CC_CREATE, &[parent], true, &parameters),
        )?;
        let mut reader = Answer::of("sealing the key", &answer)?;
        let _parameter_size = reader.u32()?;
        let private = reader.sized_whole()?;
        let public = reader.sized_whole()?;
        Ok([private, public].concat())
    }

    fn unseal(&mut self, blob: &[u8]) -> Result<Vec<u8>, TpmError> {
        let (private, public) = split_blob(blob).map_err(TpmError::Io)?;
        let parent = self.storage_key()?;
        let parameters = [private, public].concat();
        let answer = self.ask(
            "loading the sealed key",
            &command(CC_LOAD, &[parent], true, &parameters),
        )?;
        let object = Answer::of("loading the sealed key", &answer)?.u32()?;
        self.loaded.push(object);
        let answer = self.ask(
            "unsealing the key",
            &command(CC_UNSEAL, &[object], true, &[]),
        )?;
        let mut reader = Answer::of("unsealing the key", &answer)?;
        let _parameter_size = reader.u32()?;
        let data = reader.sized_whole()?;
        Ok(data[2..].to_vec())
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        for handle in std::mem::take(&mut self.loaded).into_iter().rev() {
            let mut parameters = Vec::new();
            put_u32(&mut parameters, handle);
            let _ = self
                .tpm
                .transmit(&command(CC_FLUSH_CONTEXT, &[], false, &parameters));
        }
    }
}

/// Makes the storage root key and lets it go: whether the TPM can seal for DSA now.
pub fn probe(tpm: &mut dyn TpmTransport) -> Result<(), TpmError> {
    let mut session = Session {
        tpm,
        loaded: Vec::new(),
    };
    session.storage_key().map(|_| ())
}

/// Seals `plain` for `label`; the blob opens only on this TPM.
pub fn seal(tpm: &mut dyn TpmTransport, plain: &[u8], label: &str) -> Result<Vec<u8>, TpmError> {
    let data = [plain, label.as_bytes()].concat();
    if data.len() > MAX_SEALED {
        return Err(TpmError::Io(format!(
            "{} bytes is more than a sealed object holds",
            data.len()
        )));
    }
    let mut session = Session {
        tpm,
        loaded: Vec::new(),
    };
    session.seal(&data)
}

/// Opens what [`seal`] sealed for `label`.
pub fn unseal(tpm: &mut dyn TpmTransport, blob: &[u8], label: &str) -> Result<Vec<u8>, TpmError> {
    let mut session = Session {
        tpm,
        loaded: Vec::new(),
    };
    let mut data = session.unseal(blob)?;
    match data.strip_suffix(label.as_bytes()) {
        Some(plain) => {
            let plain = plain.to_vec();
            data.iter_mut().for_each(|b| *b = 0);
            Ok(plain)
        }
        None => Err(TpmError::Io(format!("it was not sealed as the {label}"))),
    }
}

/// Whether the computer's TPM can be used, from what the kernel says of it: `version` is
/// the content of [`VERSION_FILE`] (`None` when there is no such file), `device` whether
/// [`DEVICE`] is there. `Err` says why not.
pub fn usable(version: Option<&str>, device: bool) -> Result<(), String> {
    match version.map(str::trim) {
        Some("2") if device => Ok(()),
        Some("2") => Err(format!(
            "the kernel has the computer's TPM 2.0 but no resource manager for it ({DEVICE}); \
             a kernel with CONFIG_TCG_TPM 5.x and tpm_tis or tpm_crb provides one"
        )),
        Some("1") => Err(
            "this computer has a TPM 1.2, which DSA uses only on Windows; on Linux it seals \
             with a TPM 2.0"
                .into(),
        ),
        Some(other) => Err(format!(
            "the kernel reports a TPM version DSA does not know ({other})"
        )),
        None if device => Ok(()),
        None => Err(
            "this computer has no TPM the kernel knows of (no /dev/tpmrm0); turn the TPM on \
             in the computer's firmware settings"
                .into(),
        ),
    }
}

/// Why the TPM cannot seal now, after `error`, for a person.
pub fn unusable_reason(error: &TpmError) -> String {
    match error {
        TpmError::Rc(_, rc) => match meaning(*rc) {
            RcMeaning::LockedOut => format!(
                "the TPM is locked out after too many wrong authorizations ({error}); it lifts \
                 the lockout after a while, and DSA seals the key with it at the next start"
            ),
            RcMeaning::NotReady => format!(
                "the TPM was not ready ({error}); DSA seals the key with it at the next start"
            ),
            RcMeaning::OwnerPassword => format!(
                "the TPM's owner hierarchy has a password, so DSA cannot make its storage root \
                 key ({error})"
            ),
            RcMeaning::NotThisTpm | RcMeaning::Failed => {
                format!("the TPM failed ({error}); DSA tries it again at the next start")
            }
        },
        TpmError::Io(why) => format!("the TPM cannot be reached ({why})"),
    }
}

/// What a failed unseal means for the key labelled `label`.
pub fn unseal_error(label: &str, error: &TpmError) -> KeyError {
    match error {
        TpmError::Rc(_, rc) => match meaning(*rc) {
            RcMeaning::LockedOut => KeyError::Unavailable(format!(
                "the TPM that sealed the {label} is locked out ({error}); it opens again once \
                 the lockout ends, at a later start"
            )),
            RcMeaning::NotReady => KeyError::Unavailable(format!(
                "the TPM that sealed the {label} was not ready ({error}); it is tried again at \
                 the next start"
            )),
            RcMeaning::NotThisTpm => KeyError::Unreadable(format!(
                "the {label} was sealed by a TPM this one is not: the TPM was cleared, or this \
                 is not the computer it was sealed on ({error})"
            )),
            RcMeaning::OwnerPassword => KeyError::Unavailable(format!(
                "the TPM's owner hierarchy has a password now, so the {label} cannot be opened \
                 ({error}); remove the password to open it again"
            )),
            RcMeaning::Failed => KeyError::Unavailable(format!(
                "the TPM could not open the {label} ({error}); it is tried again at the next \
                 start"
            )),
        },
        TpmError::Io(why) => KeyError::Unavailable(format!(
            "the TPM that sealed the {label} cannot be reached ({why}); turn it on again in the \
             computer's firmware settings if it was turned off"
        )),
    }
}

/// Opens the TPM for one operation.
pub type OpenTpm = Box<dyn Fn() -> Result<Box<dyn TpmTransport>, String> + Send + Sync>;

/// Seals with the computer's TPM 2.0.
pub struct Tpm2Sealer {
    open: OpenTpm,
}

impl Tpm2Sealer {
    pub fn new(open: OpenTpm) -> Tpm2Sealer {
        Tpm2Sealer { open }
    }

    fn device(&self) -> Result<Box<dyn TpmTransport>, TpmError> {
        (self.open)().map_err(TpmError::Io)
    }
}

impl Sealer for Tpm2Sealer {
    fn name(&self) -> &str {
        SEALER
    }

    fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        let failed =
            |e: TpmError| KeyError::Failed(format!("the TPM could not seal the {label}: {e}"));
        let mut tpm = self.device().map_err(failed)?;
        seal(tpm.as_mut(), plain, label).map_err(failed)
    }

    fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        let mut tpm = self.device().map_err(|e| unseal_error(label, &e))?;
        unseal(tpm.as_mut(), sealed, label).map_err(|e| unseal_error(label, &e))
    }

    fn kind(&self) -> &str {
        "tpm"
    }

    fn hardware(&self) -> bool {
        true
    }

    fn description(&self) -> String {
        "the computer's TPM 2.0".into()
    }
}

#[cfg(target_os = "linux")]
pub use device::open_device;

#[cfg(target_os = "linux")]
mod device {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};

    use super::{TpmTransport, DEVICE};

    /// The kernel's resource manager, one open file for one operation.
    struct TpmDevice(File);

    impl TpmTransport for TpmDevice {
        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, String> {
            self.0
                .write_all(command)
                .map_err(|e| format!("writing to {DEVICE}: {e}"))?;
            // The resource manager answers a command in one read.
            let mut answer = vec![0u8; 4096];
            let read = self
                .0
                .read(&mut answer)
                .map_err(|e| format!("reading from {DEVICE}: {e}"))?;
            answer.truncate(read);
            Ok(answer)
        }
    }

    /// Opens [`DEVICE`].
    pub fn open_device() -> Result<Box<dyn TpmTransport>, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(DEVICE)
            .map_err(|e| format!("{DEVICE}: {e}"))?;
        Ok(Box::new(TpmDevice(file)))
    }
}

#[cfg(test)]
pub mod fake {
    //! A TPM in memory that does what DSA asks of one: it makes a storage root key from its
    //! owner seed, seals data under it (the private part bound to the seed), loads, unseals
    //! and flushes. A clear gives it a new seed; it can be locked out.

    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct FakeTpmState {
        pub seed: u8,
        pub locked_out: bool,
        pub owner_password: bool,
        pub handles: HashMap<u32, Vec<u8>>,
        pub next: u32,
        pub flushed: Vec<u32>,
        pub commands: Vec<u32>,
    }

    #[derive(Clone, Default)]
    pub struct FakeTpm(pub Arc<Mutex<FakeTpmState>>);

    fn answer(rc: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_u16(&mut out, ST_SESSIONS);
        put_u32(&mut out, (10 + body.len()) as u32);
        put_u32(&mut out, rc);
        out.extend_from_slice(body);
        out
    }

    fn sized(data: &[u8]) -> (&[u8], &[u8]) {
        let length = u16::from_be_bytes([data[0], data[1]]) as usize;
        (&data[2..2 + length], &data[2 + length..])
    }

    impl TpmTransport for FakeTpm {
        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, String> {
            let mut state = self.0.lock().unwrap();
            let size = u32::from_be_bytes(command[2..6].try_into().unwrap()) as usize;
            assert_eq!(size, command.len(), "the command's size is its length");
            let code = u32::from_be_bytes(command[6..10].try_into().unwrap());
            state.commands.push(code);
            if code == CC_FLUSH_CONTEXT {
                let handle = u32::from_be_bytes(command[10..14].try_into().unwrap());
                state.handles.remove(&handle);
                state.flushed.push(handle);
                return Ok(answer(0, &[]));
            }
            assert_eq!(
                u16::from_be_bytes([command[0], command[1]]),
                ST_SESSIONS,
                "every other command has the password session"
            );
            let handle = u32::from_be_bytes(command[10..14].try_into().unwrap());
            let auth_size = u32::from_be_bytes(command[14..18].try_into().unwrap()) as usize;
            assert_eq!(
                u32::from_be_bytes(command[18..22].try_into().unwrap()),
                RS_PW
            );
            let parameters = &command[18 + auth_size..];
            if state.locked_out {
                return Ok(answer(RC_LOCKOUT, &[]));
            }
            let new_handle = |state: &mut FakeTpmState, what: Vec<u8>| {
                state.next += 1;
                let handle = 0x8000_0000 + state.next;
                state.handles.insert(handle, what);
                handle
            };
            match code {
                CC_CREATE_PRIMARY => {
                    assert_eq!(handle, RH_OWNER);
                    if state.owner_password {
                        return Ok(answer(0x9A2, &[]));
                    }
                    let (_, rest) = sized(parameters);
                    let (public, _) = sized(rest);
                    assert_eq!(public, storage_key_template().as_slice());
                    let seed = state.seed;
                    let primary = new_handle(&mut state, vec![b'P', seed]);
                    Ok(answer(0, &primary.to_be_bytes()))
                }
                CC_CREATE => {
                    let parent = state.handles[&handle].clone();
                    let (sensitive, rest) = sized(parameters);
                    let (public, _) = sized(rest);
                    assert_eq!(public, sealed_data_template().as_slice());
                    let (_, rest) = sized(sensitive);
                    let (data, _) = sized(rest);
                    // The private part: the data under the parent's seed.
                    let mut private = vec![parent[1]];
                    private.extend(data.iter().map(|b| b ^ parent[1] ^ 0xA5));
                    let mut body = Vec::new();
                    let mut params = Vec::new();
                    put_sized(&mut params, &private);
                    put_sized(&mut params, public);
                    put_u32(&mut body, params.len() as u32);
                    body.extend(params);
                    Ok(answer(0, &body))
                }
                CC_LOAD => {
                    let parent = state.handles[&handle].clone();
                    let (private, rest) = sized(parameters);
                    let (public, _) = sized(rest);
                    assert_eq!(public, sealed_data_template().as_slice());
                    if private.first() != Some(&parent[1]) {
                        // TPM_RC_INTEGRITY of the first parameter.
                        return Ok(answer(0x1DF, &[]));
                    }
                    let data: Vec<u8> = private[1..].iter().map(|b| b ^ parent[1] ^ 0xA5).collect();
                    let mut stored = vec![b'O'];
                    stored.extend(data);
                    let object = new_handle(&mut state, stored);
                    let mut body = object.to_be_bytes().to_vec();
                    put_u32(&mut body, 2);
                    put_sized(&mut body, &[]);
                    Ok(answer(0, &body))
                }
                CC_UNSEAL => {
                    let object = state.handles[&handle].clone();
                    let mut body = Vec::new();
                    let mut params = Vec::new();
                    put_sized(&mut params, &object[1..]);
                    put_u32(&mut body, params.len() as u32);
                    body.extend(params);
                    Ok(answer(0, &body))
                }
                other => panic!("an unexpected command {other:#x}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeTpm;
    use super::*;

    #[test]
    fn fr36_a_key_sealed_by_the_tpm_opens_on_it_and_its_handles_are_flushed() {
        let mut tpm = FakeTpm::default();
        let sealed = seal(&mut tpm, &[7; 32], "keymint-root-key").unwrap();
        assert!(
            !sealed.windows(32).any(|w| w == [7; 32]),
            "sealed in the clear"
        );
        assert_eq!(
            unseal(&mut tpm, &sealed, "keymint-root-key").unwrap(),
            vec![7; 32]
        );
        let state = tpm.0.lock().unwrap();
        assert!(state.handles.is_empty(), "every handle made is flushed");
        assert_eq!(state.flushed.len(), 3);
        assert_eq!(
            &state.commands[..2],
            &[CC_CREATE_PRIMARY, CC_CREATE],
            "sealing makes the storage root key, then the sealed object"
        );
    }

    #[test]
    fn fr36_a_key_sealed_for_one_purpose_does_not_open_as_another() {
        let mut tpm = FakeTpm::default();
        let sealed = seal(&mut tpm, &[7; 32], "backup-key").unwrap();
        assert!(unseal(&mut tpm, &sealed, "keymint-root-key").is_err());
    }

    #[test]
    fn fr36_a_cleared_tpm_opens_nothing_it_sealed_before_and_says_so() {
        let mut tpm = FakeTpm::default();
        let sealed = seal(&mut tpm, &[7; 32], "keymint-root-key").unwrap();
        tpm.0.lock().unwrap().seed = 9;
        let error = unseal(&mut tpm, &sealed, "keymint-root-key").unwrap_err();
        assert_eq!(error, TpmError::Rc("loading the sealed key", 0x1DF));
        match unseal_error("KeyMint root key", &error) {
            KeyError::Unreadable(why) => assert!(why.contains("cleared"), "{why}"),
            other => panic!("expected unreadable, got {other:?}"),
        }
        assert!(tpm.0.lock().unwrap().handles.is_empty());
    }

    #[test]
    fn fr36_a_locked_out_tpm_is_waited_for_and_an_owner_password_is_told() {
        let mut tpm = FakeTpm::default();
        let sealed = seal(&mut tpm, &[7; 32], "keymint-root-key").unwrap();
        tpm.0.lock().unwrap().locked_out = true;
        let error = unseal(&mut tpm, &sealed, "keymint-root-key").unwrap_err();
        assert!(matches!(
            unseal_error("KeyMint root key", &error),
            KeyError::Unavailable(why) if why.contains("locked out")
        ));
        assert!(unusable_reason(&probe(&mut tpm).unwrap_err()).contains("locked out"));
        let mut owned = FakeTpm::default();
        owned.0.lock().unwrap().owner_password = true;
        assert!(unusable_reason(&probe(&mut owned).unwrap_err()).contains("password"));
    }

    #[test]
    fn fr36_tpm_answers_are_told_apart() {
        assert_eq!(meaning(0x921), RcMeaning::LockedOut);
        assert_eq!(meaning(0x922), RcMeaning::NotReady);
        assert_eq!(meaning(0x90A), RcMeaning::NotReady);
        assert_eq!(meaning(0x100), RcMeaning::NotReady);
        assert_eq!(meaning(0x9A2), RcMeaning::OwnerPassword);
        assert_eq!(meaning(0x98E), RcMeaning::OwnerPassword);
        assert_eq!(meaning(0x1DF), RcMeaning::NotThisTpm);
        assert_eq!(meaning(0x101), RcMeaning::Failed);
    }

    #[test]
    fn fr36_only_a_tpm_20_with_its_resource_manager_is_used() {
        assert_eq!(usable(Some("2\n"), true), Ok(()));
        assert!(usable(Some("2"), false)
            .unwrap_err()
            .contains("resource manager"));
        assert!(usable(Some("1"), true).unwrap_err().contains("TPM 1.2"));
        assert!(usable(None, false).unwrap_err().contains("no TPM"));
        assert_eq!(usable(None, true), Ok(()));
    }

    #[test]
    fn fr36_commands_are_laid_out_as_the_tpm_reads_them() {
        let flush = command(CC_FLUSH_CONTEXT, &[], false, &0x8000_0001u32.to_be_bytes());
        assert_eq!(
            flush,
            vec![0x80, 0x01, 0, 0, 0, 14, 0, 0, 1, 0x65, 0x80, 0, 0, 1]
        );
        let unseal = command(CC_UNSEAL, &[0x8000_0002], true, &[]);
        assert_eq!(
            unseal,
            vec![
                0x80, 0x02, 0, 0, 0, 27, 0, 0, 1, 0x5E, 0x80, 0, 0, 2, // header, handle
                0, 0, 0, 9, 0x40, 0, 0, 9, 0, 0, 0, 0, 0, // the password session
            ]
        );
        assert_eq!(STORAGE_KEY_ATTRIBUTES, 0x0003_0472);
        assert_eq!(SEALED_DATA_ATTRIBUTES, 0x0000_0452);
    }
}

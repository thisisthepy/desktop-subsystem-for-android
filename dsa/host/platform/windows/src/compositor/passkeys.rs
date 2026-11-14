//! Passkeys for the apps in the user's Android, made and used by Windows Hello.
//!
//! The service checks each passkey request of the user's Android (the relying party, and the
//! app's signature against the site's Digital Asset Links) and hands only the requests that
//! pass to the user's desktop session. The window manager runs there, as the user, so it
//! watches those ceremonies over the local API ([`serve`]) and performs each one with
//! Windows' own WebAuthn API (`webauthn.dll`, [`WindowsHello`]): Windows Hello asks the person
//! (face, fingerprint or PIN) and names the relying party, and the passkey is the user's
//! Windows passkey, the same one their browsers use.
//!
//! Each ceremony runs on a thread of its own, so the watch goes on while Windows Hello is up.
//! What became of it goes back to Android over a new connection: the credential made or the
//! signature, or why there is none (canceled, not allowed, unavailable, failed), which the app
//! gets as Android's own error. A ceremony Android withdraws (the app gave up, Android
//! stopped) is canceled in Windows Hello too. Nothing about a ceremony ends the window
//! manager.
//!
//! `webauthn.dll` is loaded when the first ceremony comes, not linked: a Windows without it
//! (older than Windows 10 version 1903) still runs the window manager, and its ceremonies
//! are unavailable with that reason.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::passkeys::{Ceremony, CeremonyResult, PasskeyEvent};
use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm passkeys";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it looks for finished ceremonies again.
const POLL: Duration = Duration::from_millis(100);

/// Performs one ceremony. The window manager's is Windows Hello ([`WindowsHello`]); a test's
/// answers as it is told.
pub trait Authenticator: Send + Sync {
    /// Makes or uses a passkey as `ceremony` says. `cancel` is withdrawn when Android no
    /// longer wants it; the ceremony then ends, canceled.
    fn perform(&self, ceremony: &Ceremony, cancel: &Cancel) -> CeremonyResult;
}

type Hook = Box<dyn Fn() + Send>;

/// Whether a ceremony is still wanted, and how to stop it while it runs.
#[derive(Default)]
pub struct Cancel {
    withdrawn: AtomicBool,
    hook: Mutex<Option<Hook>>,
}

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Android no longer wants the ceremony: stops it if it runs.
    pub fn withdraw(&self) {
        self.withdrawn.store(true, Ordering::SeqCst);
        if let Some(hook) = self.hook.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            hook();
        }
    }

    pub fn is_withdrawn(&self) -> bool {
        self.withdrawn.load(Ordering::SeqCst)
    }

    /// Sets what stops the ceremony while it runs. False when it was withdrawn already, so it
    /// is not started; a withdrawal from now on calls `hook`.
    pub fn while_running(&self, hook: Hook) -> bool {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
        !self.is_withdrawn()
    }

    /// The ceremony is over; nothing is left to stop.
    pub fn finished(&self) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Performs the passkey ceremonies of the user's Android until `stop`, reconnecting to the
/// service whenever the connection ends. What became of each is passed back through new
/// connections made with `connector`.
pub fn serve(
    mut connector: Box<dyn Connector>,
    authenticator: Arc<dyn Authenticator>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let (done_tx, done) = mpsc::channel();
    let mut running: HashMap<u64, Arc<Cancel>> = HashMap::new();
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let watch = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_passkeys());
        let mut watch = match watch {
            Ok(watch) => watch,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!("passkeys cannot reach the service: {error}"),
                    );
                    last_error = error;
                }
                report(&mut *connector, &done, &mut running, logger.as_deref());
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        while !stop.load(Ordering::Relaxed) {
            report(&mut *connector, &done, &mut running, logger.as_deref());
            match watch.next(POLL) {
                Ok(Some(PasskeyEvent::Ceremony { id, ceremony })) => {
                    let cancel = Arc::new(Cancel::new());
                    running.insert(id, Arc::clone(&cancel));
                    start_ceremony(
                        id,
                        *ceremony,
                        Arc::clone(&authenticator),
                        cancel,
                        done_tx.clone(),
                        logger.clone(),
                    );
                }
                Ok(Some(PasskeyEvent::Withdrawn { id })) => {
                    if let Some(cancel) = running.get(&id) {
                        cancel.withdraw();
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    note(
                        logger.as_deref(),
                        &format!("the passkeys' connection to the service ended: {error}"),
                    );
                    break;
                }
            }
        }
    }
    // The window manager is ending: what still runs is canceled in Windows Hello.
    for cancel in running.values() {
        cancel.withdraw();
    }
}

/// Passes what became of each finished ceremony to the service. A withdrawn one has nobody
/// waiting for it, and is not reported.
fn report(
    connector: &mut dyn Connector,
    done: &Receiver<(u64, CeremonyResult)>,
    running: &mut HashMap<u64, Arc<Cancel>>,
    logger: Option<&CompositorLogger>,
) {
    while let Ok((id, result)) = done.try_recv() {
        let withdrawn = running
            .remove(&id)
            .is_some_and(|cancel| cancel.is_withdrawn());
        if withdrawn {
            continue;
        }
        let sent = HostApiClient::connect(connector, BUILD, ANSWER)
            .and_then(|mut client| client.report_passkey_ceremony(id, &result));
        if let Err(error) = sent {
            note(
                logger,
                &format!("what became of passkey ceremony {id} did not reach Android: {error}"),
            );
        }
    }
}

/// What a ceremony is, for the log: never the user's name or any secret.
fn describe(ceremony: &Ceremony) -> String {
    let what = if ceremony.create {
        "making a passkey"
    } else {
        "signing in with a passkey"
    };
    let who = if ceremony.app_label.is_empty() {
        &ceremony.package
    } else {
        &ceremony.app_label
    };
    format!("{what} for {} ({who})", ceremony.rp_id)
}

/// Performs one ceremony on a thread of its own, and sends what became of it on `done`.
fn start_ceremony(
    id: u64,
    ceremony: Ceremony,
    authenticator: Arc<dyn Authenticator>,
    cancel: Arc<Cancel>,
    done: Sender<(u64, CeremonyResult)>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let failed = done.clone();
    let spawned = thread::Builder::new()
        .name("dsa-wm-passkey".into())
        .spawn(move || {
            let result = perform_one(&ceremony, &*authenticator, &cancel);
            match &result {
                CeremonyResult::Created { .. } | CeremonyResult::Asserted { .. } => {}
                CeremonyResult::Canceled(why)
                | CeremonyResult::Unavailable(why)
                | CeremonyResult::NotAllowed(why)
                | CeremonyResult::Failed(why) => note(
                    logger.as_deref(),
                    &format!("{}: {why}", describe(&ceremony)),
                ),
            }
            let _ = done.send((id, result));
        });
    if let Err(error) = spawned {
        let why = format!("the window manager could not start Windows Hello: {error}");
        let _ = failed.send((id, CeremonyResult::Failed(why)));
    }
}

/// Performs `ceremony`, unless it was withdrawn first; a panic in the authenticator's code is a
/// failure of this ceremony only.
pub fn perform_one(
    ceremony: &Ceremony,
    authenticator: &dyn Authenticator,
    cancel: &Cancel,
) -> CeremonyResult {
    if cancel.is_withdrawn() {
        return CeremonyResult::Canceled("Android withdrew the request".into());
    }
    let performed = catch_unwind(AssertUnwindSafe(|| authenticator.perform(ceremony, cancel)));
    cancel.finished();
    let result =
        performed.unwrap_or_else(|_| CeremonyResult::Failed("the passkey code failed".into()));
    if cancel.is_withdrawn() {
        return CeremonyResult::Canceled("Android withdrew the request".into());
    }
    result
}

// ---------------------------------------------------------------------------------------------
// What a ceremony asks of webauthn.dll, and what its answers mean. Pure, so they are tested
// without Windows Hello.

/// The oldest WebAuthn API version used: the first, Windows 10 version 1903's, which has every
/// call and structure a ceremony needs.
pub const MIN_API_VERSION: u32 = 1;

/// The `WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS` version WebAuthn API version `api`
/// takes, at most the newest these bindings describe.
pub fn make_credential_options_version(api: u32) -> u32 {
    match api {
        0..=2 => 3,
        3 => 4,
        4 | 5 => 5,
        6 => 6,
        _ => 7,
    }
}

/// The `WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS` version WebAuthn API version `api` takes,
/// at most the newest these bindings describe.
pub fn get_assertion_options_version(api: u32) -> u32 {
    match api {
        0..=2 => 4,
        3 => 5,
        4..=6 => 6,
        _ => 7,
    }
}

/// `WEBAUTHN_USER_VERIFICATION_REQUIREMENT_*` for WebAuthn's `userVerification`.
pub fn user_verification(requirement: &str) -> u32 {
    match requirement {
        "required" => 1,
        "preferred" => 2,
        "discouraged" => 3,
        _ => 0,
    }
}

/// `WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_*` for WebAuthn's `attestation`. An enterprise
/// attestation is asked as a direct one: Windows gives enterprise attestation only to
/// managed browsers.
pub fn attestation_conveyance(preference: &str) -> u32 {
    match preference {
        "indirect" => 2,
        "direct" | "enterprise" => 3,
        // "none", and nothing said, which WebAuthn reads as none.
        _ => 1,
    }
}

/// The algorithms a passkey may be made with: what the relying party listed, else ES256 and
/// RS256, which WebAuthn takes when nothing is listed.
pub fn algorithms(listed: &[i32]) -> Vec<i32> {
    if listed.is_empty() {
        vec![-7, -257]
    } else {
        listed.to_vec()
    }
}

/// WebAuthn's transport names for `WEBAUTHN_CTAP_TRANSPORT_*` flags.
pub fn transports(flags: u32) -> Vec<String> {
    let names = [
        (1, "usb"),
        (2, "nfc"),
        (4, "ble"),
        (16, "internal"),
        (32, "hybrid"),
    ];
    names
        .iter()
        .filter(|(flag, _)| flags & flag != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// Why `ceremony` cannot be performed by WebAuthn API version `api`, if it cannot.
pub fn unperformable(ceremony: &Ceremony, api: u32) -> Option<CeremonyResult> {
    if api < MIN_API_VERSION {
        return Some(CeremonyResult::Unavailable(format!(
            "this Windows has WebAuthn API version {api}, older than the {MIN_API_VERSION} \
             passkeys need; update Windows"
        )));
    }
    if ceremony.client_data_json.is_empty() {
        // Every version of Windows' WebAuthn API takes the client data itself and hashes it;
        // none takes only a hash.
        return Some(CeremonyResult::Unavailable(format!(
            "the browser gave only the hash of its client data, and Windows Hello (WebAuthn API \
             version {api}) takes the client data itself; use this computer's passkeys from a \
             browser on Windows"
        )));
    }
    if ceremony.rp_id.is_empty() {
        return Some(CeremonyResult::Failed(
            "the request names no relying party".into(),
        ));
    }
    if ceremony.create && ceremony.user_id.is_empty() {
        return Some(CeremonyResult::Failed(
            "the request names no user for the passkey".into(),
        ));
    }
    None
}

/// How a passkey is made, as `WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS` says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePlan {
    pub options_version: u32,
    pub timeout_ms: u32,
    pub require_resident_key: bool,
    pub prefer_resident_key: bool,
    pub user_verification: u32,
    pub attestation: u32,
    pub algorithms: Vec<i32>,
}

/// How `ceremony`, a creation, is asked of WebAuthn API version `api`.
pub fn create_plan(ceremony: &Ceremony, api: u32) -> CreatePlan {
    let options_version = make_credential_options_version(api);
    CreatePlan {
        options_version,
        timeout_ms: ceremony.wait().as_millis().min(u32::MAX as u128) as u32,
        require_resident_key: ceremony.resident_key == "required",
        // bPreferResidentKey came with version 4 of the options.
        prefer_resident_key: ceremony.resident_key == "preferred" && options_version >= 4,
        user_verification: user_verification(&ceremony.user_verification),
        attestation: attestation_conveyance(&ceremony.attestation),
        algorithms: algorithms(&ceremony.algorithms),
    }
}

/// How a passkey is used, as `WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS` says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetPlan {
    pub options_version: u32,
    pub timeout_ms: u32,
    pub user_verification: u32,
}

/// How `ceremony`, a sign-in, is asked of WebAuthn API version `api`.
pub fn get_plan(ceremony: &Ceremony, api: u32) -> GetPlan {
    GetPlan {
        options_version: get_assertion_options_version(api),
        timeout_ms: ceremony.wait().as_millis().min(u32::MAX as u128) as u32,
        user_verification: user_verification(&ceremony.user_verification),
    }
}

/// What a failed WebAuthn call's HRESULT means for the app, with `name`, the name WebAuthn
/// gives the error (it may be empty).
pub fn result_of_error(hr: i32, name: &str) -> CeremonyResult {
    const NTE_USER_CANCELLED: u32 = 0x8009_0036;
    const ERROR_CANCELLED: u32 = 0x8007_04C7;
    const NTE_EXISTS: u32 = 0x8009_000F;
    const NTE_NOT_FOUND: u32 = 0x8009_0011;
    const ERROR_TIMEOUT: u32 = 0x8007_05B4;
    const NTE_NOT_SUPPORTED: u32 = 0x8009_0029;
    const NTE_DEVICE_NOT_FOUND: u32 = 0x8009_0035;
    let code = format!("0x{:08X}", hr as u32);
    let named = if name.is_empty() {
        code
    } else {
        format!("{name}, {code}")
    };
    match hr as u32 {
        NTE_USER_CANCELLED | ERROR_CANCELLED => {
            CeremonyResult::Canceled(format!("Windows Hello was canceled ({named})"))
        }
        NTE_EXISTS => CeremonyResult::NotAllowed(format!(
            "this computer has a passkey the relying party asked not to make again ({named})"
        )),
        NTE_NOT_FOUND => CeremonyResult::NotAllowed(format!(
            "this computer has no passkey the relying party accepts ({named})"
        )),
        ERROR_TIMEOUT => {
            CeremonyResult::NotAllowed(format!("Windows Hello was not answered in time ({named})"))
        }
        NTE_NOT_SUPPORTED | NTE_DEVICE_NOT_FOUND => CeremonyResult::Unavailable(format!(
            "Windows Hello cannot do what the relying party asks on this computer ({named})"
        )),
        _ => CeremonyResult::Failed(format!("Windows Hello failed ({named})")),
    }
}

/// The reason given when Windows Hello is not set up.
pub const HELLO_NOT_SET_UP: &str = "Windows Hello is not set up on this computer: set up a PIN, \
     face or fingerprint in Settings, Accounts, Sign-in options";

#[cfg(windows)]
pub use self::hello::WindowsHello;

#[cfg(windows)]
mod hello {
    use std::sync::OnceLock;

    use dsa_host_core::passkeys::{Ceremony, CeremonyResult};
    use windows_sys::core::{BOOL, GUID, HRESULT, PCWSTR};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Networking::WindowsWebServices::{
        WEBAUTHN_ASSERTION, WEBAUTHN_AUTHENTICATOR_ATTACHMENT_PLATFORM,
        WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
        WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS, WEBAUTHN_CLIENT_DATA,
        WEBAUTHN_CLIENT_DATA_CURRENT_VERSION, WEBAUTHN_COSE_CREDENTIAL_PARAMETER,
        WEBAUTHN_COSE_CREDENTIAL_PARAMETERS, WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
        WEBAUTHN_CREDENTIAL, WEBAUTHN_CREDENTIALS, WEBAUTHN_CREDENTIAL_ATTESTATION,
        WEBAUTHN_CREDENTIAL_CURRENT_VERSION, WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
        WEBAUTHN_HASH_ALGORITHM_SHA_256, WEBAUTHN_RP_ENTITY_INFORMATION,
        WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION, WEBAUTHN_USER_ENTITY_INFORMATION,
        WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetForegroundWindow};

    use super::{
        create_plan, get_plan, result_of_error, transports, unperformable, Authenticator, Cancel,
        HELLO_NOT_SET_UP,
    };

    type MakeCredential = unsafe extern "system" fn(
        HWND,
        *const WEBAUTHN_RP_ENTITY_INFORMATION,
        *const WEBAUTHN_USER_ENTITY_INFORMATION,
        *const WEBAUTHN_COSE_CREDENTIAL_PARAMETERS,
        *const WEBAUTHN_CLIENT_DATA,
        *const WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
        *mut *mut WEBAUTHN_CREDENTIAL_ATTESTATION,
    ) -> HRESULT;
    type GetAssertion = unsafe extern "system" fn(
        HWND,
        PCWSTR,
        *const WEBAUTHN_CLIENT_DATA,
        *const WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
        *mut *mut WEBAUTHN_ASSERTION,
    ) -> HRESULT;
    type FreeAttestation = unsafe extern "system" fn(*const WEBAUTHN_CREDENTIAL_ATTESTATION);
    type FreeAssertion = unsafe extern "system" fn(*const WEBAUTHN_ASSERTION);
    type ApiVersion = unsafe extern "system" fn() -> u32;
    type CancellationId = unsafe extern "system" fn(*mut GUID) -> HRESULT;
    type CancelOperation = unsafe extern "system" fn(*const GUID) -> HRESULT;
    type ErrorName = unsafe extern "system" fn(HRESULT) -> PCWSTR;
    type PlatformAvailable = unsafe extern "system" fn(*mut BOOL) -> HRESULT;
    /// What GetProcAddress gives, before it is turned into the export's own type.
    type Farproc = unsafe extern "system" fn() -> isize;

    /// The calls of `webauthn.dll`, which stays loaded for the life of the process.
    struct WebAuthn {
        version: u32,
        make_credential: MakeCredential,
        get_assertion: GetAssertion,
        free_attestation: FreeAttestation,
        free_assertion: FreeAssertion,
        cancellation_id: Option<CancellationId>,
        cancel: Option<CancelOperation>,
        error_name: Option<ErrorName>,
        platform_available: Option<PlatformAvailable>,
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn load() -> Result<WebAuthn, String> {
        let name = wide("webauthn.dll");
        // SAFETY: a NUL-terminated name, looked up in System32 only.
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(format!(
                "this Windows has no WebAuthn API (webauthn.dll: {}); passkeys need Windows 10 \
                 version 1903 or later",
                std::io::Error::last_os_error()
            ));
        }
        let find = |symbol: &[u8]| {
            // SAFETY: a loaded module and a NUL-terminated symbol name.
            unsafe { GetProcAddress(module, symbol.as_ptr()) }
        };
        let required = |symbol: &[u8]| {
            find(symbol).ok_or_else(|| {
                format!(
                    "this Windows' webauthn.dll has no {}; update Windows",
                    String::from_utf8_lossy(&symbol[..symbol.len() - 1])
                )
            })
        };
        // SAFETY: each pointer is the export of that name, whose signature is the one
        // webauthn.h declares and the type it is turned into.
        unsafe {
            let version = std::mem::transmute::<Farproc, ApiVersion>(required(
                b"WebAuthNGetApiVersionNumber\0",
            )?);
            Ok(WebAuthn {
                version: version(),
                make_credential: std::mem::transmute::<Farproc, MakeCredential>(required(
                    b"WebAuthNAuthenticatorMakeCredential\0",
                )?),
                get_assertion: std::mem::transmute::<Farproc, GetAssertion>(required(
                    b"WebAuthNAuthenticatorGetAssertion\0",
                )?),
                free_attestation: std::mem::transmute::<Farproc, FreeAttestation>(required(
                    b"WebAuthNFreeCredentialAttestation\0",
                )?),
                free_assertion: std::mem::transmute::<Farproc, FreeAssertion>(required(
                    b"WebAuthNFreeAssertion\0",
                )?),
                cancellation_id: find(b"WebAuthNGetCancellationId\0")
                    .map(|f| std::mem::transmute::<Farproc, CancellationId>(f)),
                cancel: find(b"WebAuthNCancelCurrentOperation\0")
                    .map(|f| std::mem::transmute::<Farproc, CancelOperation>(f)),
                error_name: find(b"WebAuthNGetErrorName\0")
                    .map(|f| std::mem::transmute::<Farproc, ErrorName>(f)),
                platform_available: find(
                    b"WebAuthNIsUserVerifyingPlatformAuthenticatorAvailable\0",
                )
                .map(|f| std::mem::transmute::<Farproc, PlatformAvailable>(f)),
            })
        }
    }

    fn api() -> &'static Result<WebAuthn, String> {
        static API: OnceLock<Result<WebAuthn, String>> = OnceLock::new();
        API.get_or_init(load)
    }

    /// Reads a NUL-terminated string WebAuthn owns.
    fn text(pointer: PCWSTR) -> String {
        if pointer.is_null() {
            return String::new();
        }
        // SAFETY: a NUL-terminated string that lives as long as the module.
        unsafe {
            let mut len = 0;
            while *pointer.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(pointer, len))
        }
    }

    /// The bytes at `pointer`, `len` long, copied.
    fn bytes(pointer: *const u8, len: u32) -> Vec<u8> {
        if pointer.is_null() || len == 0 {
            return Vec::new();
        }
        // SAFETY: WebAuthn's result holds `len` bytes at `pointer` until it is freed.
        unsafe { std::slice::from_raw_parts(pointer, len as usize) }.to_vec()
    }

    /// The window Windows Hello comes up over: the one the person is looking at (the app's
    /// own window, which asked), so its prompt is in front of them.
    fn parent() -> HWND {
        // SAFETY: plain queries of the desktop's state.
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() {
                GetDesktopWindow()
            } else {
                window
            }
        }
    }

    /// Credential descriptors over `ids`, which must outlive them.
    fn credentials(ids: &mut [Vec<u8>]) -> Vec<WEBAUTHN_CREDENTIAL> {
        ids.iter_mut()
            .map(|id| WEBAUTHN_CREDENTIAL {
                dwVersion: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
                cbId: id.len() as u32,
                pbId: id.as_mut_ptr(),
                pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
            })
            .collect()
    }

    fn credential_list(list: &mut [WEBAUTHN_CREDENTIAL]) -> WEBAUTHN_CREDENTIALS {
        WEBAUTHN_CREDENTIALS {
            cCredentials: list.len() as u32,
            pCredentials: if list.is_empty() {
                std::ptr::null_mut()
            } else {
                list.as_mut_ptr()
            },
        }
    }

    /// Windows Hello, through `webauthn.dll`.
    #[derive(Default)]
    pub struct WindowsHello;

    impl WindowsHello {
        fn error(api: &WebAuthn, hr: HRESULT) -> CeremonyResult {
            // SAFETY: an export that names an HRESULT, with a string the module owns.
            let name = api
                .error_name
                .map(|f| text(unsafe { f(hr) }))
                .unwrap_or_default();
            result_of_error(hr, &name)
        }

        /// A cancellation ID for the next call, set up so that a withdrawal cancels it. `None`
        /// when this Windows cannot cancel; `Err` when the ceremony was withdrawn already.
        fn cancellation(api: &WebAuthn, cancel: &Cancel) -> Result<Option<GUID>, ()> {
            let (Some(get_id), Some(cancel_call)) = (api.cancellation_id, api.cancel) else {
                return if cancel.while_running(Box::new(|| {})) {
                    Ok(None)
                } else {
                    Err(())
                };
            };
            let mut id = GUID::from_u128(0);
            // SAFETY: an out GUID.
            if unsafe { get_id(&mut id) } < 0 {
                return if cancel.while_running(Box::new(|| {})) {
                    Ok(None)
                } else {
                    Err(())
                };
            }
            let copy = id;
            let hook = Box::new(move || {
                // SAFETY: the ID of the operation to cancel; canceling one that ended does
                // nothing.
                unsafe { cancel_call(&copy) };
            });
            if cancel.while_running(hook) {
                Ok(Some(id))
            } else {
                Err(())
            }
        }

        fn make(api: &WebAuthn, ceremony: &Ceremony, cancel: &Cancel) -> CeremonyResult {
            let plan = create_plan(ceremony, api.version);
            let rp_id = wide(&ceremony.rp_id);
            let rp_name = wide(if ceremony.rp_name.is_empty() {
                &ceremony.rp_id
            } else {
                &ceremony.rp_name
            });
            let rp = WEBAUTHN_RP_ENTITY_INFORMATION {
                dwVersion: WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION,
                pwszId: rp_id.as_ptr(),
                pwszName: rp_name.as_ptr(),
                pwszIcon: std::ptr::null(),
            };
            let mut user_id = ceremony.user_id.clone();
            let user_name = wide(&ceremony.user_name);
            let display_name = wide(if ceremony.user_display_name.is_empty() {
                &ceremony.user_name
            } else {
                &ceremony.user_display_name
            });
            let user = WEBAUTHN_USER_ENTITY_INFORMATION {
                dwVersion: WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
                cbId: user_id.len() as u32,
                pbId: user_id.as_mut_ptr(),
                pwszName: user_name.as_ptr(),
                pwszIcon: std::ptr::null(),
                pwszDisplayName: display_name.as_ptr(),
            };
            let mut parameters: Vec<WEBAUTHN_COSE_CREDENTIAL_PARAMETER> = plan
                .algorithms
                .iter()
                .map(|&algorithm| WEBAUTHN_COSE_CREDENTIAL_PARAMETER {
                    dwVersion: WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
                    pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
                    lAlg: algorithm,
                })
                .collect();
            let cose = WEBAUTHN_COSE_CREDENTIAL_PARAMETERS {
                cCredentialParameters: parameters.len() as u32,
                pCredentialParameters: parameters.as_mut_ptr(),
            };
            let mut client_json = ceremony.client_data_json.clone();
            let client = WEBAUTHN_CLIENT_DATA {
                dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
                cbClientDataJSON: client_json.len() as u32,
                pbClientDataJSON: client_json.as_mut_ptr(),
                pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
            };
            let mut excluded = ceremony.credential_ids.clone();
            let mut excluded_list = credentials(&mut excluded);
            let Ok(mut cancellation) = Self::cancellation(api, cancel) else {
                return CeremonyResult::Canceled("Android withdrew the request".into());
            };
            let mut options = WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS {
                dwVersion: plan.options_version,
                dwTimeoutMilliseconds: plan.timeout_ms,
                CredentialList: credential_list(&mut excluded_list),
                dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_PLATFORM,
                bRequireResidentKey: plan.require_resident_key.into(),
                dwUserVerificationRequirement: plan.user_verification,
                dwAttestationConveyancePreference: plan.attestation,
                ..Default::default()
            };
            if let Some(id) = cancellation.as_mut() {
                options.pCancellationId = id as *mut GUID;
            }
            if plan.prefer_resident_key {
                options.bPreferResidentKey = 1;
            }
            let mut attestation: *mut WEBAUTHN_CREDENTIAL_ATTESTATION = std::ptr::null_mut();
            // SAFETY: every structure and buffer it points to lives until the call returns;
            // the out pointer receives WebAuthn's result, freed below.
            let hr = unsafe {
                (api.make_credential)(
                    parent(),
                    &rp,
                    &user,
                    &cose,
                    &client,
                    &options,
                    &mut attestation,
                )
            };
            if hr < 0 || attestation.is_null() {
                return Self::error(api, hr);
            }
            // SAFETY: WebAuthn's result, read before it is freed.
            let result = unsafe {
                let made = &*attestation;
                let credential_id = bytes(made.pbCredentialId, made.cbCredentialId);
                let attestation_object = bytes(made.pbAttestationObject, made.cbAttestationObject);
                // dwUsedTransport came with version 3 of the result.
                let used = if made.dwVersion >= 3 {
                    transports(made.dwUsedTransport)
                } else {
                    vec!["internal".to_string()]
                };
                (credential_id, attestation_object, used)
            };
            // SAFETY: WebAuthn's result, freed once.
            unsafe { (api.free_attestation)(attestation) };
            let (credential_id, attestation_object, transports) = result;
            if credential_id.is_empty() || attestation_object.is_empty() {
                return CeremonyResult::Failed(
                    "Windows Hello made a passkey but returned no credential".into(),
                );
            }
            CeremonyResult::Created {
                credential_id,
                attestation_object,
                transports,
            }
        }

        fn get(api: &WebAuthn, ceremony: &Ceremony, cancel: &Cancel) -> CeremonyResult {
            let plan = get_plan(ceremony, api.version);
            let rp_id = wide(&ceremony.rp_id);
            let mut client_json = ceremony.client_data_json.clone();
            let client = WEBAUTHN_CLIENT_DATA {
                dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
                cbClientDataJSON: client_json.len() as u32,
                pbClientDataJSON: client_json.as_mut_ptr(),
                pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
            };
            let mut allowed = ceremony.credential_ids.clone();
            let mut allowed_list = credentials(&mut allowed);
            let Ok(mut cancellation) = Self::cancellation(api, cancel) else {
                return CeremonyResult::Canceled("Android withdrew the request".into());
            };
            let mut options = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS {
                dwVersion: plan.options_version,
                dwTimeoutMilliseconds: plan.timeout_ms,
                CredentialList: credential_list(&mut allowed_list),
                dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_PLATFORM,
                dwUserVerificationRequirement: plan.user_verification,
                ..Default::default()
            };
            if let Some(id) = cancellation.as_mut() {
                options.pCancellationId = id as *mut GUID;
            }
            let mut assertion: *mut WEBAUTHN_ASSERTION = std::ptr::null_mut();
            // SAFETY: every structure and buffer it points to lives until the call returns;
            // the out pointer receives WebAuthn's result, freed below.
            let hr = unsafe {
                (api.get_assertion)(parent(), rp_id.as_ptr(), &client, &options, &mut assertion)
            };
            if hr < 0 || assertion.is_null() {
                return Self::error(api, hr);
            }
            // SAFETY: WebAuthn's result, read before it is freed.
            let result = unsafe {
                let signed = &*assertion;
                CeremonyResult::Asserted {
                    credential_id: bytes(signed.Credential.pbId, signed.Credential.cbId),
                    authenticator_data: bytes(
                        signed.pbAuthenticatorData,
                        signed.cbAuthenticatorData,
                    ),
                    signature: bytes(signed.pbSignature, signed.cbSignature),
                    user_handle: bytes(signed.pbUserId, signed.cbUserId),
                }
            };
            // SAFETY: WebAuthn's result, freed once.
            unsafe { (api.free_assertion)(assertion) };
            result
        }
    }

    impl Authenticator for WindowsHello {
        fn perform(&self, ceremony: &Ceremony, cancel: &Cancel) -> CeremonyResult {
            let api = match api() {
                Ok(api) => api,
                Err(why) => return CeremonyResult::Unavailable(why.clone()),
            };
            if let Some(refused) = unperformable(ceremony, api.version) {
                return refused;
            }
            if let Some(available) = api.platform_available {
                let mut yes: BOOL = 0;
                // SAFETY: an out BOOL.
                let hr = unsafe { available(&mut yes) };
                if hr >= 0 && yes == 0 {
                    return CeremonyResult::Unavailable(HELLO_NOT_SET_UP.into());
                }
            }
            if ceremony.create {
                Self::make(api, ceremony, cancel)
            } else {
                Self::get(api, ceremony, cancel)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ceremony(create: bool) -> Ceremony {
        Ceremony {
            create,
            rp_id: "bank.example".into(),
            rp_name: "Bank".into(),
            user_id: vec![1, 2, 3],
            user_name: "alice".into(),
            user_display_name: "Alice".into(),
            client_data_json: br#"{"type":"webauthn.create"}"#.to_vec(),
            client_data_hash: vec![0; 32],
            resident_key: "required".into(),
            user_verification: "required".into(),
            timeout_ms: 60_000,
            package: "com.bank.app".into(),
            ..Ceremony::default()
        }
    }

    #[test]
    fn fr36_windows_hello_errors_become_the_apps_errors() {
        assert!(matches!(
            result_of_error(0x8009_0036_u32 as i32, "NotAllowedError"),
            CeremonyResult::Canceled(_)
        ));
        assert!(matches!(
            result_of_error(0x8007_04C7_u32 as i32, ""),
            CeremonyResult::Canceled(_)
        ));
        assert!(matches!(
            result_of_error(0x8009_000F_u32 as i32, ""),
            CeremonyResult::NotAllowed(_)
        ));
        assert!(matches!(
            result_of_error(0x8009_0011_u32 as i32, ""),
            CeremonyResult::NotAllowed(_)
        ));
        assert!(matches!(
            result_of_error(0x8009_0029_u32 as i32, ""),
            CeremonyResult::Unavailable(_)
        ));
        let CeremonyResult::Failed(why) = result_of_error(0x8007_0057_u32 as i32, "InvalidArg")
        else {
            panic!("an unknown error is a failure");
        };
        assert!(
            why.contains("InvalidArg") && why.contains("0x80070057"),
            "{why}"
        );
    }

    #[test]
    fn fr36_the_options_version_follows_the_installed_webauthn() {
        assert_eq!(make_credential_options_version(1), 3);
        assert_eq!(make_credential_options_version(3), 4);
        assert_eq!(make_credential_options_version(4), 5);
        assert_eq!(make_credential_options_version(6), 6);
        assert_eq!(make_credential_options_version(9), 7);
        assert_eq!(get_assertion_options_version(1), 4);
        assert_eq!(get_assertion_options_version(3), 5);
        assert_eq!(get_assertion_options_version(5), 6);
        assert_eq!(get_assertion_options_version(9), 7);
    }

    #[test]
    fn fr36_a_creation_is_asked_as_the_relying_party_asked() {
        let plan = create_plan(&ceremony(true), 4);
        assert_eq!(plan.options_version, 5);
        assert_eq!(plan.timeout_ms, 60_000);
        assert!(plan.require_resident_key);
        assert!(!plan.prefer_resident_key);
        assert_eq!(plan.user_verification, 1);
        assert_eq!(plan.attestation, 1, "no attestation asked is none");
        assert_eq!(plan.algorithms, vec![-7, -257]);

        let mut preferred = ceremony(true);
        preferred.resident_key = "preferred".into();
        preferred.attestation = "direct".into();
        preferred.algorithms = vec![-8, -7];
        preferred.timeout_ms = 0;
        let plan = create_plan(&preferred, 3);
        assert!(plan.prefer_resident_key && !plan.require_resident_key);
        assert_eq!(plan.attestation, 3);
        assert_eq!(plan.algorithms, vec![-8, -7]);
        assert_eq!(plan.timeout_ms, 300_000, "no timeout is the default wait");
        assert!(
            !create_plan(&preferred, 1).prefer_resident_key,
            "too old to say so"
        );

        let plan = get_plan(&ceremony(false), 7);
        assert_eq!(plan.options_version, 7);
        assert_eq!(plan.user_verification, 1);
    }

    #[test]
    fn fr36_only_a_hash_of_the_client_data_is_unavailable_with_the_reason() {
        let mut hashed = ceremony(false);
        hashed.client_data_json.clear();
        let Some(CeremonyResult::Unavailable(why)) = unperformable(&hashed, 7) else {
            panic!("a hash alone cannot go to Windows Hello");
        };
        assert!(why.contains("hash"), "{why}");
        assert!(matches!(
            unperformable(&ceremony(true), 0),
            Some(CeremonyResult::Unavailable(_))
        ));
        assert_eq!(unperformable(&ceremony(true), 1), None);
        let mut nobody = ceremony(true);
        nobody.user_id.clear();
        assert!(matches!(
            unperformable(&nobody, 7),
            Some(CeremonyResult::Failed(_))
        ));
    }

    #[test]
    fn fr36_transports_are_named_as_webauthn_names_them() {
        assert_eq!(transports(16), vec!["internal"]);
        assert_eq!(transports(32 | 1), vec!["usb", "hybrid"]);
        assert!(transports(0).is_empty());
    }

    /// An authenticator that waits until it is canceled, or answers at once.
    struct Waiting;

    impl Authenticator for Waiting {
        fn perform(&self, _: &Ceremony, cancel: &Cancel) -> CeremonyResult {
            let (tx, rx) = mpsc::channel::<()>();
            let tx = Mutex::new(tx);
            if !cancel.while_running(Box::new(move || {
                let _ = tx.lock().unwrap().send(());
            })) {
                return CeremonyResult::Canceled("before".into());
            }
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(()) => CeremonyResult::Canceled("canceled".into()),
                Err(_) => CeremonyResult::Failed("never canceled".into()),
            }
        }
    }

    struct Panicking;

    impl Authenticator for Panicking {
        fn perform(&self, _: &Ceremony, _: &Cancel) -> CeremonyResult {
            panic!("a bug in the authenticator");
        }
    }

    #[test]
    fn fr36_a_withdrawn_ceremony_is_canceled_and_a_panic_fails_only_its_ceremony() {
        let cancel = Arc::new(Cancel::new());
        let running = {
            let cancel = Arc::clone(&cancel);
            thread::spawn(move || perform_one(&ceremony(false), &Waiting, &cancel))
        };
        thread::sleep(Duration::from_millis(50));
        cancel.withdraw();
        assert!(matches!(
            running.join().unwrap(),
            CeremonyResult::Canceled(_)
        ));

        let early = Cancel::new();
        early.withdraw();
        assert!(matches!(
            perform_one(&ceremony(false), &Waiting, &early),
            CeremonyResult::Canceled(_)
        ));

        assert!(matches!(
            perform_one(&ceremony(true), &Panicking, &Cancel::new()),
            CeremonyResult::Failed(_)
        ));
    }
}

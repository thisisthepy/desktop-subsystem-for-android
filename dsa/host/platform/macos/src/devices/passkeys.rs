//! Passkey ceremonies with the Mac's passkeys, through AuthenticationServices.
//!
//! A ceremony is performed as a browser performs one: an
//! `ASAuthorizationPlatformPublicKeyCredentialProvider` for the relying party, a registration
//! or assertion request made from `ASPublicKeyCredentialClientData` (the challenge and the
//! origin, from which macOS writes the client data it signs), and an
//! `ASAuthorizationController` that shows the Mac's passkey prompt over a small window of the
//! device lender's. That needs macOS 14 or later, and the program must be signed with the
//! `com.apple.developer.web-browser.public-key-credential` entitlement, which Apple grants on
//! request and only with a provisioning profile; without it the ceremony is unavailable,
//! with that reason, before any prompt.
//!
//! The controller and its callbacks live on the main thread, whose run loop AppKit runs. The
//! follower's thread waits for the outcome, cancels the prompt when Android withdraws the
//! request or its time runs out, and lets go of the controller and the window afterwards.
//! Which ceremonies the Mac can perform, and what its outcome means, are the plain rules of
//! [`super::passkey_ceremonies`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dsa_host_core::passkeys::{Ceremony, CeremonyResult};
use objc2::msg_send;
use objc2::rc::{autoreleasepool, Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, ClassBuilder, Sel};
use objc2::sel;

use super::passkey_ceremonies::{mac_client_data, result_of, MacClientData, MacOutcome};
use crate::objc::{
    bytes_of, class, ns_data, ns_string, on_main, string_of, CGPoint, CGRect, CGSize,
};

#[link(name = "AuthenticationServices", kind = "framework")]
extern "C" {}

/// The entitlement a program needs to perform a ceremony for another app or a web page.
pub const BROWSER_ENTITLEMENT: &str = "com.apple.developer.web-browser.public-key-credential";

/// `ASPublicKeyCredentialClientDataCrossOriginValueSameOriginWithAncestors`: the client data
/// says `"crossOrigin":false`.
const SAME_ORIGIN_WITH_ANCESTORS: isize = 2;

/// `NSWindowStyleMaskTitled | NSWindowStyleMaskClosable`, `NSBackingStoreBuffered`.
const STYLE: usize = 1 | 2;
const BACKING_BUFFERED: usize = 2;

/// How long a canceled prompt may take to say it ended.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// How often the waiting thread looks whether the ceremony was withdrawn.
const LOOK: Duration = Duration::from_millis(200);

/// A ceremony under way: its controller and window (owned, as raw pointers, so the list can
/// be shared), and where its outcome goes.
struct Active {
    id: u64,
    controller: usize,
    window: usize,
    outcome: Option<Sender<MacOutcome>>,
}

static ACTIVE: Mutex<Vec<Active>> = Mutex::new(Vec::new());

fn active() -> std::sync::MutexGuard<'static, Vec<Active>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sends `outcome` for the ceremony of `controller`, once.
fn deliver(controller: *mut AnyObject, outcome: MacOutcome) {
    let sender = active()
        .iter_mut()
        .find(|a| a.controller == controller as usize)
        .and_then(|a| a.outcome.take());
    if let Some(sender) = sender {
        let _ = sender.send(outcome);
    }
}

unsafe extern "C-unwind" fn did_complete(
    _this: &AnyObject,
    _: Sel,
    controller: *mut AnyObject,
    authorization: *mut AnyObject,
) {
    // The closure only reads the authorization the system handed this callback; a panic
    // while reading it leaves nothing half changed, so asserting unwind safety is sound.
    let read = std::panic::AssertUnwindSafe(|| unsafe { outcome_of(authorization) });
    let outcome = std::panic::catch_unwind(read).unwrap_or_else(|_| MacOutcome::Error {
        domain: String::new(),
        code: -1,
        message: "the passkey's result could not be read".into(),
    });
    deliver(controller, outcome);
}

unsafe extern "C-unwind" fn did_fail(
    _this: &AnyObject,
    _: Sel,
    controller: *mut AnyObject,
    error: *mut AnyObject,
) {
    let outcome = if error.is_null() {
        MacOutcome::Error {
            domain: String::new(),
            code: -1,
            message: "the passkey prompt ended without a result".into(),
        }
    } else {
        let domain: *mut AnyObject = msg_send![error, domain];
        let code: isize = msg_send![error, code];
        let message: *mut AnyObject = msg_send![error, localizedDescription];
        MacOutcome::Error {
            domain: string_of(domain),
            code: code as i64,
            message: string_of(message),
        }
    };
    deliver(controller, outcome);
}

unsafe extern "C-unwind" fn anchor(
    _this: &AnyObject,
    _: Sel,
    controller: *mut AnyObject,
) -> *mut AnyObject {
    active()
        .iter()
        .find(|a| a.controller == controller as usize)
        .map_or(std::ptr::null_mut(), |a| a.window as *mut AnyObject)
}

/// What `authorization` (an `ASAuthorization`) holds: a passkey made or used.
unsafe fn outcome_of(authorization: *mut AnyObject) -> MacOutcome {
    let credential: *mut AnyObject = if authorization.is_null() {
        std::ptr::null_mut()
    } else {
        msg_send![authorization, credential]
    };
    let is = |name: &std::ffi::CStr| -> bool {
        match class(name) {
            Ok(kind) if !credential.is_null() => msg_send![credential, isKindOfClass: kind],
            _ => false,
        }
    };
    let data = |object: *mut AnyObject| bytes_of(object);
    if is(c"ASAuthorizationPlatformPublicKeyCredentialRegistration") {
        MacOutcome::Registered {
            credential_id: data(msg_send![credential, credentialID]),
            attestation_object: data(msg_send![credential, rawAttestationObject]),
            client_data_json: data(msg_send![credential, rawClientDataJSON]),
        }
    } else if is(c"ASAuthorizationPlatformPublicKeyCredentialAssertion") {
        MacOutcome::Asserted {
            credential_id: data(msg_send![credential, credentialID]),
            authenticator_data: data(msg_send![credential, rawAuthenticatorData]),
            signature: data(msg_send![credential, signature]),
            user_handle: data(msg_send![credential, userID]),
            client_data_json: data(msg_send![credential, rawClientDataJSON]),
        }
    } else {
        MacOutcome::Error {
            domain: String::new(),
            code: -1,
            message: "macOS gave a credential that is not a passkey".into(),
        }
    }
}

/// The class of the controller's delegate and presentation context provider.
fn delegate_class() -> Result<&'static AnyClass, String> {
    static CLASS: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let mut b = ClassBuilder::new(c"DSAPasskeyDelegate", class(c"NSObject")?)
                .ok_or("the passkey delegate class is already defined")?;
            for protocol in [
                c"ASAuthorizationControllerDelegate",
                c"ASAuthorizationControllerPresentationContextProviding",
            ] {
                if let Some(protocol) = AnyProtocol::get(protocol) {
                    b.add_protocol(protocol);
                }
            }
            b.add_method(
                sel!(authorizationController:didCompleteWithAuthorization:),
                did_complete as unsafe extern "C-unwind" fn(_, _, _, _),
            );
            b.add_method(
                sel!(authorizationController:didCompleteWithError:),
                did_fail as unsafe extern "C-unwind" fn(_, _, _, _),
            );
            b.add_method(
                sel!(presentationAnchorForAuthorizationController:),
                anchor as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            Ok(b.register())
        })
        .clone()
}

/// The one delegate of every ceremony. A controller holds its delegate weakly, so it is kept
/// for the life of the program.
fn delegate() -> Result<*mut AnyObject, String> {
    static DELEGATE: OnceLock<usize> = OnceLock::new();
    let class = delegate_class()?;
    let delegate = *DELEGATE.get_or_init(|| unsafe {
        let made: Option<Retained<AnyObject>> = msg_send![class, new];
        made.map(|made| Retained::into_raw(made) as usize)
            .unwrap_or(0)
    });
    if delegate == 0 {
        return Err("the passkey delegate could not be made".into());
    }
    Ok(delegate as *mut AnyObject)
}

/// Whether `object` answers `selector`.
unsafe fn responds(object: &AnyObject, selector: Sel) -> bool {
    msg_send![object, respondsToSelector: selector]
}

/// An `NSArray` of `objects`.
unsafe fn ns_array(objects: &[Retained<AnyObject>]) -> Result<Retained<AnyObject>, String> {
    let pointers: Vec<*const AnyObject> = objects.iter().map(Retained::as_ptr).collect();
    let array_class = class(c"NSArray")?;
    let array: Option<Retained<AnyObject>> = msg_send![
        array_class,
        arrayWithObjects: pointers.as_ptr(),
        count: pointers.len()
    ];
    array.ok_or_else(|| "a list could not be made".to_string())
}

/// The credential descriptors of `ids`.
unsafe fn descriptors(ids: &[Vec<u8>]) -> Result<Retained<AnyObject>, String> {
    let descriptor_class = class(c"ASAuthorizationPlatformPublicKeyCredentialDescriptor")?;
    let mut list = Vec::new();
    for id in ids {
        let made: Allocated<AnyObject> = msg_send![descriptor_class, alloc];
        let made: Option<Retained<AnyObject>> =
            msg_send![made, initWithCredentialID: &*ns_data(id)?];
        list.push(made.ok_or("a passkey's descriptor could not be made")?);
    }
    ns_array(&list)
}

/// The small window the prompt is shown over, in front.
unsafe fn anchor_window(ceremony: &Ceremony) -> Result<Retained<AnyObject>, String> {
    let window_class = class(c"NSWindow")?;
    let rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 480.0,
            height: 120.0,
        },
    };
    let allocated: Allocated<AnyObject> = msg_send![window_class, alloc];
    let window: Option<Retained<AnyObject>> = msg_send![
        allocated,
        initWithContentRect: rect,
        styleMask: STYLE,
        backing: BACKING_BUFFERED,
        defer: false
    ];
    let window = window.ok_or("AppKit could not make a window for the passkey prompt")?;
    let _: () = msg_send![&*window, setReleasedWhenClosed: false];
    let app = if ceremony.app_label.is_empty() {
        ceremony.package.as_str()
    } else {
        ceremony.app_label.as_str()
    };
    let title = format!("{app}: {}", ceremony.rp_id);
    let _: () = msg_send![&*window, setTitle: &*ns_string(&title)];
    let _: () = msg_send![&*window, center];
    let app_class = class(c"NSApplication")?;
    let shared: *mut AnyObject = msg_send![app_class, sharedApplication];
    if !shared.is_null() {
        let _: () = msg_send![shared, activateIgnoringOtherApps: true];
    }
    let _: () = msg_send![&*window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
    Ok(window)
}

/// The request for `ceremony`, made from `client`.
unsafe fn request(
    ceremony: &Ceremony,
    client: &MacClientData,
) -> Result<Retained<AnyObject>, CeremonyResult> {
    let unavailable = |why: String| CeremonyResult::Unavailable(why);
    let provider_class = class(c"ASAuthorizationPlatformPublicKeyCredentialProvider")
        .map_err(|_| unavailable("this Mac has no passkeys (macOS 12 or later has)".into()))?;
    let client_class = class(c"ASPublicKeyCredentialClientData").map_err(|_| {
        unavailable("using the Mac's passkeys for Android apps needs macOS 14 or later".into())
    })?;
    let failed = |why: &str| CeremonyResult::Failed(why.to_string());

    let provider: Allocated<AnyObject> = msg_send![provider_class, alloc];
    let provider: Option<Retained<AnyObject>> = msg_send![
        provider,
        initWithRelyingPartyIdentifier: &*ns_string(&ceremony.rp_id)
    ];
    let provider = provider.ok_or_else(|| failed("the passkey provider could not be made"))?;

    let challenge = ns_data(&client.challenge).map_err(|e| failed(&e))?;
    let data: Allocated<AnyObject> = msg_send![client_class, alloc];
    let data: Option<Retained<AnyObject>> = msg_send![
        data,
        initWithChallenge: &*challenge,
        origin: &*ns_string(&client.origin)
    ];
    let data = data.ok_or_else(|| failed("the client data could not be made"))?;
    if responds(&data, sel!(setCrossOrigin:)) {
        let _: () = msg_send![&*data, setCrossOrigin: SAME_ORIGIN_WITH_ANCESTORS];
    }

    let request: Option<Retained<AnyObject>> = if ceremony.create {
        if !responds(
            &provider,
            sel!(createCredentialRegistrationRequestWithClientData:name:userID:),
        ) {
            return Err(unavailable(
                "using the Mac's passkeys for Android apps needs macOS 14 or later".into(),
            ));
        }
        let user_id = ns_data(&ceremony.user_id).map_err(|e| failed(&e))?;
        msg_send![
            &*provider,
            createCredentialRegistrationRequestWithClientData: &*data,
            name: &*ns_string(&ceremony.user_name),
            userID: &*user_id
        ]
    } else {
        if !responds(
            &provider,
            sel!(createCredentialAssertionRequestWithClientData:),
        ) {
            return Err(unavailable(
                "using the Mac's passkeys for Android apps needs macOS 14 or later".into(),
            ));
        }
        msg_send![&*provider, createCredentialAssertionRequestWithClientData: &*data]
    };
    let request = request.ok_or_else(|| failed("the passkey request could not be made"))?;

    if !ceremony.user_verification.is_empty() {
        let _: () = msg_send![
            &*request,
            setUserVerificationPreference: &*ns_string(&ceremony.user_verification)
        ];
    }
    if ceremony.create {
        if !ceremony.attestation.is_empty() {
            let _: () = msg_send![
                &*request,
                setAttestationPreference: &*ns_string(&ceremony.attestation)
            ];
        }
        if !ceremony.credential_ids.is_empty() && responds(&request, sel!(setExcludedCredentials:))
        {
            let excluded = descriptors(&ceremony.credential_ids).map_err(|e| failed(&e))?;
            let _: () = msg_send![&*request, setExcludedCredentials: &*excluded];
        }
    } else if !ceremony.credential_ids.is_empty() {
        let allowed = descriptors(&ceremony.credential_ids).map_err(|e| failed(&e))?;
        let _: () = msg_send![&*request, setAllowedCredentials: &*allowed];
    }
    Ok(request)
}

/// Starts the ceremony `id` on the main thread; its outcome goes to `outcome`.
unsafe fn start_on_main(
    id: u64,
    ceremony: &Ceremony,
    client: &MacClientData,
    outcome: Sender<MacOutcome>,
) -> Result<(), CeremonyResult> {
    let failed = |why: String| CeremonyResult::Failed(why);
    let request = request(ceremony, client)?;
    let delegate = delegate().map_err(failed)?;
    let controller_class = class(c"ASAuthorizationController").map_err(failed)?;
    let requests = ns_array(&[request]).map_err(failed)?;
    let controller: Allocated<AnyObject> = msg_send![controller_class, alloc];
    let controller: Option<Retained<AnyObject>> =
        msg_send![controller, initWithAuthorizationRequests: &*requests];
    let controller =
        controller.ok_or_else(|| failed("the passkey prompt could not be made".into()))?;
    let window = anchor_window(ceremony).map_err(failed)?;
    let _: () = msg_send![&*controller, setDelegate: delegate];
    let _: () = msg_send![&*controller, setPresentationContextProvider: delegate];
    let controller = Retained::into_raw(controller);
    active().push(Active {
        id,
        controller: controller as usize,
        window: Retained::into_raw(window) as usize,
        outcome: Some(outcome),
    });
    let _: () = msg_send![controller, performRequests];
    Ok(())
}

/// Cancels the prompt of ceremony `id`, if it is still shown.
fn cancel(id: u64) {
    on_main(move || {
        let controller = active()
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.controller as *mut AnyObject);
        if let Some(controller) = controller {
            // SAFETY: a controller this code owns, on the main thread.
            unsafe {
                if responds(&*controller, sel!(cancel)) {
                    let _: () = msg_send![controller, cancel];
                }
            }
        }
    });
}

/// Lets go of ceremony `id`'s controller and window.
fn finish(id: u64) {
    on_main(move || {
        let ended = {
            let mut list = active();
            list.iter()
                .position(|a| a.id == id)
                .map(|at| list.remove(at))
        };
        if let Some(ended) = ended {
            // SAFETY: the window and the controller were made and kept by this code, and are
            // released once, here, on the main thread.
            unsafe {
                let window = ended.window as *mut AnyObject;
                let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
                let _: () = msg_send![window, close];
                drop(Retained::from_raw(window));
                drop(Retained::from_raw(ended.controller as *mut AnyObject));
            }
        }
    });
}

/// Performs ceremony `id` with the Mac's passkeys and waits for the person, at most as long
/// as the relying party allows, or until `withdrawn` is set. Call it off the main thread.
pub fn perform(id: u64, ceremony: Ceremony, withdrawn: Arc<AtomicBool>) -> CeremonyResult {
    let client = match mac_client_data(&ceremony) {
        Ok(client) => client,
        Err(refused) => return refused,
    };
    if !crate::security::has_entitlement(BROWSER_ENTITLEMENT) {
        return CeremonyResult::Unavailable(format!(
            "this copy of DSA is not signed with {BROWSER_ENTITLEMENT}, which Apple grants to \
             programs that perform passkey requests for others, so the Mac's passkeys cannot \
             be used for Android apps"
        ));
    }
    let (outcome_tx, outcome) = channel();
    let (started_tx, started) = channel();
    {
        let ceremony = ceremony.clone();
        on_main(move || {
            let result =
                autoreleasepool(|_| unsafe { start_on_main(id, &ceremony, &client, outcome_tx) });
            let _ = started_tx.send(result);
        });
    }
    match started.recv() {
        Ok(Ok(())) => {}
        Ok(Err(refused)) => return refused,
        Err(_) => return CeremonyResult::Failed("the passkey prompt could not start".into()),
    }
    let deadline = Instant::now() + ceremony.wait();
    let mut canceled_at: Option<Instant> = None;
    let result = loop {
        match outcome.recv_timeout(LOOK) {
            Ok(outcome) => {
                break match (canceled_at, &outcome) {
                    // The prompt ended because it was canceled here.
                    (Some(_), MacOutcome::Error { .. }) => CeremonyResult::Canceled(
                        "the request was withdrawn or its time ran out".into(),
                    ),
                    _ => result_of(&ceremony, outcome),
                };
            }
            Err(RecvTimeoutError::Disconnected) => {
                break CeremonyResult::Failed("the passkey prompt went away".into())
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        match canceled_at {
            None if withdrawn.load(Ordering::Relaxed) || now >= deadline => {
                cancel(id);
                canceled_at = Some(now);
            }
            Some(at) if now.saturating_duration_since(at) >= CANCEL_WAIT => {
                break CeremonyResult::Canceled(
                    "the request was withdrawn or its time ran out".into(),
                );
            }
            _ => {}
        }
    };
    finish(id);
    result
}

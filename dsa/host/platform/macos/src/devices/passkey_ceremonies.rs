//! The passkey ceremonies of the user's Android, performed with the Mac's passkeys.
//!
//! The service checks each request first (the app's signature against the relying party's
//! Digital Asset Links); only a checked one reaches this follower, which watches the
//! ceremonies over the local API ([`serve`]) and hands each to the [`Perform`] function (on a
//! Mac, AuthenticationServices: `crate::devices::passkeys`). What became of it goes back over
//! another connection. A ceremony Android withdraws is canceled. Nothing about a ceremony ends
//! the follower, and the follower reconnects when the service restarts.
//!
//! What macOS can sign: its passkeys sign the hash of client data that macOS writes itself,
//! from the challenge and the origin it is given (a browser's request, which needs the
//! `com.apple.developer.web-browser.public-key-credential` entitlement). macOS takes no hash
//! made elsewhere. So a ceremony is performed only when the client data Android is answered
//! with is exactly what macOS writes ([`mac_client_data`]), and the result is checked again
//! ([`result_of`]): a signature over other client data would fail at the relying party, and
//! is never handed back as a success.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::passkeys::encoding::from_base64url;
use dsa_host_core::passkeys::{Ceremony, CeremonyResult, PasskeyEvent};
use dsa_transport::Connector;
use serde_json::Value;

use super::Log;

/// What the follower calls itself to the service.
const BUILD: &str = "dsa-devices passkeys";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it looks for finished ceremonies again.
const POLL: Duration = Duration::from_millis(100);

/// The error domain of AuthenticationServices' authorization errors.
pub const AUTHORIZATION_ERROR_DOMAIN: &str = "com.apple.AuthenticationServices.AuthorizationError";

/// The COSE algorithm the Mac's passkeys sign with: ES256.
pub const ES256: i32 = -7;

/// Why a ceremony whose client data macOS cannot write is not performed.
pub const OWN_CLIENT_DATA: &str = "macOS signs only client data it writes itself, and this \
     ceremony's client data is not written the way macOS writes it, so the relying party would \
     refuse the passkey";

/// Performs one ceremony (number `id`) and says what became of it. It may block until the
/// person answers; it ends early, canceled, once `withdrawn` is set.
pub type Perform = Arc<dyn Fn(u64, Ceremony, Arc<AtomicBool>) -> CeremonyResult + Send + Sync>;

/// What the Mac's authenticator gave back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacOutcome {
    Registered {
        credential_id: Vec<u8>,
        attestation_object: Vec<u8>,
        client_data_json: Vec<u8>,
    },
    Asserted {
        credential_id: Vec<u8>,
        authenticator_data: Vec<u8>,
        signature: Vec<u8>,
        user_handle: Vec<u8>,
        client_data_json: Vec<u8>,
    },
    /// The ceremony failed: the error's domain, code and text.
    Error {
        domain: String,
        code: i64,
        message: String,
    },
}

/// What macOS is given to write the client data from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacClientData {
    pub challenge: Vec<u8>,
    pub origin: String,
}

/// The client data WebAuthn's serialization makes of these, as a browser engine writes it:
/// `type`, `challenge`, `origin`, `crossOrigin`, in that order, with nothing between.
/// The service writes the ceremony's client data the same way, so the two agree.
pub fn webauthn_client_data(kind: &str, challenge: &[u8], origin: &str) -> String {
    dsa_host_core::passkeys::client_data_json(kind, challenge, origin)
}

/// What macOS is to be given for `ceremony`, or why the Mac cannot perform it: a browser's own
/// client data hash, client data macOS would not write the same, or algorithms it has none
/// of.
pub fn mac_client_data(ceremony: &Ceremony) -> Result<MacClientData, CeremonyResult> {
    if ceremony.client_data_json.is_empty() {
        return Err(CeremonyResult::Unavailable(
            "the browser gave only its own client data hash, and macOS signs only client data \
             it writes itself"
                .into(),
        ));
    }
    if ceremony.create && !ceremony.algorithms.is_empty() && !ceremony.algorithms.contains(&ES256) {
        return Err(CeremonyResult::NotAllowed(
            "the relying party takes none of the algorithms this Mac's passkeys sign with \
             (ES256)"
                .into(),
        ));
    }
    let parsed: Value = serde_json::from_slice(&ceremony.client_data_json)
        .map_err(|_| CeremonyResult::Failed("the client data is not JSON".into()))?;
    let field = |name: &str| parsed.get(name).and_then(Value::as_str).unwrap_or_default();
    let kind = if ceremony.create {
        "webauthn.create"
    } else {
        "webauthn.get"
    };
    let challenge = from_base64url(field("challenge"))
        .filter(|challenge| !challenge.is_empty())
        .ok_or_else(|| CeremonyResult::Failed("the client data has no challenge".into()))?;
    let origin = field("origin").to_string();
    if field("type") != kind || origin.is_empty() {
        return Err(CeremonyResult::Failed(
            "the client data is not for this ceremony".into(),
        ));
    }
    if webauthn_client_data(kind, &challenge, &origin).as_bytes()
        != ceremony.client_data_json.as_slice()
    {
        return Err(CeremonyResult::Unavailable(OWN_CLIENT_DATA.into()));
    }
    Ok(MacClientData { challenge, origin })
}

/// What a failed ceremony means to the app.
pub fn error_result(domain: &str, code: i64, message: &str) -> CeremonyResult {
    let lower = message.to_ascii_lowercase();
    let not_entitled = [
        "entitle",
        "application identifier",
        "not associated",
        "not authorized",
    ]
    .iter()
    .any(|word| lower.contains(word));
    let said = if message.is_empty() {
        format!("{domain} {code}")
    } else {
        format!("{message} ({domain} {code})")
    };
    if domain != AUTHORIZATION_ERROR_DOMAIN {
        return CeremonyResult::Failed(said);
    }
    match code {
        // ASAuthorizationErrorCanceled.
        1001 => CeremonyResult::Canceled(said),
        _ if not_entitled => CeremonyResult::Unavailable(format!(
            "this copy of DSA is not allowed to use the Mac's passkeys for other apps: {said}"
        )),
        // Unknown, InvalidResponse, NotHandled, Failed, NotInteractive and
        // MatchedExcludedCredential: WebAuthn's NotAllowedError (InvalidStateError for an
        // excluded credential, which apps treat alike), which tells the relying party nothing
        // more than that it did not happen.
        _ => CeremonyResult::NotAllowed(said),
    }
}

/// What became of `ceremony`, from what the Mac gave back. A result over client data other
/// than the ceremony's is a failure, never a success.
pub fn result_of(ceremony: &Ceremony, outcome: MacOutcome) -> CeremonyResult {
    let mismatch = |signed: &[u8]| {
        (signed != ceremony.client_data_json.as_slice()).then(|| {
            CeremonyResult::Failed(
                "macOS signed client data other than the ceremony's, so the result was not \
                 used"
                    .into(),
            )
        })
    };
    match outcome {
        MacOutcome::Registered {
            credential_id,
            attestation_object,
            client_data_json,
        } => {
            if !ceremony.create {
                return CeremonyResult::Failed("macOS made a passkey when asked to use one".into());
            }
            if let Some(failed) = mismatch(&client_data_json) {
                return failed;
            }
            if attestation_object.is_empty() {
                return CeremonyResult::Failed("macOS gave the passkey no attestation".into());
            }
            CeremonyResult::Created {
                credential_id,
                attestation_object,
                transports: vec!["internal".into(), "hybrid".into()],
            }
        }
        MacOutcome::Asserted {
            credential_id,
            authenticator_data,
            signature,
            user_handle,
            client_data_json,
        } => {
            if ceremony.create {
                return CeremonyResult::Failed(
                    "macOS used a passkey when asked to make one".into(),
                );
            }
            if let Some(failed) = mismatch(&client_data_json) {
                return failed;
            }
            CeremonyResult::Asserted {
                credential_id,
                authenticator_data,
                signature,
                user_handle,
            }
        }
        MacOutcome::Error {
            domain,
            code,
            message,
        } => error_result(&domain, code, &message),
    }
}

/// Performs `ceremony` with `perform` unless it was withdrawn first. A panic in the passkey
/// code is a failed ceremony, never the end of the follower.
pub fn perform_one(
    id: u64,
    ceremony: Ceremony,
    perform: &Perform,
    withdrawn: &Arc<AtomicBool>,
) -> CeremonyResult {
    if withdrawn.load(Ordering::Relaxed) {
        return CeremonyResult::Canceled("Android withdrew the request".into());
    }
    catch_unwind(AssertUnwindSafe(|| {
        perform(id, ceremony, Arc::clone(withdrawn))
    }))
    .unwrap_or_else(|_| CeremonyResult::Failed("the passkey code failed".into()))
}

/// Performs the passkey ceremonies of the user's Android until `stop`, reconnecting to the
/// service whenever the connection ends. Each ceremony runs on a thread of its own.
pub fn serve(mut connector: Box<dyn Connector>, perform: Perform, stop: Arc<AtomicBool>, log: Log) {
    let (done_tx, done) = mpsc::channel();
    let mut running: HashMap<u64, Arc<AtomicBool>> = HashMap::new();
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let watch = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_passkeys());
        let mut watch = match watch {
            Ok(watch) => watch,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    log(&format!("passkeys cannot reach the service: {error}"));
                    last_error = error;
                }
                report(&mut *connector, &done, &mut running, &log);
                super::wait(&stop, RETRY);
                continue;
            }
        };
        last_error.clear();
        while !stop.load(Ordering::Relaxed) {
            report(&mut *connector, &done, &mut running, &log);
            match watch.next(POLL) {
                Ok(Some(PasskeyEvent::Ceremony { id, ceremony })) => {
                    let withdrawn = Arc::new(AtomicBool::new(false));
                    running.insert(id, Arc::clone(&withdrawn));
                    start(
                        id,
                        *ceremony,
                        Arc::clone(&perform),
                        withdrawn,
                        done_tx.clone(),
                        Arc::clone(&log),
                    );
                }
                Ok(Some(PasskeyEvent::Withdrawn { id })) => {
                    if let Some(withdrawn) = running.get(&id) {
                        withdrawn.store(true, Ordering::Relaxed);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    log(&format!(
                        "the passkey ceremonies' connection to the service ended: {error}"
                    ));
                    break;
                }
            }
        }
    }
    for withdrawn in running.values() {
        withdrawn.store(true, Ordering::Relaxed);
    }
}

fn start(
    id: u64,
    ceremony: Ceremony,
    perform: Perform,
    withdrawn: Arc<AtomicBool>,
    done: Sender<(u64, CeremonyResult)>,
    log: Log,
) {
    let failed = done.clone();
    let spawned = thread::Builder::new()
        .name("dsa-passkey".into())
        .spawn(move || {
            let result = perform_one(id, ceremony, &perform, &withdrawn);
            let _ = done.send((id, result));
        });
    if let Err(error) = spawned {
        log(&format!("passkey ceremony {id} cannot start: {error}"));
        let _ = failed.send((
            id,
            CeremonyResult::Failed(format!("the ceremony could not start: {error}")),
        ));
    }
}

/// Passes what became of each finished ceremony to the service.
fn report(
    connector: &mut dyn Connector,
    done: &Receiver<(u64, CeremonyResult)>,
    running: &mut HashMap<u64, Arc<AtomicBool>>,
    log: &Log,
) {
    while let Ok((id, result)) = done.try_recv() {
        running.remove(&id);
        let reported = HostApiClient::connect(connector, BUILD, ANSWER)
            .and_then(|mut client| client.report_passkey_ceremony(id, &result));
        if let Err(error) = reported {
            // The service may have restarted; its Android no longer waits for this one.
            log(&format!(
                "passkey ceremony {id}: the result did not reach the service: {error}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_ceremony(create: bool) -> Ceremony {
        Ceremony {
            create,
            rp_id: "bank.example".into(),
            client_data_json: br#"{"androidPackageName":"com.example.notes","challenge":"AQID","crossOrigin":false,"origin":"android:apk-key-hash:abc","type":"webauthn.get"}"#.to_vec(),
            origin: "android:apk-key-hash:abc".into(),
            ..Ceremony::default()
        }
    }

    fn mac_ceremony(create: bool) -> Ceremony {
        let kind = if create {
            "webauthn.create"
        } else {
            "webauthn.get"
        };
        Ceremony {
            create,
            rp_id: "bank.example".into(),
            client_data_json: webauthn_client_data(kind, &[1, 2, 3], "https://bank.example")
                .into_bytes(),
            origin: "https://bank.example".into(),
            ..Ceremony::default()
        }
    }

    #[test]
    fn fr36_client_data_follows_webauthns_serialization() {
        assert_eq!(
            webauthn_client_data("webauthn.get", &[1, 2, 3], "https://bank.example"),
            r#"{"type":"webauthn.get","challenge":"AQID","origin":"https://bank.example","crossOrigin":false}"#
        );
    }

    #[test]
    fn fr36_the_mac_performs_only_what_it_can_sign_for() {
        assert_eq!(
            mac_client_data(&mac_ceremony(false)),
            Ok(MacClientData {
                challenge: vec![1, 2, 3],
                origin: "https://bank.example".into()
            })
        );
        // Client data macOS would write differently: refused before any prompt.
        assert_eq!(
            mac_client_data(&app_ceremony(false)),
            Err(CeremonyResult::Unavailable(OWN_CLIENT_DATA.into()))
        );
        // A browser's own hash cannot be signed by macOS.
        let mut hash_only = mac_ceremony(false);
        hash_only.client_data_json.clear();
        hash_only.client_data_hash = vec![9; 32];
        assert!(matches!(
            mac_client_data(&hash_only),
            Err(CeremonyResult::Unavailable(_))
        ));
        // Client data of the other ceremony is not this one's.
        let mut wrong_kind = mac_ceremony(true);
        wrong_kind.create = false;
        assert!(matches!(
            mac_client_data(&wrong_kind),
            Err(CeremonyResult::Failed(_))
        ));
        // A relying party that takes no ES256.
        let mut rsa_only = mac_ceremony(true);
        rsa_only.algorithms = vec![-257];
        assert!(matches!(
            mac_client_data(&rsa_only),
            Err(CeremonyResult::NotAllowed(_))
        ));
    }

    #[test]
    fn fr36_a_mac_result_over_other_client_data_is_never_a_success() {
        let ceremony = mac_ceremony(false);
        let asserted = |client_data_json: Vec<u8>| MacOutcome::Asserted {
            credential_id: vec![1],
            authenticator_data: vec![2],
            signature: vec![3],
            user_handle: vec![4],
            client_data_json,
        };
        assert_eq!(
            result_of(&ceremony, asserted(ceremony.client_data_json.clone())),
            CeremonyResult::Asserted {
                credential_id: vec![1],
                authenticator_data: vec![2],
                signature: vec![3],
                user_handle: vec![4],
            }
        );
        assert!(matches!(
            result_of(&ceremony, asserted(b"{}".to_vec())),
            CeremonyResult::Failed(_)
        ));
        let registered = MacOutcome::Registered {
            credential_id: vec![1],
            attestation_object: vec![2],
            client_data_json: ceremony.client_data_json.clone(),
        };
        assert!(
            matches!(result_of(&ceremony, registered), CeremonyResult::Failed(_)),
            "a passkey made when asked to use one is a failure"
        );
        let created = mac_ceremony(true);
        let registered = MacOutcome::Registered {
            credential_id: vec![1],
            attestation_object: vec![2],
            client_data_json: created.client_data_json.clone(),
        };
        assert!(matches!(
            result_of(&created, registered),
            CeremonyResult::Created { .. }
        ));
    }

    #[test]
    fn fr36_the_macs_errors_map_to_webauthns() {
        assert!(matches!(
            error_result(AUTHORIZATION_ERROR_DOMAIN, 1001, "The user canceled."),
            CeremonyResult::Canceled(_)
        ));
        assert!(matches!(
            error_result(
                AUTHORIZATION_ERROR_DOMAIN,
                1004,
                "Application with identifier X is not associated with domain bank.example"
            ),
            CeremonyResult::Unavailable(_)
        ));
        assert!(matches!(
            error_result(AUTHORIZATION_ERROR_DOMAIN, 1004, "Request failed."),
            CeremonyResult::NotAllowed(_)
        ));
        assert!(matches!(
            error_result(
                AUTHORIZATION_ERROR_DOMAIN,
                1006,
                "Matched an excluded credential."
            ),
            CeremonyResult::NotAllowed(_)
        ));
        assert!(matches!(
            error_result("NSCocoaErrorDomain", 4, "Something"),
            CeremonyResult::Failed(_)
        ));
    }

    #[test]
    fn fr36_a_withdrawn_or_failing_ceremony_ends_without_the_follower() {
        let perform: Perform = Arc::new(|_, _, _| panic!("the passkey code broke"));
        let withdrawn = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            perform_one(1, mac_ceremony(false), &perform, &withdrawn),
            CeremonyResult::Failed(_)
        ));
        withdrawn.store(true, Ordering::Relaxed);
        let never: Perform = Arc::new(|_, _, _| unreachable!("a withdrawn ceremony never runs"));
        assert!(matches!(
            perform_one(2, mac_ceremony(false), &never, &withdrawn),
            CeremonyResult::Canceled(_)
        ));
    }
}

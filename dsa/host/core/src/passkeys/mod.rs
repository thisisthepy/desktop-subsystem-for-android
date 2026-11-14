//! Passkeys for the apps in a user's Android, made and used by the host's own authenticator
//! (Windows Hello through webauthn.dll, macOS passkeys through AuthenticationServices).
//!
//! The agent is the guest's credential provider (`HostCredentialProvider`). When an app asks
//! Android's credential manager to make or use a passkey and the person picks "this
//! computer", the agent sends the request ([`control::PasskeyRequest`]) with what the
//! credential manager says of the caller. [`PasskeyService`] checks it ([`PasskeyGate`]); only
//! a request that passes reaches a watcher, the user's desktop session, which brings up the
//! host's prompt and performs the ceremony. The result goes back to the app as WebAuthn's JSON.
//!
//! # Threat model
//!
//! What is protected: the user's passkeys on the host. A passkey of `bank.example` must sign
//! only for something `bank.example` lets sign in: its website in a browser, or an app it names
//! as its own. The host's authenticator binds every signature to the relying party ID it was
//! given (its SHA-256 is in the signed authenticator data), so the whole question is which
//! relying party ID a request may name. Handing any app's request to the host as it came would
//! let any app sign in to any site as the user, with a prompt that looks like a normal one.
//!
//! Who is trusted, and for what:
//! - Android's framework (system_server) is trusted to say which app called: the package and
//!   the certificates it is signed with (`CallingAppInfo`). An app cannot set either about
//!   itself. The origin the framework reports is not proof on its own: the permission that
//!   lets an app set one (`CREDENTIAL_MANAGER_SET_ORIGIN`) is a normal permission any app may
//!   hold.
//! - The agent, a platform-signed system app, is trusted to pass those on, and to pass an
//!   origin (and a browser's own client data hash) only for a browser it recognises by package
//!   and signing certificate: one on the list of privileged browsers, or one the image carries.
//!   For any other app it sends no origin, so the request is checked as an app's, against the
//!   relying party's asset links. Apps cannot reach the control channel: only the guest's
//!   control daemon holds the boot's secret.
//! - The relying party's own HTTPS server is trusted to say which apps are its own, in its
//!   Digital Asset Links. Nothing an app says about itself, nor anything in its request (such
//!   as an origin it writes into its JSON), is trusted.
//!
//! The rules ([`PasskeyGate::check`]):
//! 1. The relying party ID is a well-formed domain name of at least two labels, not an
//!    address. It is the ID the authenticator signs for; nothing else is used.
//! 2. An app (no origin from the agent) passes only when the relying party's
//!    `assetlinks.json`, fetched over HTTPS from that exact domain without redirects, lists the
//!    app's package with the SHA-256 of every certificate it is signed with, under
//!    `delegate_permission/common.get_login_creds` ([`asset_links`]). The client data then names
//!    the app's own origin (`android:apk-key-hash:<hash of its certificate>`), so the relying
//!    party sees it was an app, and which one.
//! 3. A browser (an origin from the agent) passes only for an `https` origin whose host is
//!    the relying party ID or a name under it, as WebAuthn requires of a web page. Only a
//!    browser may hand over a client data hash of its own; an app that does is refused,
//!    because then nothing would show the relying party who asked.
//! 4. Anything malformed, oversized or unreadable is refused, never guessed at: a fetch that
//!    fails, a statement list that cannot be read, a request whose JSON is not WebAuthn's.
//!
//! What is out of reach: a guest whose system is compromised (an app with root that drives
//! the agent, or the framework itself) can name any package and certificate it likes; the host
//! cannot tell. Such a guest still cannot sign for a relying party without the person passing
//! the host's prompt (Windows Hello, Touch ID), which names the relying party. An attacker who
//! can serve a site's `assetlinks.json` controls that site already.
//!
//! The rejection paths are tested through the public surface in
//! `fr36_passkeys_only_for_verified_relying_parties`.

pub mod asset_links;
pub mod encoding;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_protocol::control::{self, envelope::Body, PasskeyOperation, PasskeyStatus};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::apk;
use asset_links::{AssetLinks, HttpsGet};
use encoding::{base64url, from_base64url};

/// The largest request JSON an app may send.
pub const MAX_REQUEST_JSON: usize = 64 * 1024;

/// How long a ceremony may wait for the person when the relying party sets no timeout, and
/// the shortest and longest a relying party may set.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub const MIN_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// Which ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Create,
    Get,
}

/// A checked ceremony, as the host's authenticator is to perform it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ceremony {
    pub create: bool,
    pub rp_id: String,
    pub rp_name: String,
    pub user_id: Vec<u8>,
    pub user_name: String,
    pub user_display_name: String,
    /// The client data JSON; empty when a browser gave only its hash.
    pub client_data_json: Vec<u8>,
    pub client_data_hash: Vec<u8>,
    pub algorithms: Vec<i32>,
    pub credential_ids: Vec<Vec<u8>>,
    pub resident_key: String,
    pub user_verification: String,
    pub timeout_ms: u32,
    pub app_label: String,
    pub package: String,
    pub origin: String,
    pub attestation: String,
}

impl Ceremony {
    pub fn operation(&self) -> Operation {
        if self.create {
            Operation::Create
        } else {
            Operation::Get
        }
    }

    /// How long the ceremony may wait for the person.
    pub fn wait(&self) -> Duration {
        if self.timeout_ms == 0 {
            DEFAULT_TIMEOUT
        } else {
            Duration::from_millis(self.timeout_ms as u64).clamp(MIN_TIMEOUT, MAX_TIMEOUT)
        }
    }
}

/// What became of a ceremony on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CeremonyResult {
    Created {
        credential_id: Vec<u8>,
        attestation_object: Vec<u8>,
        transports: Vec<String>,
    },
    Asserted {
        credential_id: Vec<u8>,
        authenticator_data: Vec<u8>,
        signature: Vec<u8>,
        user_handle: Vec<u8>,
    },
    Canceled(String),
    Unavailable(String),
    NotAllowed(String),
    Failed(String),
}

/// A refusal: the status the app gets, and why.
pub type Refusal = (PasskeyStatus, String);

fn rejected(why: impl Into<String>) -> Refusal {
    (PasskeyStatus::Rejected, why.into())
}

/// Whether `rp_id` is a domain name a relying party may be: lowercase letters, digits and
/// hyphens in labels of 1 to 63 characters, at least two labels, not all digits at the end (an
/// address), at most 253 characters. International names come in their ASCII form.
pub fn valid_rp_id(rp_id: &str) -> bool {
    if rp_id.is_empty() || rp_id.len() > 253 || rp_id.ends_with('.') {
        return false;
    }
    let labels: Vec<&str> = rp_id.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let label_ok = |label: &&str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if !labels.iter().all(label_ok) {
        return false;
    }
    // The top label of a name is never all digits; such a name is an IPv4 address.
    !labels
        .last()
        .is_some_and(|top| top.chars().all(|c| c.is_ascii_digit()))
}

/// The host of an `https` origin (`https://host[:port]`, nothing after it), lowercased.
pub fn https_origin_host(origin: &str) -> Option<String> {
    let rest = origin.strip_prefix("https://")?;
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '\\']) {
        return None;
    }
    let host = match rest.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        Some(_) => return None,
        None => rest,
    };
    let host = host.to_ascii_lowercase();
    valid_rp_id(&host).then_some(host)
}

/// Whether a page of `host` may use the relying party ID `rp_id`: the same name or a name
/// under it.
pub fn origin_may_use(host: &str, rp_id: &str) -> bool {
    host == rp_id || host.ends_with(&format!(".{rp_id}"))
}

/// The client data of a ceremony, in WebAuthn's own serialization: `type`, `challenge`,
/// `origin` and `crossOrigin`, in that order, with nothing else. It is the form a browser
/// engine writes and the one macOS writes for the client data it signs, so every host
/// authenticator signs the same bytes. Nothing names the app beyond its origin, which already
/// carries its certificate.
pub fn client_data_json(kind: &str, challenge: &[u8], origin: &str) -> String {
    let text = |value: &str| Value::String(value.to_string()).to_string();
    format!(
        "{{\"type\":{},\"challenge\":{},\"origin\":{},\"crossOrigin\":false}}",
        text(kind),
        text(&base64url(challenge)),
        text(origin)
    )
}

/// The origin an app's client data names: its signing certificate's hash.
pub fn app_origin(signer_sha256: &[u8]) -> String {
    format!("android:apk-key-hash:{}", base64url(signer_sha256))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn binary(value: &Value, key: &str) -> Result<Vec<u8>, Refusal> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| rejected(format!("the request has no {key}")))?;
    from_base64url(text).ok_or_else(|| rejected(format!("the request's {key} is not base64url")))
}

fn credential_list(value: &Value, key: &str) -> Result<Vec<Vec<u8>>, Refusal> {
    let Some(list) = value.get(key) else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .ok_or_else(|| rejected(format!("the request's {key} is not a list")))?;
    if list.len() > 64 {
        return Err(rejected(format!("the request lists too many {key}")));
    }
    list.iter().map(|item| binary(item, "id")).collect()
}

/// Checks requests against the rules of the threat model above.
pub struct PasskeyGate {
    links: AssetLinks,
}

impl PasskeyGate {
    pub fn new(fetch: Arc<dyn HttpsGet>) -> PasskeyGate {
        PasskeyGate {
            links: AssetLinks::new(fetch),
        }
    }

    /// The ceremony `request` asks for, if it may be performed; otherwise why not.
    pub fn check(&self, request: &control::PasskeyRequest) -> Result<Ceremony, Refusal> {
        let create = match PasskeyOperation::try_from(request.operation) {
            Ok(PasskeyOperation::Create) => true,
            Ok(PasskeyOperation::Get) => false,
            _ => {
                return Err(rejected(
                    "the request is neither to make nor to use a passkey",
                ))
            }
        };
        if request.request_json.len() > MAX_REQUEST_JSON {
            return Err(rejected("the request is too large"));
        }
        let options: Value = serde_json::from_str(&request.request_json)
            .map_err(|e| rejected(format!("the request is not JSON ({e})")))?;
        if !options.is_object() {
            return Err(rejected("the request is not WebAuthn's options"));
        }
        let browser = !request.origin.is_empty();
        let origin_host = if browser {
            Some(https_origin_host(&request.origin).ok_or_else(|| {
                rejected(format!(
                    "the browser's origin {:?} is not an https origin",
                    request.origin
                ))
            })?)
        } else {
            None
        };
        // The relying party ID: what the request names, or for a browser its origin's host.
        let named = if create {
            options
                .get("rp")
                .map(|rp| text(rp, "id"))
                .unwrap_or_default()
        } else {
            text(&options, "rpId")
        };
        let rp_id = match (named.is_empty(), &origin_host) {
            (false, _) => named.to_ascii_lowercase(),
            (true, Some(host)) => host.clone(),
            (true, None) => return Err(rejected("the request names no relying party")),
        };
        if !valid_rp_id(&rp_id) {
            return Err(rejected(format!(
                "{rp_id:?} is not a domain name a relying party can have"
            )));
        }
        let challenge = binary(&options, "challenge")?;
        if challenge.is_empty() {
            return Err(rejected("the request's challenge is empty"));
        }
        let origin = match &origin_host {
            Some(host) => {
                if !origin_may_use(host, &rp_id) {
                    return Err(rejected(format!(
                        "a page of {host} may not use the passkeys of {rp_id}"
                    )));
                }
                request.origin.to_ascii_lowercase()
            }
            None => {
                if !request.client_data_hash.is_empty() {
                    return Err(rejected(
                        "only a browser may give its own client data; an app's is made here",
                    ));
                }
                if !apk::valid_package_name(&request.package) {
                    return Err(rejected("the calling app has no valid package name"));
                }
                if request.signer_sha256.is_empty()
                    || request.signer_sha256.iter().any(|s| s.len() != 32)
                {
                    return Err(rejected(
                        "Android did not say which certificates the calling app is signed with",
                    ));
                }
                self.links
                    .verify(&rp_id, &request.package, &request.signer_sha256)
                    .map_err(rejected)?;
                app_origin(&request.signer_sha256[0])
            }
        };
        let (client_data_json, client_data_hash) =
            if browser && !request.client_data_hash.is_empty() {
                if request.client_data_hash.len() != 32 {
                    return Err(rejected("the browser's client data hash is not SHA-256"));
                }
                (Vec::new(), request.client_data_hash.clone())
            } else {
                let kind = if create {
                    "webauthn.create"
                } else {
                    "webauthn.get"
                };
                let bytes = client_data_json(kind, &challenge, &origin).into_bytes();
                let hash = Sha256::digest(&bytes).to_vec();
                (bytes, hash)
            };
        let timeout_ms = options
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32;
        let mut ceremony = Ceremony {
            create,
            rp_id,
            client_data_json,
            client_data_hash,
            timeout_ms,
            app_label: request.app_label.chars().take(128).collect(),
            package: request.package.clone(),
            origin,
            ..Ceremony::default()
        };
        if create {
            let rp = options.get("rp").cloned().unwrap_or(Value::Null);
            ceremony.rp_name = text(&rp, "name");
            let user = options
                .get("user")
                .ok_or_else(|| rejected("the request names no account"))?;
            ceremony.user_id = binary(user, "id")?;
            if ceremony.user_id.is_empty() || ceremony.user_id.len() > 64 {
                return Err(rejected(
                    "the account's id is empty or longer than 64 bytes",
                ));
            }
            ceremony.user_name = text(user, "name");
            ceremony.user_display_name = text(user, "displayName");
            ceremony.algorithms = options
                .get("pubKeyCredParams")
                .and_then(Value::as_array)
                .map(|params| {
                    params
                        .iter()
                        .filter(|p| p.get("type").and_then(Value::as_str) == Some("public-key"))
                        .filter_map(|p| p.get("alg").and_then(Value::as_i64))
                        .filter_map(|alg| i32::try_from(alg).ok())
                        .collect()
                })
                .unwrap_or_default();
            if ceremony.algorithms.is_empty() {
                // WebAuthn's default when the list is empty: ES256, then RS256.
                ceremony.algorithms = vec![-7, -257];
            }
            ceremony.credential_ids = credential_list(&options, "excludeCredentials")?;
            let selection = options
                .get("authenticatorSelection")
                .cloned()
                .unwrap_or(Value::Null);
            ceremony.resident_key = match text(&selection, "residentKey").as_str() {
                key @ ("required" | "preferred" | "discouraged") => key.to_string(),
                _ if selection.get("requireResidentKey").and_then(Value::as_bool) == Some(true) => {
                    "required".into()
                }
                // A passkey is a discoverable credential; Android's providers make one.
                _ => "preferred".into(),
            };
            ceremony.user_verification = text(&selection, "userVerification");
            ceremony.attestation = match text(&options, "attestation").as_str() {
                a @ ("none" | "indirect" | "direct" | "enterprise") => a.to_string(),
                _ => "none".into(),
            };
        } else {
            ceremony.credential_ids = credential_list(&options, "allowCredentials")?;
            ceremony.user_verification = text(&options, "userVerification");
        }
        if !matches!(
            ceremony.user_verification.as_str(),
            "required" | "preferred" | "discouraged"
        ) {
            ceremony.user_verification = "preferred".into();
        }
        Ok(ceremony)
    }
}

/// WebAuthn's JSON for the result of `ceremony`: RegistrationResponseJSON for a passkey made,
/// AuthenticationResponseJSON for one used.
pub fn response_json(ceremony: &Ceremony, result: &CeremonyResult) -> Result<String, Refusal> {
    let client_data = base64url(&ceremony.client_data_json);
    let value = match result {
        CeremonyResult::Created {
            credential_id,
            attestation_object,
            transports,
        } => {
            if !ceremony.create {
                return Err((
                    PasskeyStatus::Error,
                    "the host made a passkey when asked to use one".into(),
                ));
            }
            let attested = encoding::attested_credential(attestation_object).ok_or_else(|| {
                (
                    PasskeyStatus::Error,
                    "the host's authenticator gave an attestation that cannot be read".to_string(),
                )
            })?;
            let id = if credential_id.is_empty() {
                attested.credential_id.clone()
            } else {
                credential_id.clone()
            };
            let mut response = json!({
                "clientDataJSON": client_data,
                "attestationObject": base64url(attestation_object),
                "authenticatorData": base64url(&attested.authenticator_data),
                "transports": if transports.is_empty() { vec!["internal".to_string(), "hybrid".to_string()] } else { transports.clone() },
                "publicKeyAlgorithm": attested.algorithm,
            });
            if let Some(der) = &attested.public_key_der {
                response["publicKey"] = Value::String(base64url(der));
            }
            json!({
                "id": base64url(&id),
                "rawId": base64url(&id),
                "type": "public-key",
                "authenticatorAttachment": "platform",
                "response": response,
                "clientExtensionResults": { "credProps": { "rk": true } },
            })
        }
        CeremonyResult::Asserted {
            credential_id,
            authenticator_data,
            signature,
            user_handle,
        } => {
            if ceremony.create {
                return Err((
                    PasskeyStatus::Error,
                    "the host used a passkey when asked to make one".into(),
                ));
            }
            let mut response = json!({
                "clientDataJSON": client_data,
                "authenticatorData": base64url(authenticator_data),
                "signature": base64url(signature),
            });
            if !user_handle.is_empty() {
                response["userHandle"] = Value::String(base64url(user_handle));
            }
            json!({
                "id": base64url(credential_id),
                "rawId": base64url(credential_id),
                "type": "public-key",
                "authenticatorAttachment": "platform",
                "response": response,
                "clientExtensionResults": {},
            })
        }
        CeremonyResult::Canceled(why) => return Err((PasskeyStatus::Canceled, why.clone())),
        CeremonyResult::Unavailable(why) => return Err((PasskeyStatus::Unavailable, why.clone())),
        CeremonyResult::NotAllowed(why) => return Err((PasskeyStatus::NotAllowed, why.clone())),
        CeremonyResult::Failed(why) => return Err((PasskeyStatus::Error, why.clone())),
    };
    Ok(value.to_string())
}

/// What a watcher of passkey ceremonies hears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasskeyEvent {
    Ceremony { id: u64, ceremony: Box<Ceremony> },
    Withdrawn { id: u64 },
}

/// How a service answers the guest.
pub type GuestReply = Arc<dyn Fn(Body) + Send + Sync>;

/// Why no ceremony can run when no desktop session watches.
pub const NO_AUTHENTICATOR: &str = "this computer's passkeys cannot be reached: DSA uses Windows \
     Hello on Windows and passkeys on macOS, through the desktop session, which is not running";

/// The passkey requests of one user's Android: checked, handed to one watcher, and answered.
pub struct PasskeyService {
    gate: PasskeyGate,
    watchers: Mutex<Vec<Sender<PasskeyEvent>>>,
    pending: Mutex<HashMap<u64, Sender<CeremonyResult>>>,
    next: AtomicU64,
    reply: Mutex<Option<GuestReply>>,
    no_authenticator: String,
}

impl PasskeyService {
    pub fn new(fetch: Arc<dyn HttpsGet>) -> PasskeyService {
        PasskeyService {
            gate: PasskeyGate::new(fetch),
            watchers: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            reply: Mutex::new(None),
            no_authenticator: NO_AUTHENTICATOR.into(),
        }
    }

    /// Says `why` (in place of [`NO_AUTHENTICATOR`]) to a checked request that no desktop
    /// session watches for, as a host that has no passkeys of its own says why.
    pub fn with_no_authenticator_reason(mut self, why: impl Into<String>) -> PasskeyService {
        self.no_authenticator = why.into();
        self
    }

    /// Answers go to the guest through `reply`.
    pub fn attach(&self, reply: Arc<dyn Fn(Body) + Send + Sync>) {
        *self.reply.lock().unwrap_or_else(|e| e.into_inner()) = Some(reply);
    }

    /// The ceremonies from now on. The watch lasts across restarts of the guest.
    pub fn watch(&self) -> Receiver<PasskeyEvent> {
        let (tx, rx) = mpsc::channel();
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    fn answer(&self, request_id: u64, outcome: Result<String, Refusal>) {
        let (status, response_json, message) = match outcome {
            Ok(json) => (PasskeyStatus::Ok, json, String::new()),
            Err((status, why)) => (status, String::new(), why),
        };
        let reply = self.reply.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(reply) = reply {
            reply(Body::PasskeyResponse(control::PasskeyResponse {
                request_id,
                status: status as i32,
                response_json,
                message,
            }));
        }
    }

    /// Takes a request of the agent: checks it, and on a thread of its own hands it to a
    /// watcher and answers once the ceremony has ended.
    pub fn request(self: &Arc<Self>, request: control::PasskeyRequest) {
        let service = Arc::clone(self);
        let request_id = request.request_id;
        let spawned = thread::Builder::new()
            .name("dsa-passkey".into())
            .spawn(move || {
                let outcome = service.serve(&request);
                if let Err((status, why)) = &outcome {
                    eprintln!(
                        "passkeys: {} asked {}: {:?}, {why}",
                        request.package,
                        if request.operation == PasskeyOperation::Create as i32 {
                            "to make a passkey"
                        } else {
                            "to use a passkey"
                        },
                        status
                    );
                }
                service.answer(request.request_id, outcome);
            });
        if let Err(error) = spawned {
            self.answer(
                request_id,
                Err((
                    PasskeyStatus::Error,
                    format!("the host could not take the request: {error}"),
                )),
            );
        }
    }

    fn serve(&self, request: &control::PasskeyRequest) -> Result<String, Refusal> {
        let ceremony = self.gate.check(request)?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let handed = {
            let mut watchers = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
            let mut handed = false;
            watchers.retain(|watcher| {
                if handed {
                    return true;
                }
                let sent = watcher
                    .send(PasskeyEvent::Ceremony {
                        id,
                        ceremony: Box::new(ceremony.clone()),
                    })
                    .is_ok();
                handed = sent;
                sent
            });
            handed
        };
        let result = if handed {
            match rx.recv_timeout(ceremony.wait() + Duration::from_secs(5)) {
                Ok(result) => result,
                Err(_) => {
                    self.withdraw(id);
                    CeremonyResult::Canceled("nobody answered the passkey prompt in time".into())
                }
            }
        } else {
            CeremonyResult::Unavailable(self.no_authenticator.clone())
        };
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        response_json(&ceremony, &result)
    }

    fn withdraw(&self, id: u64) {
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(PasskeyEvent::Withdrawn { id }).is_ok());
    }

    /// What became of the ceremony `id` a watcher was given. One nobody waits for any more is
    /// an error.
    pub fn report(&self, id: u64, result: CeremonyResult) -> Result<(), String> {
        let waiting = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        match waiting {
            Some(waiting) => {
                let _ = waiting.send(result);
                Ok(())
            }
            None => Err(format!("passkey ceremony {id} is no longer waited for")),
        }
    }

    /// Android stopped: every ceremony under way is withdrawn and ends as canceled.
    pub fn guest_gone(&self) {
        let pending: Vec<(u64, Sender<CeremonyResult>)> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        for (id, waiting) in pending {
            self.withdraw(id);
            let _ = waiting.send(CeremonyResult::Canceled("Android stopped".into()));
        }
    }
}

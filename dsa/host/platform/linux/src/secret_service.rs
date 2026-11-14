//! The keys DSA keeps for each user (their backup key and their Android's KeyMint root key) in
//! the user's Secret Service, `org.freedesktop.secrets` on their session bus: GNOME Keyring,
//! KWallet or KeePassXC, whichever their desktop runs.
//!
//! The service runs as root and cannot use a user's session bus: the bus accepts only its own
//! user. Each load or store therefore runs in a short helper process started as the user
//! (`dsa-linux-hostd secret-service-helper`, with the user's IDs and their bus's address and
//! nothing else of the service's environment). The request and the secret pass over the
//! helper's standard input and output, never its arguments or environment, which other
//! processes can read.
//!
//! What the helper says to the Secret Service:
//! - `OpenSession` with the `plain` algorithm. The secret crosses only the user's own bus
//!   socket (`/run/user/<uid>/bus`, which only the user and root can open). The only parties
//!   that could watch it there, a bus monitor of the same user or root, could as well ask the
//!   Secret Service for the item, so the transport encryption the other algorithm offers would
//!   protect it from nobody.
//! - `SearchItems` for the attributes `application` (`io.github.thisisthepy.DSA`) and
//!   `purpose` (the key's name), and `Unlock` for an item that is locked. Unlocking may need a
//!   prompt, which the user's keyring shows on their desktop. A keyring that stays locked
//!   (the prompt was dismissed, or nobody answered it) makes the key unavailable, with that
//!   reason.
//! - `CreateItem` in the collection of the `default` alias, replacing an item with the same
//!   attributes. A desktop without a default collection has one made, as libsecret does.
//!
//! The rules are plain functions over [`SecretBus`], tested with a fake; the bus itself and the
//! helper process build on Linux only.

use std::time::Duration;

use dsa_host_core::host_keys::{KeyError, KeyPurpose, Secret};

/// The `application` attribute of every item DSA keeps.
pub const APPLICATION: &str = "io.github.thisisthepy.DSA";

/// The hidden subcommand of `dsa-linux-hostd` that runs the helper.
pub const HELPER_ARG: &str = "secret-service-helper";

/// How long a prompt to unlock the keyring may stay unanswered.
pub const PROMPT_WAIT: Duration = Duration::from_secs(120);

/// The algorithm the helper opens its session with.
pub const PLAIN: &str = "plain";

/// The alias of the collection new items go into.
pub const DEFAULT_ALIAS: &str = "default";

/// The label of a default collection the helper makes, as libsecret names it.
pub const DEFAULT_COLLECTION_LABEL: &str = "Default keyring";

/// The object path that means "none" in the Secret Service's answers.
pub const NO_OBJECT: &str = "/";

/// The largest request or answer that passes over the helper's pipes.
pub const MAX_MESSAGE: usize = 4096;

/// The attributes that find the item of `purpose`.
pub fn attributes(purpose: KeyPurpose) -> [(&'static str, &'static str); 2] {
    [("application", APPLICATION), ("purpose", purpose.name())]
}

/// The purpose whose [`KeyPurpose::name`] is `name`.
pub fn purpose_named(name: &str) -> Option<KeyPurpose> {
    [KeyPurpose::Backup, KeyPurpose::KeyMintRoot]
        .into_iter()
        .find(|purpose| purpose.name() == name)
}

/// A secret as the Secret Service passes it: `(oayays)`.
#[derive(Clone, PartialEq, Eq)]
pub struct WireSecret {
    pub session: String,
    pub parameters: Vec<u8>,
    pub value: Vec<u8>,
    pub content_type: String,
}

impl Drop for WireSecret {
    fn drop(&mut self) {
        wipe(&mut self.value);
    }
}

impl std::fmt::Debug for WireSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WireSecret({}, {} bytes)",
            self.session,
            self.value.len()
        )
    }
}

/// Overwrites `bytes` with zeros in a way the compiler keeps.
fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: a valid, aligned pointer to a byte the slice owns.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

/// How a call on the bus failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusError {
    /// Nothing provides `org.freedesktop.secrets` on the bus, or there is no bus.
    NoService(String),
    /// The call failed; the text says how.
    Failed(String),
}

/// What became of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOutcome {
    /// The user answered it; the objects it gives (unlocked items, a made item).
    Completed(Vec<String>),
    /// The user dismissed it.
    Dismissed,
}

/// The Secret Service's calls the helper makes, so the rules can run against a fake.
pub trait SecretBus {
    /// `OpenSession("plain", "")`: the session's path.
    fn open_session(&self) -> Result<String, BusError>;
    /// `Session.Close`.
    fn close_session(&self, session: &str);
    /// `SearchItems`: the unlocked items and the locked ones.
    fn search_items(
        &self,
        attributes: &[(&str, &str)],
    ) -> Result<(Vec<String>, Vec<String>), BusError>;
    /// `Unlock`: the objects unlocked at once, and a prompt (or [`NO_OBJECT`]).
    fn unlock(&self, objects: &[String]) -> Result<(Vec<String>, String), BusError>;
    /// `Prompt.Prompt`, waiting for `Completed`.
    fn prompt(&self, prompt: &str) -> Result<PromptOutcome, BusError>;
    /// `Item.GetSecret`.
    fn get_secret(&self, item: &str, session: &str) -> Result<WireSecret, BusError>;
    /// `ReadAlias`: the collection, or [`NO_OBJECT`].
    fn read_alias(&self, alias: &str) -> Result<String, BusError>;
    /// `CreateCollection`: the collection (or [`NO_OBJECT`]) and a prompt.
    fn create_collection(&self, label: &str, alias: &str) -> Result<(String, String), BusError>;
    /// `Collection.CreateItem`: the item (or [`NO_OBJECT`]) and a prompt.
    fn create_item(
        &self,
        collection: &str,
        label: &str,
        attributes: &[(&str, &str)],
        secret: &WireSecret,
        replace: bool,
    ) -> Result<(String, String), BusError>;
    /// `Item.Delete`: a prompt (or [`NO_OBJECT`]).
    fn delete_item(&self, item: &str) -> Result<String, BusError>;
}

/// What a person reads when no Secret Service runs.
pub fn no_service_reason(detail: &str) -> String {
    format!(
        "no Secret Service runs in your desktop session (GNOME Keyring, KWallet or KeePassXC \
         provide one), so DSA has nowhere safe to keep its keys ({detail})"
    )
}

/// Whether a D-Bus error's text says nothing provides the service.
pub fn is_missing_service(text: &str) -> bool {
    [
        "ServiceUnknown",
        "NameHasNoOwner",
        "not provided by any .service files",
        "was not provided by any",
    ]
    .iter()
    .any(|word| text.contains(word))
}

fn failed(what: &str) -> impl Fn(BusError) -> KeyError + '_ {
    move |error| match error {
        BusError::NoService(detail) => KeyError::Unavailable(no_service_reason(&detail)),
        BusError::Failed(detail) => {
            KeyError::Failed(format!("the Secret Service failed {what}: {detail}"))
        }
    }
}

fn is_none(path: &str) -> bool {
    path.is_empty() || path == NO_OBJECT
}

/// Waits for `prompt` when there is one; `what` says what it is for, for the reason.
fn answer_prompt(bus: &dyn SecretBus, prompt: &str, what: &str) -> Result<Vec<String>, KeyError> {
    if is_none(prompt) {
        return Ok(Vec::new());
    }
    match bus.prompt(prompt).map_err(failed(what))? {
        PromptOutcome::Completed(objects) => Ok(objects),
        PromptOutcome::Dismissed => Err(KeyError::Unavailable(format!(
            "the keyring that keeps DSA's keys is locked, and the prompt {what} was dismissed"
        ))),
    }
}

/// Unlocks `object`, through a prompt when the keyring asks for one.
fn unlock(bus: &dyn SecretBus, object: &str) -> Result<(), KeyError> {
    let (mut unlocked, prompt) = bus
        .unlock(&[object.to_string()])
        .map_err(failed("to unlock the keyring"))?;
    unlocked.extend(answer_prompt(bus, &prompt, "to unlock it")?);
    if unlocked.iter().any(|done| done == object) {
        Ok(())
    } else {
        Err(KeyError::Unavailable(
            "the keyring that keeps DSA's keys is locked and was not unlocked".into(),
        ))
    }
}

/// Runs `f` in a session of its own, closed afterwards.
fn in_session<T>(
    bus: &dyn SecretBus,
    f: impl FnOnce(&str) -> Result<T, KeyError>,
) -> Result<T, KeyError> {
    let session = bus.open_session().map_err(failed("to open a session"))?;
    let result = f(&session);
    bus.close_session(&session);
    result
}

/// The secret kept for `purpose`; `Ok(None)` when none is kept.
pub fn load(bus: &dyn SecretBus, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
    in_session(bus, |session| {
        let (unlocked, locked) = bus
            .search_items(&attributes(purpose))
            .map_err(failed("to search the keyring"))?;
        let item = match (unlocked.first(), locked.first()) {
            (Some(item), _) => item.clone(),
            (None, Some(item)) => {
                unlock(bus, item)?;
                item.clone()
            }
            (None, None) => return Ok(None),
        };
        let wire = bus
            .get_secret(&item, session)
            .map_err(failed("to give the key"))?;
        Secret::from_slice(&wire.value).map(Some).ok_or_else(|| {
            KeyError::Unreadable(format!(
                "the {} in the keyring is {} bytes long, not 32; it was not made by DSA",
                purpose.description(),
                wire.value.len()
            ))
        })
    })
}

/// Keeps `secret` for `purpose`, in place of what was kept.
pub fn store(bus: &dyn SecretBus, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
    in_session(bus, |session| {
        let mut collection = bus
            .read_alias(DEFAULT_ALIAS)
            .map_err(failed("to name the default keyring"))?;
        if is_none(&collection) {
            let (made, prompt) = bus
                .create_collection(DEFAULT_COLLECTION_LABEL, DEFAULT_ALIAS)
                .map_err(failed("to make a keyring"))?;
            collection = made;
            if is_none(&collection) {
                collection = answer_prompt(bus, &prompt, "to make a keyring")?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        KeyError::Unavailable("the desktop made no keyring for DSA's keys".into())
                    })?;
            }
        }
        unlock(bus, &collection)?;
        let wire = WireSecret {
            session: session.to_string(),
            parameters: Vec::new(),
            value: secret.bytes().to_vec(),
            content_type: "application/octet-stream".into(),
        };
        let (item, prompt) = bus
            .create_item(
                &collection,
                purpose.description(),
                &attributes(purpose),
                &wire,
                true,
            )
            .map_err(failed("to keep the key"))?;
        if is_none(&item) && answer_prompt(bus, &prompt, "to keep the key")?.is_empty() {
            return Err(KeyError::Failed(format!(
                "the keyring did not keep the {}",
                purpose.description()
            )));
        }
        Ok(())
    })
}

/// Removes every item kept for `purpose`; none kept is no error.
pub fn remove(bus: &dyn SecretBus, purpose: KeyPurpose) -> Result<(), KeyError> {
    let (unlocked, locked) = bus
        .search_items(&attributes(purpose))
        .map_err(failed("to search the keyring"))?;
    for item in &locked {
        unlock(bus, item)?;
    }
    for item in unlocked.iter().chain(locked.iter()) {
        let prompt = bus.delete_item(item).map_err(failed("to remove the key"))?;
        answer_prompt(bus, &prompt, "to remove the key")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// What passes over the helper's pipes.

/// What the service asks the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperRequest {
    Load(KeyPurpose),
    Store(KeyPurpose, Secret),
    Remove(KeyPurpose),
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `load <purpose>\n` or `store <purpose> <hex>\n`.
pub fn encode_request(request: &HelperRequest) -> Vec<u8> {
    match request {
        HelperRequest::Load(purpose) => format!("load {}\n", purpose.name()).into_bytes(),
        HelperRequest::Remove(purpose) => format!("remove {}\n", purpose.name()).into_bytes(),
        HelperRequest::Store(purpose, secret) => {
            format!("store {} {}\n", purpose.name(), hex(secret.bytes())).into_bytes()
        }
    }
}

pub fn decode_request(data: &[u8]) -> Result<HelperRequest, String> {
    let text = std::str::from_utf8(data).map_err(|_| "the request is not text".to_string())?;
    let mut words = text.trim_end_matches('\n').split(' ');
    let verb = words.next().unwrap_or_default();
    let purpose = words
        .next()
        .and_then(purpose_named)
        .ok_or("the request names no key DSA keeps")?;
    let request = match (verb, words.next()) {
        ("load", None) => HelperRequest::Load(purpose),
        ("remove", None) => HelperRequest::Remove(purpose),
        ("store", Some(value)) => {
            let mut bytes = unhex(value).ok_or("the key in the request is not hex")?;
            let secret = Secret::from_slice(&bytes);
            wipe(&mut bytes);
            HelperRequest::Store(
                purpose,
                secret.ok_or("the key in the request is not 32 bytes")?,
            )
        }
        _ => return Err(format!("the request {verb:?} is not load, store or remove")),
    };
    if words.next().is_some() {
        return Err("the request has more words than it should".into());
    }
    Ok(request)
}

/// What the helper answers: `secret <hex>`, `none`, or the error's kind and reason, on one
/// line.
pub fn encode_answer(answer: &Result<Option<Secret>, KeyError>) -> Vec<u8> {
    let line = |kind: &str, why: &str| format!("{kind} {}\n", why.replace(['\n', '\r'], " "));
    match answer {
        Ok(Some(secret)) => format!("secret {}\n", hex(secret.bytes())),
        Ok(None) => "none\n".into(),
        Err(KeyError::Unavailable(why)) => line("unavailable", why),
        Err(KeyError::Unreadable(why)) => line("unreadable", why),
        Err(KeyError::Failed(why)) => line("failed", why),
    }
    .into_bytes()
}

pub fn decode_answer(data: &[u8]) -> Result<Option<Secret>, KeyError> {
    let unreadable = || KeyError::Failed("the Secret Service helper's answer is unreadable".into());
    let text = std::str::from_utf8(data).map_err(|_| unreadable())?;
    let text = text.strip_suffix('\n').ok_or_else(unreadable)?;
    let (kind, rest) = text.split_once(' ').unwrap_or((text, ""));
    match kind {
        "none" if rest.is_empty() => Ok(None),
        "secret" => {
            let mut bytes = unhex(rest).ok_or_else(unreadable)?;
            let secret = Secret::from_slice(&bytes);
            wipe(&mut bytes);
            secret.map(Some).ok_or_else(unreadable)
        }
        "unavailable" => Err(KeyError::Unavailable(rest.to_string())),
        "unreadable" => Err(KeyError::Unreadable(rest.to_string())),
        "failed" => Err(KeyError::Failed(rest.to_string())),
        _ => Err(unreadable()),
    }
}

/// Runs `request` against `bus`, as the helper does.
pub fn serve_request(
    bus: &dyn SecretBus,
    request: &HelperRequest,
) -> Result<Option<Secret>, KeyError> {
    match request {
        HelperRequest::Load(purpose) => load(bus, *purpose),
        HelperRequest::Store(purpose, secret) => store(bus, *purpose, secret).map(|()| None),
        HelperRequest::Remove(purpose) => remove(bus, *purpose).map(|()| None),
    }
}

/// The session bus address of the user with `uid`, and the socket it names.
pub fn user_bus(uid: u32) -> (String, std::path::PathBuf) {
    let socket = std::path::PathBuf::from(format!("/run/user/{uid}/bus"));
    (format!("unix:path={}", socket.display()), socket)
}

#[cfg(target_os = "linux")]
pub use linux::{helper_main, SecretServiceKeys};

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::process::{Command, ExitCode, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::thread;
    use std::time::Duration;

    use dsa_host_core::host_keys::{HostKeyStore, KeyError, KeyPurpose, Secret};
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

    use super::*;
    use crate::users::Account;

    const SERVICE: &str = "org.freedesktop.secrets";
    const SERVICE_PATH: &str = "/org/freedesktop/secrets";
    const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
    const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
    const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
    const PROMPT_IFACE: &str = "org.freedesktop.Secret.Prompt";
    const SESSION_IFACE: &str = "org.freedesktop.Secret.Session";

    /// How long the service waits for a helper beyond the prompt's own wait.
    const HELPER_MARGIN: Duration = Duration::from_secs(30);

    fn bus_error(error: zbus::Error) -> BusError {
        let text = error.to_string();
        if is_missing_service(&text) {
            BusError::NoService(text)
        } else {
            BusError::Failed(text)
        }
    }

    fn path(text: &str) -> Result<ObjectPath<'_>, BusError> {
        ObjectPath::try_from(text).map_err(|e| BusError::Failed(format!("{text}: {e}")))
    }

    /// The objects a prompt's result names: `o` or `ao`.
    fn objects_in(value: &Value<'_>) -> Vec<String> {
        match value {
            Value::ObjectPath(path) => vec![path.to_string()],
            Value::Array(array) => array.inner().iter().flat_map(objects_in).collect(),
            Value::Value(inner) => objects_in(inner),
            _ => Vec::new(),
        }
    }

    /// The Secret Service on the session bus.
    struct ZbusSecrets {
        connection: Connection,
    }

    impl ZbusSecrets {
        fn call<B, R>(
            &self,
            object: &str,
            interface: &str,
            method: &str,
            body: &B,
        ) -> Result<R, BusError>
        where
            B: serde::Serialize + zbus::zvariant::DynamicType,
            R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
        {
            let reply = self
                .connection
                .call_method(Some(SERVICE), path(object)?, Some(interface), method, body)
                .map_err(bus_error)?;
            reply
                .body()
                .deserialize::<R>()
                .map_err(|e| BusError::Failed(format!("{method} answered unreadably: {e}")))
        }
    }

    impl SecretBus for ZbusSecrets {
        fn open_session(&self) -> Result<String, BusError> {
            let (_, session): (OwnedValue, OwnedObjectPath) = self.call(
                SERVICE_PATH,
                SERVICE_IFACE,
                "OpenSession",
                &(PLAIN, Value::from("")),
            )?;
            Ok(session.to_string())
        }

        fn close_session(&self, session: &str) {
            if let Ok(object) = path(session) {
                let _ = self.connection.call_method(
                    Some(SERVICE),
                    object,
                    Some(SESSION_IFACE),
                    "Close",
                    &(),
                );
            }
        }

        fn search_items(
            &self,
            attributes: &[(&str, &str)],
        ) -> Result<(Vec<String>, Vec<String>), BusError> {
            let wanted: HashMap<&str, &str> = attributes.iter().copied().collect();
            let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
                self.call(SERVICE_PATH, SERVICE_IFACE, "SearchItems", &(wanted,))?;
            let names = |paths: Vec<OwnedObjectPath>| paths.iter().map(|p| p.to_string()).collect();
            Ok((names(unlocked), names(locked)))
        }

        fn unlock(&self, objects: &[String]) -> Result<(Vec<String>, String), BusError> {
            let objects = objects
                .iter()
                .map(|object| path(object))
                .collect::<Result<Vec<_>, _>>()?;
            let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
                self.call(SERVICE_PATH, SERVICE_IFACE, "Unlock", &(objects,))?;
            Ok((
                unlocked.iter().map(|p| p.to_string()).collect(),
                prompt.to_string(),
            ))
        }

        fn prompt(&self, prompt: &str) -> Result<PromptOutcome, BusError> {
            let proxy = Proxy::new(&self.connection, SERVICE, path(prompt)?, PROMPT_IFACE)
                .map_err(bus_error)?;
            // Watched before the prompt is shown, so its answer cannot come first.
            let mut completed = proxy.receive_signal("Completed").map_err(bus_error)?;
            proxy
                .call::<_, _, ()>("Prompt", &("",))
                .map_err(bus_error)?;
            let message = completed
                .next()
                .ok_or_else(|| BusError::Failed("the Secret Service went away".into()))?;
            let (dismissed, result): (bool, OwnedValue) = message
                .body()
                .deserialize()
                .map_err(|e| BusError::Failed(format!("the prompt answered unreadably: {e}")))?;
            Ok(if dismissed {
                PromptOutcome::Dismissed
            } else {
                PromptOutcome::Completed(objects_in(&result))
            })
        }

        fn get_secret(&self, item: &str, session: &str) -> Result<WireSecret, BusError> {
            let (session, parameters, value, content_type): (
                OwnedObjectPath,
                Vec<u8>,
                Vec<u8>,
                String,
            ) = self.call(item, ITEM_IFACE, "GetSecret", &(path(session)?,))?;
            Ok(WireSecret {
                session: session.to_string(),
                parameters,
                value,
                content_type,
            })
        }

        fn read_alias(&self, alias: &str) -> Result<String, BusError> {
            let collection: OwnedObjectPath =
                self.call(SERVICE_PATH, SERVICE_IFACE, "ReadAlias", &(alias,))?;
            Ok(collection.to_string())
        }

        fn create_collection(
            &self,
            label: &str,
            alias: &str,
        ) -> Result<(String, String), BusError> {
            let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
            properties.insert(
                "org.freedesktop.Secret.Collection.Label",
                Value::from(label),
            );
            let (collection, prompt): (OwnedObjectPath, OwnedObjectPath) = self.call(
                SERVICE_PATH,
                SERVICE_IFACE,
                "CreateCollection",
                &(properties, alias),
            )?;
            Ok((collection.to_string(), prompt.to_string()))
        }

        fn create_item(
            &self,
            collection: &str,
            label: &str,
            attributes: &[(&str, &str)],
            secret: &WireSecret,
            replace: bool,
        ) -> Result<(String, String), BusError> {
            let attributes: HashMap<String, String> = attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
            properties.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
            properties.insert(
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(attributes),
            );
            let wire = (
                path(&secret.session)?,
                secret.parameters.clone(),
                secret.value.clone(),
                secret.content_type.as_str(),
            );
            let made = self.call(
                collection,
                COLLECTION_IFACE,
                "CreateItem",
                &(properties, &wire, replace),
            );
            let mut sent = wire.2;
            super::wipe(&mut sent);
            let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = made?;
            Ok((item.to_string(), prompt.to_string()))
        }

        fn delete_item(&self, item: &str) -> Result<String, BusError> {
            let prompt: OwnedObjectPath = self.call(item, ITEM_IFACE, "Delete", &())?;
            Ok(prompt.to_string())
        }
    }

    /// Whether the helper has written its answer, so the watchdog and the request do not
    /// both write one.
    static ANSWERED: AtomicBool = AtomicBool::new(false);

    fn answer_once(answer: &Result<Option<Secret>, KeyError>) {
        if ANSWERED.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut out = std::io::stdout().lock();
        let mut line = encode_answer(answer);
        let _ = out.write_all(&line);
        let _ = out.flush();
        super::wipe(&mut line);
    }

    /// `dsa-linux-hostd secret-service-helper`: reads one request from standard input, runs
    /// it against the session bus `DBUS_SESSION_BUS_ADDRESS` names, and writes the answer to
    /// standard output.
    pub fn helper_main() -> ExitCode {
        let mut request = Vec::new();
        let read = std::io::stdin()
            .lock()
            .take(MAX_MESSAGE as u64)
            .read_to_end(&mut request);
        let request = match read
            .map_err(|e| e.to_string())
            .and_then(|_| decode_request(&request))
        {
            Ok(request) => request,
            Err(why) => {
                answer_once(&Err(KeyError::Failed(format!(
                    "the Secret Service helper was asked wrongly: {why}"
                ))));
                return ExitCode::FAILURE;
            }
        };
        // An unanswered prompt ends the wait, with the reason.
        thread::spawn(|| {
            thread::sleep(PROMPT_WAIT);
            answer_once(&Err(KeyError::Unavailable(format!(
                "the keyring that keeps DSA's keys is locked, and nobody unlocked it within {} \
                 minutes",
                PROMPT_WAIT.as_secs() / 60
            ))));
            std::process::exit(0);
        });
        let answer = match Connection::session() {
            Ok(connection) => serve_request(&ZbusSecrets { connection }, &request),
            Err(error) => Err(KeyError::Unavailable(format!(
                "your desktop session's bus cannot be reached, so DSA cannot use its Secret \
                 Service: {error}"
            ))),
        };
        answer_once(&answer);
        ExitCode::SUCCESS
    }

    /// A user's keys in their Secret Service, through the helper running as them.
    pub struct SecretServiceKeys {
        account: Account,
        helper: PathBuf,
        /// What was loaded or kept, so the keyring is asked once per run of the service.
        known: Mutex<HashMap<KeyPurpose, Secret>>,
    }

    impl SecretServiceKeys {
        /// The keys of `account`. The helper is this very program, which the user can run.
        pub fn new(account: Account) -> SecretServiceKeys {
            SecretServiceKeys {
                account,
                helper: PathBuf::from("/proc/self/exe"),
                known: Mutex::new(HashMap::new()),
            }
        }

        fn known(&self) -> std::sync::MutexGuard<'_, HashMap<KeyPurpose, Secret>> {
            self.known.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn ask(&self, request: &HelperRequest) -> Result<Option<Secret>, KeyError> {
            let (address, socket) = user_bus(self.account.uid);
            if !socket.exists() {
                return Err(KeyError::Unavailable(format!(
                    "{} has no desktop session running, so their Secret Service cannot keep \
                     DSA's keys; sign in to the desktop first",
                    self.account.name
                )));
            }
            let mut command = Command::new(&self.helper);
            command.arg(HELPER_ARG);
            self.account.run_as(&mut command, None);
            command
                .env("DBUS_SESSION_BUS_ADDRESS", address)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let mut child = command.spawn().map_err(|e| {
                KeyError::Failed(format!("the Secret Service helper cannot start: {e}"))
            })?;
            let mut sent = encode_request(request);
            let wrote = child
                .stdin
                .take()
                .map(|mut stdin| stdin.write_all(&sent))
                .unwrap_or_else(|| Err(std::io::Error::other("no input")));
            super::wipe(&mut sent);
            if let Err(error) = wrote {
                let _ = child.kill();
                let _ = child.wait();
                return Err(KeyError::Failed(format!(
                    "the Secret Service helper took no request: {error}"
                )));
            }
            let mut stdout = child.stdout.take().ok_or_else(|| {
                KeyError::Failed("the Secret Service helper gave no output".into())
            })?;
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let mut answer = Vec::new();
                let read = (&mut stdout)
                    .take(MAX_MESSAGE as u64)
                    .read_to_end(&mut answer);
                let _ = tx.send(read.map(|_| answer));
            });
            let answer = rx.recv_timeout(PROMPT_WAIT + HELPER_MARGIN);
            if answer.is_err() {
                let _ = child.kill();
            }
            let _ = child.wait();
            match answer {
                Ok(Ok(mut data)) => {
                    let answer = decode_answer(&data);
                    super::wipe(&mut data);
                    answer
                }
                Ok(Err(error)) => Err(KeyError::Failed(format!(
                    "the Secret Service helper's answer cannot be read: {error}"
                ))),
                Err(_) => Err(KeyError::Failed(
                    "the Secret Service helper did not answer".into(),
                )),
            }
        }
    }

    impl HostKeyStore for SecretServiceKeys {
        fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
            if let Some(secret) = self.known().get(&purpose) {
                return Ok(Some(secret.clone()));
            }
            let secret = self.ask(&HelperRequest::Load(purpose))?;
            if let Some(secret) = &secret {
                self.known().insert(purpose, secret.clone());
            }
            Ok(secret)
        }

        fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
            // Whatever happens, what is known may no longer be what the keyring keeps.
            self.known().remove(&purpose);
            self.ask(&HelperRequest::Store(purpose, secret.clone()))?;
            Ok(())
        }

        fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
            self.known().remove(&purpose);
            self.ask(&HelperRequest::Remove(purpose))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Items by path: their attributes and their secret.
    type Items = HashMap<String, (Vec<(String, String)>, Vec<u8>)>;

    /// A Secret Service in memory: items by attributes, a keyring that may be locked, and the
    /// prompt's answer.
    #[derive(Default)]
    struct FakeBus {
        missing: bool,
        locked: RefCell<bool>,
        prompt_dismissed: bool,
        no_default: RefCell<bool>,
        items: RefCell<Items>,
        calls: RefCell<Vec<String>>,
        sessions_open: RefCell<i32>,
    }

    impl FakeBus {
        fn log(&self, call: &str) {
            self.calls.borrow_mut().push(call.to_string());
        }
        fn with_item(self, purpose: KeyPurpose, value: &[u8]) -> FakeBus {
            let attributes = attributes(purpose)
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            self.items.borrow_mut().insert(
                format!(
                    "/org/freedesktop/secrets/collection/login/{}",
                    purpose.name()
                ),
                (attributes, value.to_vec()),
            );
            self
        }
    }

    impl SecretBus for FakeBus {
        fn open_session(&self) -> Result<String, BusError> {
            self.log("OpenSession");
            if self.missing {
                return Err(BusError::NoService(
                    "org.freedesktop.DBus.Error.ServiceUnknown".into(),
                ));
            }
            *self.sessions_open.borrow_mut() += 1;
            Ok("/org/freedesktop/secrets/session/s1".into())
        }
        fn close_session(&self, _: &str) {
            self.log("Close");
            *self.sessions_open.borrow_mut() -= 1;
        }
        fn search_items(
            &self,
            wanted: &[(&str, &str)],
        ) -> Result<(Vec<String>, Vec<String>), BusError> {
            self.log("SearchItems");
            let found: Vec<String> = self
                .items
                .borrow()
                .iter()
                .filter(|(_, (attributes, _))| {
                    wanted
                        .iter()
                        .all(|(k, v)| attributes.iter().any(|(a, b)| a == k && b == v))
                })
                .map(|(path, _)| path.clone())
                .collect();
            Ok(if *self.locked.borrow() {
                (Vec::new(), found)
            } else {
                (found, Vec::new())
            })
        }
        fn unlock(&self, objects: &[String]) -> Result<(Vec<String>, String), BusError> {
            self.log("Unlock");
            Ok(if *self.locked.borrow() {
                (Vec::new(), "/org/freedesktop/secrets/prompt/p1".into())
            } else {
                (objects.to_vec(), NO_OBJECT.into())
            })
        }
        fn prompt(&self, prompt: &str) -> Result<PromptOutcome, BusError> {
            self.log("Prompt");
            if self.prompt_dismissed {
                return Ok(PromptOutcome::Dismissed);
            }
            if prompt.ends_with("make") {
                *self.no_default.borrow_mut() = false;
                return Ok(PromptOutcome::Completed(vec![
                    "/org/freedesktop/secrets/collection/made".into(),
                ]));
            }
            *self.locked.borrow_mut() = false;
            let unlocked = self.items.borrow().keys().cloned().collect::<Vec<_>>();
            Ok(PromptOutcome::Completed(
                unlocked
                    .into_iter()
                    .chain(["/org/freedesktop/secrets/aliases/default".to_string()])
                    .chain(["/org/freedesktop/secrets/collection/made".to_string()])
                    .collect(),
            ))
        }
        fn get_secret(&self, item: &str, session: &str) -> Result<WireSecret, BusError> {
            self.log("GetSecret");
            assert!(!*self.locked.borrow(), "a locked item gives no secret");
            let value = self.items.borrow()[item].1.clone();
            Ok(WireSecret {
                session: session.into(),
                parameters: Vec::new(),
                value,
                content_type: "application/octet-stream".into(),
            })
        }
        fn read_alias(&self, alias: &str) -> Result<String, BusError> {
            self.log("ReadAlias");
            assert_eq!(alias, DEFAULT_ALIAS);
            Ok(if *self.no_default.borrow() {
                NO_OBJECT.into()
            } else {
                "/org/freedesktop/secrets/aliases/default".into()
            })
        }
        fn create_collection(
            &self,
            label: &str,
            alias: &str,
        ) -> Result<(String, String), BusError> {
            self.log("CreateCollection");
            assert_eq!((label, alias), (DEFAULT_COLLECTION_LABEL, DEFAULT_ALIAS));
            Ok((
                NO_OBJECT.into(),
                "/org/freedesktop/secrets/prompt/make".into(),
            ))
        }
        fn create_item(
            &self,
            collection: &str,
            label: &str,
            wanted: &[(&str, &str)],
            secret: &WireSecret,
            replace: bool,
        ) -> Result<(String, String), BusError> {
            self.log("CreateItem");
            assert!(
                replace,
                "an item of the same key is replaced, never doubled"
            );
            assert!(label.contains("Desktop Subsystem for Android"));
            assert_eq!(secret.session, "/org/freedesktop/secrets/session/s1");
            let path = format!("{collection}/{}", wanted[1].1);
            let attributes = wanted
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            self.items
                .borrow_mut()
                .insert(path.clone(), (attributes, secret.value.clone()));
            Ok((path, NO_OBJECT.into()))
        }
        fn delete_item(&self, item: &str) -> Result<String, BusError> {
            self.log("Delete");
            assert!(
                !*self.locked.borrow(),
                "a locked item is unlocked before it is removed"
            );
            self.items.borrow_mut().remove(item);
            Ok(NO_OBJECT.into())
        }
    }

    #[test]
    fn fr36_a_key_is_removed_from_the_keyring_even_while_it_is_locked() {
        let bus = FakeBus::default()
            .with_item(KeyPurpose::Backup, &[3; 32])
            .with_item(KeyPurpose::KeyMintRoot, &[4; 32]);
        *bus.locked.borrow_mut() = true;
        remove(&bus, KeyPurpose::Backup).unwrap();
        assert_eq!(load(&bus, KeyPurpose::Backup), Ok(None));
        assert_eq!(
            load(&bus, KeyPurpose::KeyMintRoot),
            Ok(Some(Secret::from_bytes([4; 32]))),
            "only the purpose asked for goes"
        );
        remove(&bus, KeyPurpose::Backup).unwrap();
        let request = HelperRequest::Remove(KeyPurpose::Backup);
        assert_eq!(decode_request(&encode_request(&request)), Ok(request));
    }

    #[test]
    fn fr36_a_key_kept_in_the_secret_service_reads_back() {
        let bus = FakeBus::default();
        assert_eq!(load(&bus, KeyPurpose::Backup), Ok(None));
        let secret = Secret::from_bytes([7; 32]);
        store(&bus, KeyPurpose::Backup, &secret).unwrap();
        assert_eq!(load(&bus, KeyPurpose::Backup), Ok(Some(secret.clone())));
        assert_eq!(
            load(&bus, KeyPurpose::KeyMintRoot),
            Ok(None),
            "each purpose has an item of its own"
        );
        // A second store replaces the item rather than adding one.
        store(&bus, KeyPurpose::Backup, &Secret::from_bytes([8; 32])).unwrap();
        assert_eq!(bus.items.borrow().len(), 1);
        assert_eq!(*bus.sessions_open.borrow(), 0, "every session is closed");
        let (_, kept) = bus.items.borrow().values().next().cloned().unwrap();
        assert_eq!(kept, vec![8; 32]);
    }

    #[test]
    fn fr36_items_are_found_by_application_and_purpose() {
        assert_eq!(
            attributes(KeyPurpose::KeyMintRoot),
            [
                ("application", "io.github.thisisthepy.DSA"),
                ("purpose", "keymint-root-key")
            ]
        );
        assert_eq!(purpose_named("backup-key"), Some(KeyPurpose::Backup));
        assert_eq!(
            purpose_named("keymint-root-key"),
            Some(KeyPurpose::KeyMintRoot)
        );
        assert_eq!(purpose_named("ssh-key"), None);
    }

    #[test]
    fn fr36_a_locked_keyring_is_unlocked_through_its_prompt() {
        let bus = FakeBus::default().with_item(KeyPurpose::KeyMintRoot, &[3; 32]);
        *bus.locked.borrow_mut() = true;
        assert_eq!(
            load(&bus, KeyPurpose::KeyMintRoot),
            Ok(Some(Secret::from_bytes([3; 32])))
        );
        let calls = bus.calls.borrow();
        let unlock = calls.iter().position(|c| c == "Unlock").unwrap();
        let prompt = calls.iter().position(|c| c == "Prompt").unwrap();
        let get = calls.iter().position(|c| c == "GetSecret").unwrap();
        assert!(unlock < prompt && prompt < get);
    }

    #[test]
    fn fr36_a_keyring_left_locked_makes_the_key_unavailable() {
        let bus = FakeBus {
            prompt_dismissed: true,
            ..FakeBus::default()
        }
        .with_item(KeyPurpose::Backup, &[3; 32]);
        *bus.locked.borrow_mut() = true;
        match load(&bus, KeyPurpose::Backup) {
            Err(KeyError::Unavailable(why)) => assert!(why.contains("locked"), "{why}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
        assert!(matches!(
            store(&bus, KeyPurpose::Backup, &Secret::from_bytes([1; 32])),
            Err(KeyError::Unavailable(_))
        ));
        assert!(!bus.calls.borrow().iter().any(|c| c == "GetSecret"));
        assert!(!bus.calls.borrow().iter().any(|c| c == "CreateItem"));
    }

    #[test]
    fn fr36_no_secret_service_makes_the_key_unavailable_with_the_reason() {
        let bus = FakeBus {
            missing: true,
            ..FakeBus::default()
        };
        match load(&bus, KeyPurpose::Backup) {
            Err(KeyError::Unavailable(why)) => {
                assert!(why.contains("no Secret Service"), "{why}");
                assert!(why.contains("GNOME Keyring"), "{why}");
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
        assert!(is_missing_service(
            "org.freedesktop.DBus.Error.ServiceUnknown: The name org.freedesktop.secrets was \
             not provided by any .service files"
        ));
        assert!(!is_missing_service(
            "org.freedesktop.DBus.Error.AccessDenied"
        ));
    }

    #[test]
    fn fr36_a_desktop_without_a_default_keyring_has_one_made() {
        let bus = FakeBus::default();
        *bus.no_default.borrow_mut() = true;
        store(&bus, KeyPurpose::Backup, &Secret::from_bytes([5; 32])).unwrap();
        assert!(bus.calls.borrow().iter().any(|c| c == "CreateCollection"));
        let items = bus.items.borrow();
        assert!(items
            .keys()
            .any(|path| path.starts_with("/org/freedesktop/secrets/collection/made")));
    }

    #[test]
    fn fr36_a_key_of_the_wrong_length_is_unreadable_and_never_replaced() {
        let bus = FakeBus::default().with_item(KeyPurpose::Backup, b"a password someone typed");
        // Unreadable, not "none": a caller that makes a key when none is kept never replaces
        // this one.
        assert!(matches!(
            load(&bus, KeyPurpose::Backup),
            Err(KeyError::Unreadable(_))
        ));
        assert!(!bus.calls.borrow().iter().any(|c| c == "CreateItem"));
    }

    #[test]
    fn fr36_the_helpers_requests_and_answers_round_trip() {
        let secret = Secret::from_bytes([0xa5; 32]);
        for request in [
            HelperRequest::Load(KeyPurpose::Backup),
            HelperRequest::Store(KeyPurpose::KeyMintRoot, secret.clone()),
        ] {
            assert_eq!(decode_request(&encode_request(&request)), Ok(request));
        }
        for answer in [
            Ok(Some(secret.clone())),
            Ok(None),
            Err(KeyError::Unavailable("locked\nfor good".into())),
            Err(KeyError::Unreadable("other user".into())),
            Err(KeyError::Failed("bus".into())),
        ] {
            let decoded = decode_answer(&encode_answer(&answer));
            match (&answer, &decoded) {
                (Err(KeyError::Unavailable(_)), Err(KeyError::Unavailable(why))) => {
                    assert_eq!(why, "locked for good", "a reason stays on one line")
                }
                _ => assert_eq!(decoded, answer),
            }
        }
        assert!(decode_request(b"store backup-key 00\n").is_err());
        assert!(decode_request(b"load ssh-key\n").is_err());
        assert!(decode_request(b"load backup-key extra\n").is_err());
        assert!(decode_answer(b"secret 0011\n").is_err());
        assert!(
            decode_answer(b"none").is_err(),
            "an answer cut short is refused"
        );
    }

    #[test]
    fn fr36_the_helper_reaches_the_users_own_bus() {
        let (address, socket) = user_bus(1000);
        assert_eq!(address, "unix:path=/run/user/1000/bus");
        assert_eq!(socket, std::path::PathBuf::from("/run/user/1000/bus"));
    }
}

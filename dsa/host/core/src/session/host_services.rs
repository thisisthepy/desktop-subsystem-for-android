//! The host's system services a session serves its guest: app data backups
//! ([`crate::backup`]), passkeys ([`crate::passkeys`]), the KeyMint root key
//! ([`crate::keymint_root`]) and the root certificates added on the host
//! ([`crate::host_certificates`]).
//!
//! Each is given to the session by the platform, which supplies what names a host API: the
//! credential store, the HTTPS fetch, the desktop session that runs the passkey prompt.
//! Without one, the guest's requests for it are answered as unavailable, and nothing else
//! changes.

use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_protocol::control::{self, envelope::Body, BackupStatus, PasskeyStatus};
use dsa_transport::{Listener, TransportError};

use super::{send_quietly, Command, Session, SessionError, SessionState, Shared};
use crate::apk::InstallPart;
use crate::backup::{
    refusal, BackupReport, BackupService, BACKUP_OFF, BACKUP_RUN_WAIT, PACKAGE_MANAGER,
    RESTORE_WAIT,
};
use crate::host_certificates::{self, FoundCertificate, HostRootStore, ImportedCertificate};
use crate::host_keys::{HostKeyStore, KeyProtection, KeyPurpose};
use crate::keymint_root::{self, KEYMINT_WAIT};
use crate::passkeys::{CeremonyResult, PasskeyEvent, PasskeyService};

/// How long a restore waits for Android to say whether it has an app.
const PACKAGE_WAIT: Duration = Duration::from_secs(10);

/// What the platform gave the session, and the root key's state in the running boot.
#[derive(Default)]
pub(super) struct HostServices {
    backup: Option<Arc<BackupService>>,
    passkeys: Option<Arc<PasskeyService>>,
    keymint_keys: Option<Arc<dyn HostKeyStore>>,
    /// Whether the running boot has had its root key.
    keymint_given: bool,
    /// Why the latest boot that asked got no root key; `None` when it got one.
    keymint_problem: Option<String>,
    /// The root certificates added on the host, as the platform's stores last held them.
    host_certificates: Vec<FoundCertificate>,
    /// Whether the user turned off Android's trust in them. On unless they did.
    host_certificates_off: bool,
}

/// How the KeyMint root key is protected, and why the latest start of Android got none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyMintKeyState {
    /// `None` while no key is kept.
    pub protection: Option<KeyProtection>,
    /// Why the latest start of Android that asked got no key; empty when it got one.
    pub problem: String,
}

fn reply_of(shared: &Arc<Shared>, commands: &Sender<Command>) -> Arc<dyn Fn(Body) + Send + Sync> {
    let (shared, commands) = (Arc::clone(shared), commands.clone());
    let commands = std::sync::Mutex::new(commands);
    Arc::new(move |body: Body| {
        let commands = commands.lock().unwrap_or_else(|e| e.into_inner()).clone();
        send_quietly(&shared, &commands, body)
    })
}

impl Shared {
    fn services(&self) -> std::sync::MutexGuard<'_, HostServices> {
        self.host_services.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What the agent is told of backup: on only when the user turned it on. A host that keeps
    /// no backups tells it off, so Android's backup manager never backs up to it.
    pub(super) fn backup_setting_body(&self) -> Body {
        let backup = self.services().backup.clone();
        Body::BackupSetting(control::BackupSetting {
            enabled: backup.is_some_and(|backup| backup.enabled()),
        })
    }

    /// What the guest's control daemon is told of the root certificates added on the host: each
    /// of them while the user lets Android trust them, and none (so it takes away those it
    /// added) while they do not.
    pub(super) fn host_certificates_body(&self) -> Body {
        let services = self.services();
        let enabled = !services.host_certificates_off;
        Body::HostCertificates(control::HostCertificates {
            enabled,
            certificates: if enabled {
                services
                    .host_certificates
                    .iter()
                    .map(|certificate| certificate.der.clone())
                    .collect()
            } else {
                Vec::new()
            },
        })
    }

    /// Tells a running guest the root certificates added on the host now. A guest that cannot
    /// hear it now is told when its daemon or its agent connects, or when it is resumed.
    fn send_host_certificates(&self) {
        let frame = dsa_protocol::encode(&control::Envelope {
            body: Some(self.host_certificates_body()),
        });
        if let Some(Err(error)) = self.send_direct(&frame) {
            eprintln!("host certificates: the guest was not told: {error}");
        }
    }

    /// Android stopped: the backup and the passkeys let go of what is under way, and the next
    /// boot may have its root key.
    pub(super) fn host_services_gone(&self) {
        let (backup, passkeys) = {
            let mut services = self.services();
            services.keymint_given = false;
            (services.backup.clone(), services.passkeys.clone())
        };
        if let Some(backup) = backup {
            backup.guest_gone();
        }
        if let Some(passkeys) = passkeys {
            passkeys.guest_gone();
        }
    }

    /// Takes a message for one of the services; `false` for a message none of them is
    /// for.
    pub(super) fn host_services_message(&self, body: Body, reply: impl Fn(Body)) -> bool {
        match body {
            body @ (Body::BackupUpload(_)
            | Body::BackupCommit(_)
            | Body::BackupDiscard(_)
            | Body::BackupClear(_)
            | Body::BackupQuery(_)
            | Body::BackupFetch(_)
            | Body::BackupRunResult(_)) => {
                let backup = self.services().backup.clone();
                match backup {
                    Some(backup) => backup.guest_message(body),
                    None => {
                        no_backup(body, &reply);
                        true
                    }
                }
            }
            Body::PasskeyRequest(request) => {
                let passkeys = self.services().passkeys.clone();
                match passkeys {
                    Some(passkeys) => passkeys.request(request),
                    None => reply(Body::PasskeyResponse(control::PasskeyResponse {
                        request_id: request.request_id,
                        status: PasskeyStatus::Unavailable as i32,
                        response_json: String::new(),
                        message: "this computer has no passkeys DSA can use".into(),
                    })),
                }
                true
            }
            _ => false,
        }
    }
}

/// Answers a backup request when the host keeps no backups.
fn no_backup(body: Body, reply: &dyn Fn(Body)) {
    if let Some(answer) = refusal(&body, "this computer keeps no backups of Android's apps") {
        reply(answer);
    }
}

/// The apps a restore installed and the result of the agent's restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub installed: u32,
    pub install_failures: Vec<String>,
    pub result: control::BackupRunResult,
}

impl Session {
    /// Keeps the user's app data backups with `service`: the agent's backup transport stores
    /// in it and restores from it. Without it, the transport is told the host keeps none.
    pub fn serve_backup(&self, service: Arc<BackupService>) {
        service.attach(reply_of(&self.shared, &self.commands));
        self.shared.services().backup = Some(service);
    }

    /// Checks and serves the passkey requests of the guest's apps with `service`. Without it,
    /// every request is answered as unavailable.
    pub fn serve_passkeys(&self, service: Arc<PasskeyService>) {
        service.attach(reply_of(&self.shared, &self.commands));
        self.shared.services().passkeys = Some(service);
    }

    /// Has Android trust, as system certificates, the root certificates a user or an
    /// administrator added to `store`, the platform's trust stores. The store is read now and
    /// followed from then on: each change reaches a running guest at once.
    pub fn serve_host_certificates(&self, store: Arc<dyn HostRootStore>) {
        host_certificates::watch(
            store,
            Arc::downgrade(&self.shared),
            |shared: &Shared, found| {
                let changed = {
                    let mut services = shared.services();
                    let changed = services.host_certificates != found;
                    services.host_certificates = found;
                    changed && !services.host_certificates_off
                };
                if changed {
                    shared.send_host_certificates();
                }
            },
        );
    }

    /// Lets Android trust the root certificates added on the host (`on`, the default), or
    /// takes them away from it, as the user's setting says. A running guest follows at once.
    pub fn trust_host_certificates(&self, on: bool) {
        let changed = {
            let mut services = self.shared.services();
            let changed = services.host_certificates_off == on;
            services.host_certificates_off = !on;
            changed
        };
        if changed {
            eprintln!(
                "host certificates: Android {} the root certificates added on this computer",
                if on { "trusts" } else { "no longer trusts" }
            );
            send_quietly(
                &self.shared,
                &self.commands,
                self.shared.host_certificates_body(),
            );
        }
    }

    /// The root certificates added on the host that the platform found, whether or not the
    /// user lets Android trust them.
    pub fn host_certificates(&self) -> Vec<ImportedCertificate> {
        self.shared
            .services()
            .host_certificates
            .iter()
            .map(host_certificates::imported)
            .collect()
    }

    /// Keeps the root key of the guest's KeyMint in `store`, made there on first use.
    pub fn set_keymint_keys(&self, store: Arc<dyn HostKeyStore>) {
        self.shared.services().keymint_keys = Some(store);
    }

    /// The user's backup, if the platform gave the session one.
    pub fn backup(&self) -> Option<Arc<BackupService>> {
        self.shared.services().backup.clone()
    }

    /// Turns the user's backup on or off, and tells the running agent at once, so Android's
    /// backup manager starts or stops backing up to the host. Off with `delete_backups`, the
    /// backups made so far and their key are removed as well.
    pub fn set_backup(&self, on: bool, delete_backups: bool) -> Result<BackupReport, SessionError> {
        let backup = self.backup().ok_or_else(|| {
            SessionError::NoAnswer("this computer keeps no backups of Android's apps".into())
        })?;
        // The agent stops first, so nothing is sent while the folder goes away.
        if !on {
            send_quietly(
                &self.shared,
                &self.commands,
                Body::BackupSetting(control::BackupSetting { enabled: false }),
            );
        }
        let report = backup
            .set_enabled(on, delete_backups)
            .map_err(SessionError::NoAnswer)?;
        send_quietly(
            &self.shared,
            &self.commands,
            self.shared.backup_setting_body(),
        );
        Ok(report)
    }

    /// How the root key of the guest's KeyMint is protected, and why the latest start of
    /// Android got none. `None` when the session keeps no KeyMint root key.
    pub fn keymint_key_state(&self) -> Option<KeyMintKeyState> {
        let (store, problem) = {
            let services = self.shared.services();
            (
                services.keymint_keys.clone()?,
                services.keymint_problem.clone(),
            )
        };
        let (protection, read_problem) = match store.protection(KeyPurpose::KeyMintRoot) {
            Ok(protection) => (protection, None),
            Err(why) => (None, Some(why.to_string())),
        };
        Some(KeyMintKeyState {
            protection,
            problem: problem.or(read_problem).unwrap_or_default(),
        })
    }

    /// Lets go of the KeyMint root key after the user's Android data was reset: the fresh
    /// data holds no key wrapped under it, so the next start makes a new one. It is how a key
    /// that can no longer be opened (its TPM was cleared) is left behind.
    pub fn forget_keymint_root_key(&self) {
        let store = self.shared.services().keymint_keys.clone();
        if let Some(store) = store {
            match store.remove(KeyPurpose::KeyMintRoot) {
                Ok(()) => {
                    self.shared.services().keymint_problem = None;
                    eprintln!(
                        "keymint: the data was reset, so the next start makes a new root key"
                    );
                }
                Err(why) => eprintln!("keymint: the old root key was not removed: {why}"),
            }
        }
    }

    /// The passkey ceremonies from now on, for the user's desktop session to perform. `None`
    /// when the session serves no passkeys.
    pub fn watch_passkeys(&self) -> Option<Receiver<PasskeyEvent>> {
        let passkeys = self.shared.services().passkeys.clone()?;
        Some(passkeys.watch())
    }

    /// What became of a passkey ceremony a watcher was given.
    pub fn passkey_result(&self, id: u64, result: CeremonyResult) -> Result<(), String> {
        let passkeys = self
            .shared
            .services()
            .passkeys
            .clone()
            .ok_or_else(|| "this Android serves no passkeys".to_string())?;
        passkeys.report(id, result)
    }

    /// The backup, while Android runs and the user has turned backup on.
    fn running_backup(&self) -> Result<Arc<BackupService>, SessionError> {
        let backup = self.backup().ok_or_else(|| {
            SessionError::NoAnswer("this computer keeps no backups of Android's apps".into())
        })?;
        if !backup.enabled() {
            return Err(SessionError::NoAnswer(BACKUP_OFF.into()));
        }
        let state = self.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            return Err(SessionError::NotActive(state));
        }
        Ok(backup)
    }

    /// Has the agent back up every app now, and waits for Android's backup manager to finish.
    pub fn backup_now(&self) -> Result<control::BackupRunResult, SessionError> {
        let backup = self.running_backup()?;
        if let Some(why) = Some(backup.report())
            .filter(|r| !r.available)
            .map(|r| r.problem)
        {
            return Err(SessionError::NoAnswer(why));
        }
        let result = backup
            .run_in_guest(
                |request_id| Body::BackupRun(control::BackupRun { request_id }),
                |body| {
                    self.send(dsa_protocol::encode(&control::Envelope {
                        body: Some(body),
                    }))
                    .map_err(|e| e.to_string())
                },
                BACKUP_RUN_WAIT,
            )
            .map_err(SessionError::NoAnswer)?;
        backup.ran(if result.status == BackupStatus::Ok as i32 {
            ""
        } else {
            &result.message
        });
        eprintln!(
            "backup: Android backed up {} apps; {} failed{}",
            result.succeeded,
            result.failed,
            if result.message.is_empty() {
                String::new()
            } else {
                format!(" ({})", result.message)
            }
        );
        Ok(result)
    }

    /// Restores the user's apps from their backup: installs each app the backup holds APKs of
    /// that Android does not have, then has the agent restore every app's data.
    pub fn restore_backup(&self) -> Result<RestoreOutcome, SessionError> {
        let backup = self.running_backup()?;
        let (token, packages) = backup.contents().map_err(|e| SessionError::NoAnswer(e.0))?;
        let mut installed = 0;
        let mut install_failures = Vec::new();
        for package in &packages {
            if package.apks.is_empty() || package.package == PACKAGE_MANAGER {
                continue;
            }
            match self.package_state(&package.package, PACKAGE_WAIT) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(why) => {
                    install_failures.push(format!("{}: {why}", package.package));
                    continue;
                }
            }
            let names: Vec<String> = package.apks.iter().map(|(name, _)| name.clone()).collect();
            let parts: Vec<InstallPart> = package
                .apks
                .iter()
                .map(|(name, size)| InstallPart {
                    name: name.clone(),
                    size: *size,
                    obb: false,
                })
                .collect();
            let mut content: Box<dyn Read + Send> = match backup.apks(&package.package, &names) {
                Ok(content) => content,
                Err(why) => {
                    install_failures.push(format!("{}: {why}", package.package));
                    continue;
                }
            };
            let name = format!("{} from the backup", package.package);
            match self.install_parts(&name, &parts, None, &mut content) {
                Ok(outcome) if outcome.installed() => installed += 1,
                Ok(outcome) => install_failures.push(format!("{}: {outcome}", package.package)),
                Err(why) => install_failures.push(format!("{}: {why}", package.package)),
            }
        }
        let mut wanted: Vec<String> = packages.iter().map(|p| p.package.clone()).collect();
        // The package manager's metadata comes first: Android checks each app against it.
        wanted.sort_by_key(|p| p != PACKAGE_MANAGER);
        let result = backup
            .run_in_guest(
                |request_id| {
                    Body::BackupRestore(control::BackupRestore {
                        request_id,
                        token,
                        packages: wanted,
                    })
                },
                |body| {
                    self.send(dsa_protocol::encode(&control::Envelope {
                        body: Some(body),
                    }))
                    .map_err(|e| e.to_string())
                },
                RESTORE_WAIT,
            )
            .map_err(SessionError::NoAnswer)?;
        let mut problems = install_failures.clone();
        if result.status != BackupStatus::Ok as i32 && !result.message.is_empty() {
            problems.push(result.message.clone());
        }
        backup.restored(&problems.join("; "));
        eprintln!(
            "backup: restored {} apps' data, {} failed; installed {installed} apps{}",
            result.succeeded,
            result.failed,
            if install_failures.is_empty() {
                String::new()
            } else {
                format!(", {} could not be installed", install_failures.len())
            }
        );
        Ok(RestoreOutcome {
            installed,
            install_failures,
            result,
        })
    }

    /// Where the user's backup stands, if the session keeps one.
    pub fn backup_report(&self) -> Option<BackupReport> {
        self.backup().map(|backup| backup.report())
    }

    /// Serves the KeyMint port on `listener`: each boot's control daemon gets the root key
    /// there once, after the handshake. `build` names this host in the handshake.
    pub fn serve_keymint_port(&self, listener: Box<dyn Listener>, build: &str) {
        let shared = Arc::clone(&self.shared);
        let hello = dsa_protocol::handshake::local_hello(control::Role::Host, build);
        let spawned = thread::Builder::new()
            .name("dsa-keymint-key".into())
            .spawn(move || accept_keymint(listener, shared, hello));
        if let Err(error) = spawned {
            eprintln!("keymint: the root key port cannot be served: {error}");
        }
    }
}

fn accept_keymint(mut listener: Box<dyn Listener>, shared: Arc<Shared>, hello: control::Hello) {
    let mut reported = false;
    while !shared.log_port_stop.load(Ordering::Relaxed) {
        let mut connection = match listener.accept(Duration::from_millis(200)) {
            Ok(connection) => connection,
            Err(TransportError::Timeout) => continue,
            Err(error) => {
                if !reported {
                    eprintln!("keymint: accepting on the root key port failed: {error}");
                    reported = true;
                }
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        reported = false;
        let secret = shared
            .boot_secret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(secret) = secret else {
            connection.close();
            continue;
        };
        let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(error) = dsa_protocol::handshake::host_accept(
                connection.as_mut(),
                &hello,
                &secret,
                KEYMINT_WAIT,
            ) {
                eprintln!("keymint: refused a connection on the root key port: {error}");
                return;
            }
            let store = {
                let mut services = shared.services();
                if services.keymint_given {
                    eprintln!(
                        "keymint: refused a second request for the root key in the same boot"
                    );
                    return;
                }
                services.keymint_given = true;
                services.keymint_keys.clone()
            };
            let key = match &store {
                Some(store) => keymint_root::root_key(store),
                None => Err(
                    "this computer keeps no key for Android's KeyMint; its keys stay \
                             under KeyMint's built-in root key"
                        .to_string(),
                ),
            };
            match &key {
                Ok(_) => eprintln!("keymint: gave this boot its root key"),
                Err(why) => eprintln!("keymint: gave this boot no root key: {why}"),
            }
            shared.services().keymint_problem = key.as_ref().err().cloned();
            if let Err(why) = keymint_root::serve_root_key(connection.as_mut(), &key) {
                eprintln!("keymint: the root key request failed: {why}");
                // It may ask again on a connection of its own.
                shared.services().keymint_given = false;
            }
        }));
        if served.is_err() {
            eprintln!("keymint: a root key request failed; the guest goes on");
        }
        connection.close();
    }
}

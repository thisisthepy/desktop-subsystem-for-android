//! The local API of the host's system services: app data backups and the passkey ceremonies
//! of the caller's Android.
//!
//! Backing up, restoring and the recovery key are plain requests, which the settings app and
//! `dsa backup` make. The passkey ceremonies come on a connection of their own, watched by the
//! window manager in the user's desktop session, where the host's passkey prompt can show; the
//! service hands it only ceremonies it has checked ([`crate::passkeys`]).

use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use std::time::Duration;

use dsa_protocol::control::BackupStatus;
use dsa_protocol::host_api::types::{
    self, envelope::Body, request, response, Envelope, ErrorCode, Response,
};
use dsa_protocol::host_api::{decode, send};
use dsa_transport::local::Peer;
use dsa_transport::{Connection, TransportError};

use super::{error, ClientError, HostApiClient, HostApiService, UserAndroid, CLIPBOARD_POLL};
use crate::backup::{BackupReport, BackupService, BACKUP_RUN_WAIT, RESTORE_WAIT};
use crate::host_keys::KeyProtection;
use crate::passkeys::{Ceremony, CeremonyResult, PasskeyEvent};
use crate::session::KeyMintKeyState;
use crate::tasks::TaskRequestError;

/// What a backup or a restore did, and where the backup stands after it.
pub type BackupOutcome = BackupReport;

/// How long a client waits for a backup or a restore: as long as the service waits for
/// Android, and a little more.
const RUN_ANSWER_WAIT: Duration = Duration::from_secs(
    if BACKUP_RUN_WAIT.as_secs() > RESTORE_WAIT.as_secs() {
        BACKUP_RUN_WAIT.as_secs()
    } else {
        RESTORE_WAIT.as_secs()
    } + 120,
);

fn report_to_wire(report: &BackupReport) -> types::BackupReport {
    types::BackupReport {
        available: report.available,
        problem: report.problem.clone(),
        folder: report.folder.display().to_string(),
        last_backup_unix_ms: report.last_backup_ms,
        packages: report.packages,
        size: report.size,
        last_error: report.last_error.clone(),
        succeeded: report.succeeded,
        failed: report.failed,
        installed: report.installed,
        last_restore_unix_ms: report.last_restore_ms,
        enabled: report.enabled,
    }
}

fn protection_to_wire(state: &KeyMintKeyState) -> types::KeyProtection {
    let protection = state.protection.clone().unwrap_or_default();
    types::KeyProtection {
        kept: state.protection.is_some(),
        sealed_by: protection.sealed_by,
        hardware: protection.hardware,
        description: protection.description,
        fallback_reason: protection.fallback_reason,
        problem: state.problem.clone(),
    }
}

fn protection_from_wire(wire: types::KeyProtection) -> KeyMintKeyState {
    KeyMintKeyState {
        protection: wire.kept.then_some(KeyProtection {
            sealed_by: wire.sealed_by,
            hardware: wire.hardware,
            description: wire.description,
            fallback_reason: wire.fallback_reason,
        }),
        problem: wire.problem,
    }
}

fn report_from_wire(report: types::BackupReport) -> BackupReport {
    BackupReport {
        available: report.available,
        problem: report.problem,
        folder: report.folder.into(),
        last_backup_ms: report.last_backup_unix_ms,
        packages: report.packages,
        size: report.size,
        last_error: report.last_error,
        last_restore_ms: report.last_restore_unix_ms,
        succeeded: report.succeeded,
        failed: report.failed,
        installed: report.installed,
        enabled: report.enabled,
    }
}

fn task_error(why: TaskRequestError) -> response::Body {
    match why {
        TaskRequestError::NotRunning(why) => error(ErrorCode::NotRunning, why),
        TaskRequestError::Failed(why) => error(ErrorCode::Unavailable, why),
    }
}

/// A ceremony as the local API carries it.
pub fn ceremony_to_wire(id: u64, ceremony: &Ceremony) -> types::PasskeyCeremony {
    types::PasskeyCeremony {
        ceremony_id: id,
        operation: if ceremony.create {
            types::PasskeyOperation::Create
        } else {
            types::PasskeyOperation::Get
        } as i32,
        rp_id: ceremony.rp_id.clone(),
        rp_name: ceremony.rp_name.clone(),
        user_id: ceremony.user_id.clone(),
        user_name: ceremony.user_name.clone(),
        user_display_name: ceremony.user_display_name.clone(),
        client_data_json: ceremony.client_data_json.clone(),
        client_data_hash: ceremony.client_data_hash.clone(),
        algorithms: ceremony.algorithms.clone(),
        credential_ids: ceremony.credential_ids.clone(),
        resident_key: ceremony.resident_key.clone(),
        user_verification: ceremony.user_verification.clone(),
        timeout_ms: ceremony.timeout_ms,
        app_label: ceremony.app_label.clone(),
        package: ceremony.package.clone(),
        origin: ceremony.origin.clone(),
        attestation: ceremony.attestation.clone(),
    }
}

/// A ceremony from the local API.
pub fn ceremony_from_wire(wire: types::PasskeyCeremony) -> (u64, Ceremony) {
    let create = wire.operation == types::PasskeyOperation::Create as i32;
    (
        wire.ceremony_id,
        Ceremony {
            create,
            rp_id: wire.rp_id,
            rp_name: wire.rp_name,
            user_id: wire.user_id,
            user_name: wire.user_name,
            user_display_name: wire.user_display_name,
            client_data_json: wire.client_data_json,
            client_data_hash: wire.client_data_hash,
            algorithms: wire.algorithms,
            credential_ids: wire.credential_ids,
            resident_key: wire.resident_key,
            user_verification: wire.user_verification,
            timeout_ms: wire.timeout_ms,
            app_label: wire.app_label,
            package: wire.package,
            origin: wire.origin,
            attestation: wire.attestation,
        },
    )
}

fn result_to_wire(id: u64, result: &CeremonyResult) -> types::ReportPasskeyCeremony {
    let mut report = types::ReportPasskeyCeremony {
        ceremony_id: id,
        ..Default::default()
    };
    match result {
        CeremonyResult::Created {
            credential_id,
            attestation_object,
            transports,
        } => {
            report.result = types::PasskeyResult::Ok as i32;
            report.credential_id = credential_id.clone();
            report.attestation_object = attestation_object.clone();
            report.transports = transports.clone();
        }
        CeremonyResult::Asserted {
            credential_id,
            authenticator_data,
            signature,
            user_handle,
        } => {
            report.result = types::PasskeyResult::Ok as i32;
            report.credential_id = credential_id.clone();
            report.authenticator_data = authenticator_data.clone();
            report.signature = signature.clone();
            report.user_handle = user_handle.clone();
        }
        CeremonyResult::Canceled(why) => {
            report.result = types::PasskeyResult::Canceled as i32;
            report.message = why.clone();
        }
        CeremonyResult::Unavailable(why) => {
            report.result = types::PasskeyResult::Unavailable as i32;
            report.message = why.clone();
        }
        CeremonyResult::NotAllowed(why) => {
            report.result = types::PasskeyResult::NotAllowed as i32;
            report.message = why.clone();
        }
        CeremonyResult::Failed(why) => {
            report.result = types::PasskeyResult::Error as i32;
            report.message = why.clone();
        }
    }
    report
}

/// What a report says became of a ceremony of the kind `create`.
fn result_from_wire(report: types::ReportPasskeyCeremony, create: Option<bool>) -> CeremonyResult {
    match types::PasskeyResult::try_from(report.result) {
        Ok(types::PasskeyResult::Ok) => {
            let created = match create {
                Some(create) => create,
                None => !report.attestation_object.is_empty(),
            };
            if created {
                CeremonyResult::Created {
                    credential_id: report.credential_id,
                    attestation_object: report.attestation_object,
                    transports: report.transports,
                }
            } else {
                CeremonyResult::Asserted {
                    credential_id: report.credential_id,
                    authenticator_data: report.authenticator_data,
                    signature: report.signature,
                    user_handle: report.user_handle,
                }
            }
        }
        Ok(types::PasskeyResult::Canceled) => CeremonyResult::Canceled(report.message),
        Ok(types::PasskeyResult::Unavailable) => CeremonyResult::Unavailable(report.message),
        Ok(types::PasskeyResult::NotAllowed) => CeremonyResult::NotAllowed(report.message),
        _ => CeremonyResult::Failed(if report.message.is_empty() {
            "the host's authenticator failed".into()
        } else {
            report.message
        }),
    }
}

/// A caller's Android and its backup.
type UserBackup = (Arc<dyn UserAndroid>, Arc<BackupService>);

impl HostApiService {
    fn user_backup(&self, peer: &Peer) -> Result<UserBackup, Box<response::Body>> {
        let android = self
            .androids
            .android(&peer.user)
            .map_err(|why| Box::new(error(ErrorCode::Unavailable, why)))?;
        let backup = android.backup().ok_or_else(|| {
            Box::new(error(
                ErrorCode::Unavailable,
                "this computer keeps no backups of Android's apps; update DSA".into(),
            ))
        })?;
        Ok((android, backup))
    }

    pub(super) fn get_backup(&self, peer: &Peer) -> response::Body {
        match self.user_backup(peer) {
            Ok((_, backup)) => response::Body::BackupReport(report_to_wire(&backup.report())),
            Err(body) => *body,
        }
    }

    pub(super) fn backup_now(&self, peer: &Peer) -> response::Body {
        let (android, backup) = match self.user_backup(peer) {
            Ok(found) => found,
            Err(body) => return *body,
        };
        eprintln!("local API: {} backs up their Android's apps", peer.user);
        match android.backup_now() {
            Ok(result) => {
                let mut report = backup.report();
                report.succeeded = result.succeeded;
                report.failed = result.failed;
                if result.status != BackupStatus::Ok as i32 && !result.message.is_empty() {
                    report.last_error = result.message;
                }
                response::Body::BackupReport(report_to_wire(&report))
            }
            Err(why) => task_error(why),
        }
    }

    pub(super) fn restore_backup(&self, peer: &Peer) -> response::Body {
        let (android, backup) = match self.user_backup(peer) {
            Ok(found) => found,
            Err(body) => return *body,
        };
        eprintln!(
            "local API: {} restores their Android's apps from the backup",
            peer.user
        );
        match android.restore_backup() {
            Ok(outcome) => {
                let mut report = backup.report();
                report.succeeded = outcome.result.succeeded;
                report.failed = outcome.result.failed + outcome.install_failures.len() as u32;
                report.installed = outcome.installed;
                response::Body::BackupReport(report_to_wire(&report))
            }
            Err(why) => task_error(why),
        }
    }

    pub(super) fn backup_recovery_key(&self, peer: &Peer) -> response::Body {
        match self.user_backup(peer) {
            Ok((_, backup)) => match backup.recovery_key() {
                Ok(key) => response::Body::BackupRecoveryKey(types::BackupRecoveryKey { key }),
                Err(why) => error(ErrorCode::Unavailable, why.0),
            },
            Err(body) => *body,
        }
    }

    pub(super) fn set_backup(&self, peer: &Peer, set: &types::SetBackup) -> response::Body {
        let (android, _) = match self.user_backup(peer) {
            Ok(found) => found,
            Err(body) => return *body,
        };
        eprintln!(
            "local API: {} turns backup {}{}",
            peer.user,
            if set.enabled { "on" } else { "off" },
            if set.delete_backups {
                " and deletes the backups"
            } else {
                ""
            }
        );
        match android.set_backup(set.enabled, set.delete_backups) {
            Ok(report) => response::Body::BackupReport(report_to_wire(&report)),
            Err(why) => task_error(why),
        }
    }

    pub(super) fn key_protection(&self, peer: &Peer) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.keymint_key_state() {
            Some(state) => response::Body::KeyProtection(protection_to_wire(&state)),
            None => error(
                ErrorCode::Unavailable,
                "this Android's KeyMint takes no root key from the host".into(),
            ),
        }
    }

    pub(super) fn set_backup_recovery_key(&self, peer: &Peer, key: &str) -> response::Body {
        match self.user_backup(peer) {
            Ok((_, backup)) => match backup.use_recovery_key(key) {
                Ok(()) => response::Body::BackupReport(report_to_wire(&backup.report())),
                Err(why) => error(ErrorCode::InvalidSetting, why),
            },
            Err(body) => *body,
        }
    }

    /// Answers a WatchPasskeys request, then passes the checked passkey ceremonies of the
    /// client's Android to it until the client goes away. The client sends nothing more:
    /// anything it sends closes the connection. A ceremony that cannot be sent ends as
    /// unavailable, so the app does not wait for an answer that never comes.
    pub(super) fn watch_passkeys(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => match android.watch_passkeys() {
                Some(watch) => Ok((android, watch)),
                None => Err(error(
                    ErrorCode::Unavailable,
                    "this Android uses no passkeys of the host".into(),
                )),
            },
        };
        let (android, watch) = match watched {
            Ok(watched) => watched,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            loop {
                match watch.try_recv() {
                    Ok(PasskeyEvent::Ceremony { id, ceremony }) => {
                        let frame = Body::PasskeyCeremony(ceremony_to_wire(id, &ceremony));
                        if send(&mut *connection, frame).is_err() {
                            let _ = android.passkey_result(
                                id,
                                CeremonyResult::Unavailable(
                                    "the desktop session that was to ask went away".into(),
                                ),
                            );
                            return;
                        }
                    }
                    Ok(PasskeyEvent::Withdrawn { id }) => {
                        let frame =
                            Body::PasskeyCeremonyWithdrawn(types::PasskeyCeremonyWithdrawn {
                                ceremony_id: id,
                            });
                        if send(&mut *connection, frame).is_err() {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connection.close();
                        return;
                    }
                }
            }
            let frame = match connection.recv(Some(CLIPBOARD_POLL)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(_) => return,
            };
            if decode(&frame).is_ok() {
                eprintln!(
                    "local API: a client of {} sent something on a connection that watches \
                     passkey ceremonies; closing its connection",
                    peer.user
                );
                connection.close();
                return;
            }
        }
    }

    pub(super) fn report_passkey_ceremony(
        &self,
        peer: &Peer,
        report: types::ReportPasskeyCeremony,
    ) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        let id = report.ceremony_id;
        match android.passkey_result(id, result_from_wire(report, None)) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(why) => task_error(why),
        }
    }
}

impl HostApiClient {
    fn backup_request(
        &mut self,
        body: request::Body,
        wait: Duration,
    ) -> Result<BackupReport, ClientError> {
        match self.exchange(body, wait)? {
            response::Body::BackupReport(report) => Ok(report_from_wire(report)),
            _ => Err(ClientError::Malformed(
                "expected where the backup stands".into(),
            )),
        }
    }

    /// Where the caller's backup stands.
    pub fn backup(&mut self) -> Result<BackupReport, ClientError> {
        let wait = self.timeout;
        self.backup_request(request::Body::GetBackup(types::GetBackup {}), wait)
    }

    /// Backs up every app of the caller's Android now, and waits until Android has.
    pub fn backup_now(&mut self) -> Result<BackupOutcome, ClientError> {
        self.backup_request(
            request::Body::BackupNow(types::BackupNow {}),
            RUN_ANSWER_WAIT,
        )
    }

    /// Restores the caller's Android from their backup: the apps it lacks, then every app's
    /// data.
    pub fn restore_backup(&mut self) -> Result<BackupOutcome, ClientError> {
        self.backup_request(
            request::Body::RestoreBackup(types::RestoreBackup {}),
            RUN_ANSWER_WAIT,
        )
    }

    /// The caller's backup recovery key.
    pub fn backup_recovery_key(&mut self) -> Result<String, ClientError> {
        match self.exchange(
            request::Body::GetBackupRecoveryKey(types::GetBackupRecoveryKey {}),
            self.timeout,
        )? {
            response::Body::BackupRecoveryKey(key) => Ok(key.key),
            _ => Err(ClientError::Malformed("expected the recovery key".into())),
        }
    }

    /// Turns the caller's backup on or off; off with `delete_backups`, removes the backups
    /// made so far and their key.
    pub fn set_backup(
        &mut self,
        enabled: bool,
        delete_backups: bool,
    ) -> Result<BackupReport, ClientError> {
        let wait = self.timeout;
        self.backup_request(
            request::Body::SetBackup(types::SetBackup {
                enabled,
                delete_backups,
            }),
            wait,
        )
    }

    /// How the caller's KeyMint root key is protected on this computer.
    pub fn key_protection(&mut self) -> Result<KeyMintKeyState, ClientError> {
        match self.exchange(
            request::Body::GetKeyProtection(types::GetKeyProtection {}),
            self.timeout,
        )? {
            response::Body::KeyProtection(wire) => Ok(protection_from_wire(wire)),
            _ => Err(ClientError::Malformed(
                "expected how the KeyMint root key is protected".into(),
            )),
        }
    }

    /// Keeps the key a recovery key carries as the caller's backup key.
    pub fn set_backup_recovery_key(&mut self, key: &str) -> Result<BackupReport, ClientError> {
        let wait = self.timeout;
        self.backup_request(
            request::Body::SetBackupRecoveryKey(types::SetBackupRecoveryKey {
                key: key.to_string(),
            }),
            wait,
        )
    }

    /// Turns this connection into a watch of the caller's passkey ceremonies.
    pub fn watch_passkeys(mut self) -> Result<PasskeyCeremonyWatch, ClientError> {
        let wait = self.timeout;
        match self.exchange(request::Body::WatchPasskeys(types::WatchPasskeys {}), wait)? {
            response::Body::Done(_) => Ok(PasskeyCeremonyWatch {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that passkey ceremonies are watched".into(),
            )),
        }
    }

    /// Tells the caller's Android what became of the passkey ceremony `id`.
    pub fn report_passkey_ceremony(
        &mut self,
        id: u64,
        result: &CeremonyResult,
    ) -> Result<(), ClientError> {
        let wait = self.timeout;
        match self.exchange(
            request::Body::ReportPasskeyCeremony(result_to_wire(id, result)),
            wait,
        )? {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed(
                "expected the answer that the ceremony was reported".into(),
            )),
        }
    }
}

/// A connection that watches the caller's passkey ceremonies.
pub struct PasskeyCeremonyWatch {
    connection: Box<dyn Connection>,
}

impl PasskeyCeremonyWatch {
    /// The next ceremony or withdrawal, waiting at most `timeout`. `Ok(None)` means nothing
    /// came in time; an error means the connection has ended or the service sent something
    /// else.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<PasskeyEvent>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::PasskeyCeremony(ceremony)),
            }) => {
                let (id, ceremony) = ceremony_from_wire(ceremony);
                Ok(Some(PasskeyEvent::Ceremony {
                    id,
                    ceremony: Box::new(ceremony),
                }))
            }
            Ok(Envelope {
                body: Some(Body::PasskeyCeremonyWithdrawn(withdrawn)),
            }) => Ok(Some(PasskeyEvent::Withdrawn {
                id: withdrawn.ceremony_id,
            })),
            Ok(_) => Err(ClientError::Malformed("expected a passkey ceremony".into())),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

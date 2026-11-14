//! The host's system services for the guest: app data backups, passkeys and the KeyMint root
//! key, through the session's public API and the control channel, over the in-memory
//! transport, with fakes for the credential store, the HTTPS fetch and the desktop session.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::backup::crypto::{
    parse_recovery_key, recovery_key, BackupKey, DecryptingReader, EncryptingWriter, OpenError,
};
use dsa_host_core::backup::{records, BackupService, BackupSwitch, FileSwitch};
use dsa_host_core::data_reset::{reset_user_data, TemplateDataDisk};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_keys::{
    load_or_create, sealer_of, Hardware, HostKeyStore, KeyError, KeyPurpose, NoKeyStore,
    SealedFileStore, SealedKeys, Sealer, SealerSource, Secret,
};
use dsa_host_core::keymint_root::{ask_root_key, root_key};
use dsa_host_core::passkeys::asset_links::{fingerprint, HttpsGet};
use dsa_host_core::passkeys::encoding::{attested_credential, base64url, from_base64url};
use dsa_host_core::passkeys::{
    https_origin_host, valid_rp_id, CeremonyResult, PasskeyEvent, PasskeyService,
};
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{
    self, envelope::Body, BackupDataKind, BackupStatus, Envelope, PasskeyOperation, PasskeyStatus,
    Role,
};
use dsa_protocol::encode;
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector};
use serde_json::Value;
use sha2::{Digest, Sha256};

const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------------
// The fakes.

/// A guest that only records the secret each boot was given.
#[derive(Clone, Default)]
struct FakeLauncher(Arc<Mutex<Option<BootSecret>>>);

impl FakeLauncher {
    fn secret(&self) -> BootSecret {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(secret) = self.0.lock().unwrap().clone() {
                return secret;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the guest never started"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl GuestLauncher for FakeLauncher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

/// A credential store in memory.
#[derive(Default)]
struct MemoryKeys(Mutex<HashMap<KeyPurpose, Secret>>);

impl HostKeyStore for MemoryKeys {
    fn load(&self, purpose: KeyPurpose) -> Result<Option<Secret>, KeyError> {
        Ok(self.0.lock().unwrap().get(&purpose).cloned())
    }
    fn store(&self, purpose: KeyPurpose, secret: &Secret) -> Result<(), KeyError> {
        self.0.lock().unwrap().insert(purpose, secret.clone());
        Ok(())
    }
    fn remove(&self, purpose: KeyPurpose) -> Result<(), KeyError> {
        self.0.lock().unwrap().remove(&purpose);
        Ok(())
    }
}

/// A sealer bound to one user on one computer, as DPAPI and a TPM are: what it seals opens
/// only where it was sealed.
struct BoundSealer {
    computer: String,
    user: String,
    name: &'static str,
    hardware: bool,
}

impl BoundSealer {
    fn new(computer: &str, user: &str) -> Arc<BoundSealer> {
        Arc::new(BoundSealer {
            computer: computer.into(),
            user: user.into(),
            name: "test-bound",
            hardware: false,
        })
    }

    /// A sealer held by a TPM: `tpm` names the TPM, which a clear replaces.
    fn tpm(computer: &str, user: &str, tpm: &str) -> Arc<BoundSealer> {
        Arc::new(BoundSealer {
            computer: format!("{computer}/{tpm}"),
            user: user.into(),
            name: "test-tpm",
            hardware: true,
        })
    }

    fn pad(&self, label: &str, len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut counter = 0u32;
        while out.len() < len {
            let mut h = Sha256::new();
            h.update(self.computer.as_bytes());
            h.update([0]);
            h.update(self.user.as_bytes());
            h.update([0]);
            h.update(label.as_bytes());
            h.update(counter.to_be_bytes());
            out.extend_from_slice(&h.finalize());
            counter += 1;
        }
        out.truncate(len);
        out
    }

    fn tag(&self, label: &str, sealed: &[u8]) -> Vec<u8> {
        let mut h = Sha256::new();
        h.update(b"tag");
        h.update(self.pad(label, 32));
        h.update(sealed);
        h.finalize().to_vec()
    }
}

impl Sealer for BoundSealer {
    fn name(&self) -> &str {
        self.name
    }
    fn kind(&self) -> &str {
        if self.hardware {
            "tpm"
        } else {
            "dpapi"
        }
    }
    fn hardware(&self) -> bool {
        self.hardware
    }
    fn description(&self) -> String {
        if self.hardware {
            "the test computer's TPM".into()
        } else {
            "the test credential store".into()
        }
    }
    fn seal(&self, plain: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        let pad = self.pad(label, plain.len());
        let mut sealed: Vec<u8> = plain.iter().zip(pad).map(|(a, b)| a ^ b).collect();
        let tag = self.tag(label, &sealed);
        sealed.extend(tag);
        Ok(sealed)
    }
    fn unseal(&self, sealed: &[u8], label: &str) -> Result<Vec<u8>, KeyError> {
        if sealed.len() < 32 {
            return Err(KeyError::Unreadable("too short".into()));
        }
        let (body, tag) = sealed.split_at(sealed.len() - 32);
        if self.tag(label, body) != tag {
            return Err(KeyError::Unreadable(
                "it was sealed for another user or another computer".into(),
            ));
        }
        let pad = self.pad(label, body.len());
        Ok(body.iter().zip(pad).map(|(a, b)| a ^ b).collect())
    }
}

/// An HTTPS fetch that serves fixed documents, and fails for anything else.
#[derive(Default)]
struct FakeWeb(
    Mutex<HashMap<String, Result<Vec<u8>, String>>>,
    Mutex<Vec<String>>,
);

impl FakeWeb {
    fn serve(&self, url: &str, body: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(url.into(), Ok(body.as_bytes().to_vec()));
    }
    fn fail(&self, url: &str, why: &str) {
        self.0.lock().unwrap().insert(url.into(), Err(why.into()));
    }
    fn fetched(&self) -> Vec<String> {
        self.1.lock().unwrap().clone()
    }
}

impl HttpsGet for FakeWeb {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        self.1.lock().unwrap().push(url.to_string());
        let body = self
            .0
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or_else(|| Err("404 Not Found".into()))?;
        if body.len() > limit {
            return Err("too large".into());
        }
        Ok(body)
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-fr36-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every byte of every file under `dir`.
fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(all_bytes(&path));
        } else {
            out.extend(std::fs::read(&path).unwrap());
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------------------------------------
// The guest's side.

fn session_with(launcher: FakeLauncher) -> (Session, MemoryConnector) {
    let handshake = ControlHandshake::new("dsa-host test", WAIT);
    let (listener, connector) = memory::pair();
    let config = SessionConfig {
        connect_timeout: WAIT,
        stop_timeout: WAIT,
        ..SessionConfig::default()
    };
    (
        Session::new(launcher, Box::new(listener), handshake, config),
        connector,
    )
}

fn send_body(agent: &mut Box<dyn Connection>, body: Body) {
    agent
        .send(&encode(&Envelope { body: Some(body) }))
        .expect("the agent sends");
}

fn connect_agent(connector: &mut MemoryConnector, launcher: &FakeLauncher) -> Box<dyn Connection> {
    let secret = launcher.secret();
    let mut agent = connector.connect(WAIT).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the agent");
    send_body(
        &mut agent,
        Body::GuestBoot(control::GuestBoot {
            boot_completed: true,
            agent_connected: true,
        }),
    );
    agent
}

/// The next message of a kind `pick` takes, skipping the others the host sends on connect.
fn next_of<T>(agent: &mut Box<dyn Connection>, mut pick: impl FnMut(Body) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the host never sent it"
        );
        let frame = agent.recv(Some(WAIT)).expect("a message for the guest");
        let body = dsa_protocol::decode(&frame)
            .expect("well formed")
            .body
            .expect("a body");
        if let Some(found) = pick(body) {
            return found;
        }
    }
}

fn backup_answer(agent: &mut Box<dyn Connection>, request_id: u64) -> control::BackupAnswer {
    next_of(agent, |body| match body {
        Body::BackupAnswer(answer) if answer.request_id == request_id => Some(answer),
        _ => None,
    })
}

/// Uploads `data` as the agent's transport does, in pieces, and commits it.
fn upload(
    agent: &mut Box<dyn Connection>,
    request_id: u64,
    data: &[u8],
    commit: control::BackupCommit,
) -> control::BackupAnswer {
    for (index, piece) in data.chunks(100_000).enumerate() {
        send_body(
            agent,
            Body::BackupUpload(control::BackupUpload {
                request_id,
                offset: (index * 100_000) as u64,
                data: piece.to_vec(),
            }),
        );
        let answer = backup_answer(agent, request_id);
        assert_eq!(answer.status, BackupStatus::Ok as i32, "{}", answer.message);
    }
    send_body(
        agent,
        Body::BackupCommit(control::BackupCommit {
            request_id,
            size: data.len() as u64,
            ..commit
        }),
    );
    backup_answer(agent, request_id)
}

/// Reads one package's data back as the agent's transport does during a restore.
fn fetch(
    agent: &mut Box<dyn Connection>,
    request_id: u64,
    token: u64,
    package: &str,
    kind: BackupDataKind,
) -> Result<Vec<u8>, (i32, String)> {
    let mut data = Vec::new();
    loop {
        send_body(
            agent,
            Body::BackupFetch(control::BackupFetch {
                request_id,
                token,
                package: package.into(),
                kind: kind as i32,
                apk_name: String::new(),
                offset: data.len() as u64,
            }),
        );
        let piece = next_of(agent, |body| match body {
            Body::BackupFetchData(piece) if piece.request_id == request_id => Some(piece),
            _ => None,
        });
        if piece.status != BackupStatus::Ok as i32 {
            return Err((piece.status, piece.message));
        }
        assert_eq!(piece.offset, data.len() as u64);
        data.extend(piece.data);
        if piece.last {
            return Ok(data);
        }
    }
}

fn contents(agent: &mut Box<dyn Connection>, request_id: u64) -> control::BackupContents {
    send_body(
        agent,
        Body::BackupQuery(control::BackupQuery { request_id }),
    );
    next_of(agent, |body| match body {
        Body::BackupContents(contents) if contents.request_id == request_id => Some(contents),
        _ => None,
    })
}

fn full_backup_of_notes() -> Vec<u8> {
    // A tar stream's worth of an app's files, larger than one piece and one chunk.
    (0..700_000u32)
        .flat_map(|i| (i % 251).to_le_bytes().into_iter().take(1))
        .chain(b"the note that must survive".iter().copied())
        .collect()
}

fn kv(records: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
    records::encode_records(
        &records
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(|v| v.to_vec())))
            .collect(),
    )
}

// ---------------------------------------------------------------------------------------------
// Backup.

#[test]
fn fr36_backup_survives_a_data_reset() {
    let home = scratch("backup-reset");
    let folder = home.join("DSA Backup");
    let keys: Arc<dyn HostKeyStore> = Arc::new(MemoryKeys::default());
    let backup = Arc::new(BackupService::new(
        folder.clone(),
        Arc::clone(&keys),
        "TEST-PC",
    ));
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    session.serve_backup(Arc::clone(&backup));
    let session = Arc::new(session);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // Backup is off until the user turns it on: the agent is told so, nothing the transport
    // sends is kept, and neither a folder nor a key is made.
    assert!(!backup_setting(&mut agent), "the agent hears backup is off");
    let refused = upload_refused(&mut agent, 90);
    assert_eq!(refused.status, BackupStatus::NotAvailable as i32);
    assert!(
        refused.message.contains("backup is off"),
        "{}",
        refused.message
    );
    let report = session.backup_report().expect("a backup");
    assert!(!report.enabled && !report.available);
    assert!(
        report.problem.is_empty(),
        "off is no problem: {}",
        report.problem
    );
    assert!(
        session.backup_now().is_err(),
        "no backup runs while it is off"
    );
    assert!(
        backup.recovery_key().is_err(),
        "no key is made while it is off"
    );
    assert!(
        !folder.exists(),
        "a user who never turned backup on has no backup files"
    );
    assert_eq!(keys.load(KeyPurpose::Backup).unwrap(), None);

    // The user turns it on, and the agent hears it at once.
    let report = session.set_backup(true, false).expect("backup turns on");
    assert!(report.enabled && report.available, "{}", report.problem);
    assert!(backup_setting(&mut agent), "the agent hears backup is on");

    // Android backs the notes app up: a full backup, then its APK when the host asks for it,
    // and the settings provider's key-value records in two passes.
    let full = full_backup_of_notes();
    let answer = upload(
        &mut agent,
        1,
        &full,
        control::BackupCommit {
            package: "com.example.notes".into(),
            kind: BackupDataKind::Full as i32,
            version_code: 7,
            ..Default::default()
        },
    );
    assert_eq!(answer.status, BackupStatus::Ok as i32, "{}", answer.message);
    assert!(
        answer.apks_wanted,
        "the host asks for the APK of an app it has none of"
    );
    let apk = b"PK\x03\x04 the notes app".repeat(5000);
    let answer = upload(
        &mut agent,
        2,
        &apk,
        control::BackupCommit {
            package: "com.example.notes".into(),
            kind: BackupDataKind::Apk as i32,
            version_code: 7,
            apk_name: "base.apk".into(),
            apk_names: vec!["base.apk".into()],
            ..Default::default()
        },
    );
    assert_eq!(answer.status, BackupStatus::Ok as i32, "{}", answer.message);
    let first = kv(&[("theme", Some(b"dark")), ("font", Some(b"large"))]);
    let answer = upload(
        &mut agent,
        3,
        &first,
        control::BackupCommit {
            package: "com.android.providers.settings".into(),
            kind: BackupDataKind::KeyValue as i32,
            version_code: 36,
            ..Default::default()
        },
    );
    assert_eq!(answer.status, BackupStatus::Ok as i32, "{}", answer.message);
    let second = kv(&[("font", None), ("ringtone", Some(b"chime"))]);
    let answer = upload(
        &mut agent,
        4,
        &second,
        control::BackupCommit {
            package: "com.android.providers.settings".into(),
            kind: BackupDataKind::KeyValue as i32,
            version_code: 36,
            incremental: true,
            ..Default::default()
        },
    );
    assert_eq!(answer.status, BackupStatus::Ok as i32, "{}", answer.message);
    let report = session.backup_report().expect("a backup");
    assert!(report.available, "{}", report.problem);
    assert_eq!(report.packages, 2);
    assert!(report.last_backup_ms > 0);

    // Nothing in the folder can be read without the key: not the data, not the APK, not even
    // which apps it holds.
    let stored = all_bytes(&folder);
    assert!(!contains(&stored, b"the note that must survive"));
    assert!(!contains(&stored, b"the notes app"));
    assert!(!contains(&stored, b"com.example.notes"));
    assert!(!contains(&stored, b"chime"));
    let stranger = BackupService::new(folder.clone(), Arc::new(MemoryKeys::default()), "OTHER-PC");
    let refused = stranger.set_enabled(true, false).unwrap();
    assert!(!refused.available);
    assert!(
        refused.problem.contains("recovery key"),
        "{}",
        refused.problem
    );
    let other_key = Arc::new(MemoryKeys::default());
    other_key
        .store(KeyPurpose::Backup, &Secret::generate().unwrap())
        .unwrap();
    let wrong = BackupService::new(folder.clone(), other_key, "OTHER-PC");
    assert!(wrong.contents().is_err(), "another key reads nothing");

    // The user resets Android's data: the disk is replaced, the backup folder stays.
    let disk = home.join("userdata.vhdx");
    std::fs::write(&disk, b"the old disk").unwrap();
    let reset = {
        let disk = disk.clone();
        TemplateDataDisk::new(
            Box::new(move |_user: &str| Ok(disk.clone())),
            Box::new(|path: &Path| {
                std::fs::write(path, b"a fresh disk").map_err(|e| e.to_string())
            }),
        )
    };
    drop(agent);
    reset_user_data(&*session, &reset, "user", WAIT).expect("the data is reset");
    assert_eq!(std::fs::read(&disk).unwrap(), b"a fresh disk");
    assert_eq!(
        all_bytes(&folder),
        stored,
        "the reset left the backup as it was"
    );

    // The fresh Android has none of the apps. A restore installs the app from the backup, then
    // Android reads every app's data back through the transport.
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let restoring = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.restore_backup())
    };
    let query = next_of(&mut agent, |body| match body {
        Body::QueryPackage(query) => Some(query),
        _ => None,
    });
    assert_eq!(query.package_name, "com.example.notes");
    send_body(
        &mut agent,
        Body::PackageState(control::PackageState {
            request_id: query.request_id,
            installed: false,
            ..Default::default()
        }),
    );
    let install = next_of(&mut agent, |body| match body {
        Body::InstallApk(install) => Some(install),
        _ => None,
    });
    assert_eq!(install.parts.len(), 1);
    assert_eq!(install.parts[0].name, "base.apk");
    let mut received = Vec::new();
    while received.len() < apk.len() {
        let chunk = next_of(&mut agent, |body| match body {
            Body::ApkChunk(chunk) => Some(chunk),
            _ => None,
        });
        received.extend(chunk.data);
        send_body(
            &mut agent,
            Body::InstallProgress(control::InstallProgress {
                transfer_id: install.transfer_id,
                received: received.len() as u64,
            }),
        );
    }
    assert!(received == apk, "the APK came back changed");
    send_body(
        &mut agent,
        Body::InstallResult(control::InstallResult {
            transfer_id: install.transfer_id,
            status: control::InstallStatus::Success as i32,
            package_name: "com.example.notes".into(),
            message: String::new(),
        }),
    );
    let restore = next_of(&mut agent, |body| match body {
        Body::BackupRestore(restore) => Some(restore),
        _ => None,
    });
    assert!(restore.packages.contains(&"com.example.notes".to_string()));
    assert!(restore
        .packages
        .contains(&"com.android.providers.settings".to_string()));
    let listed = contents(&mut agent, 10);
    assert_eq!(listed.status, BackupStatus::Ok as i32, "{}", listed.message);
    assert_eq!(listed.token, restore.token);
    assert!(listed.name.contains("TEST-PC"));
    let notes = listed
        .packages
        .iter()
        .find(|p| p.package == "com.example.notes")
        .expect("the notes app is in the backup");
    assert!(notes.full);
    assert_eq!(notes.version_code, 7);
    assert_eq!(notes.apk_names, vec!["base.apk".to_string()]);
    let back = fetch(
        &mut agent,
        11,
        restore.token,
        "com.example.notes",
        BackupDataKind::Full,
    )
    .expect("the full backup reads back");
    assert!(back == full, "the app's data came back changed");
    let settings = fetch(
        &mut agent,
        12,
        restore.token,
        "com.android.providers.settings",
        BackupDataKind::KeyValue,
    )
    .expect("the records read back");
    let mut map = BTreeMap::new();
    records::apply(&mut map, records::parse(&settings).unwrap());
    let expected: BTreeMap<String, Vec<u8>> = [
        ("ringtone".to_string(), b"chime".to_vec()),
        ("theme".to_string(), b"dark".to_vec()),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        map, expected,
        "the records are the two passes applied in order"
    );
    send_body(
        &mut agent,
        Body::BackupRunResult(control::BackupRunResult {
            request_id: restore.request_id,
            status: BackupStatus::Ok as i32,
            message: String::new(),
            succeeded: 2,
            failed: 0,
        }),
    );
    let outcome = restoring.join().unwrap().expect("restored");
    assert_eq!(outcome.installed, 1);
    assert!(outcome.install_failures.is_empty());
    assert_eq!(outcome.result.succeeded, 2);
    assert!(session.backup_report().unwrap().last_restore_ms > 0);

    // Turned off, the agent hears it, the host keeps nothing more, and the backups stay for a
    // later restore until the user has them deleted.
    std::fs::write(folder.join("my notes.txt"), b"the user's own file").unwrap();
    let report = session.set_backup(false, false).expect("backup turns off");
    assert!(!report.enabled && !report.available);
    assert_eq!(report.packages, 2, "what is kept is still counted");
    assert!(!backup_setting(&mut agent), "the agent hears backup is off");
    let refused = upload_refused(&mut agent, 91);
    assert_eq!(refused.status, BackupStatus::NotAvailable as i32);
    assert!(folder.join("backup.json").exists());
    let report = session
        .set_backup(false, true)
        .expect("the backups are deleted");
    assert_eq!(report.packages, 0);
    assert!(!folder.join("backup.json").exists());
    assert!(!folder.join("index.dsab").exists());
    assert!(!folder.join("data").exists());
    assert_eq!(
        std::fs::read(folder.join("my notes.txt")).unwrap(),
        b"the user's own file",
        "a file of the user's own stays"
    );
    assert_eq!(
        keys.load(KeyPurpose::Backup).unwrap(),
        None,
        "the key goes too"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The next backup setting the host tells the agent: whether backup is on.
fn backup_setting(agent: &mut Box<dyn Connection>) -> bool {
    next_of(agent, |body| match body {
        Body::BackupSetting(setting) => Some(setting.enabled),
        _ => None,
    })
}

/// One piece of an upload, which the host is expected to refuse.
fn upload_refused(agent: &mut Box<dyn Connection>, request_id: u64) -> control::BackupAnswer {
    send_body(
        agent,
        Body::BackupUpload(control::BackupUpload {
            request_id,
            offset: 0,
            data: b"settings".to_vec(),
        }),
    );
    backup_answer(agent, request_id)
}

#[test]
fn fr36_backup_is_off_until_the_user_turns_it_on_and_the_choice_is_kept() {
    let home = scratch("backup-switch");
    let switch_path = home
        .join("settings")
        .join("users")
        .join("S-1-5-21-1.backup.json");
    let switch = Arc::new(
        FileSwitch::for_user(&home.join("settings"), "S-1-5-21-1").expect("a user's switch"),
    );
    assert_eq!(switch.path(), switch_path.as_path());
    assert_eq!(
        switch.enabled(),
        Ok(false),
        "off when nothing was ever chosen"
    );
    let folder = home.join("DSA Backup");
    let backup = Arc::new(
        BackupService::new(folder.clone(), Arc::new(MemoryKeys::default()), "PC")
            .with_switch(switch.clone()),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    backup.attach(Arc::new(move |body| tx.send(body).unwrap()));
    for body in [
        Body::BackupUpload(control::BackupUpload {
            request_id: 1,
            offset: 0,
            data: vec![1; 10],
        }),
        Body::BackupQuery(control::BackupQuery { request_id: 2 }),
    ] {
        assert!(backup.guest_message(body));
        match rx.recv_timeout(WAIT).unwrap() {
            Body::BackupAnswer(answer) => {
                assert_eq!(answer.status, BackupStatus::NotAvailable as i32)
            }
            Body::BackupContents(contents) => {
                assert_eq!(contents.status, BackupStatus::NotAvailable as i32)
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert!(backup
        .use_recovery_key(&recovery_key(&Secret::generate().unwrap()))
        .is_err());
    assert!(!folder.exists(), "nothing is made while backup is off");
    assert!(!switch_path.exists(), "asking is not choosing");

    backup.set_enabled(true, false).unwrap();
    assert!(
        backup.set_enabled(true, true).is_err(),
        "deleting goes with turning off"
    );
    let again = BackupService::new(folder.clone(), Arc::new(MemoryKeys::default()), "PC")
        .with_switch(Arc::new(FileSwitch::new(&switch_path)));
    assert!(again.enabled(), "the choice outlives the service");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fr36_a_backup_folder_moved_to_another_computer_reads_with_its_recovery_key() {
    let home = scratch("backup-move");
    let folder = home.join("DSA Backup");
    let here: Arc<dyn HostKeyStore> = Arc::new(MemoryKeys::default());
    let backup = Arc::new(BackupService::new(folder.clone(), here, "FIRST-PC"));
    backup.set_enabled(true, false).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    backup.attach(Arc::new(move |body| tx.send(body).unwrap()));
    let data = b"settings that move with the folder".to_vec();
    assert!(
        backup.guest_message(Body::BackupUpload(control::BackupUpload {
            request_id: 1,
            offset: 0,
            data: data.clone(),
        }))
    );
    rx.recv_timeout(WAIT).unwrap();
    assert!(
        backup.guest_message(Body::BackupCommit(control::BackupCommit {
            request_id: 1,
            package: "com.example.notes".into(),
            kind: BackupDataKind::Full as i32,
            size: data.len() as u64,
            version_code: 1,
            ..Default::default()
        }))
    );
    rx.recv_timeout(WAIT).unwrap();
    let key = backup.recovery_key().expect("a recovery key");

    let elsewhere = BackupService::new(folder.clone(), Arc::new(MemoryKeys::default()), "SECOND");
    elsewhere.set_enabled(true, false).unwrap();
    assert!(elsewhere.contents().is_err(), "no key, no backup");
    let wrong = recovery_key(&Secret::generate().unwrap());
    assert!(
        elsewhere.use_recovery_key(&wrong).is_err(),
        "a key the folder was not made with is refused"
    );
    elsewhere
        .use_recovery_key(&key.to_lowercase().replace('-', " "))
        .expect("the folder's own key, however it is written");
    let (_, packages) = elsewhere.contents().expect("the folder reads");
    assert_eq!(packages[0].package, "com.example.notes");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fr36_a_backup_is_refused_without_a_credential_store() {
    let home = scratch("backup-no-store");
    let backup = BackupService::new(
        home.join("DSA Backup"),
        Arc::new(NoKeyStore("the keyring is locked".into())),
        "PC",
    );
    let report = backup.set_enabled(true, false).unwrap();
    assert!(!report.available);
    assert!(report.problem.contains("the keyring is locked"));
    let (tx, rx) = std::sync::mpsc::channel();
    let backup = Arc::new(backup);
    backup.attach(Arc::new(move |body| tx.send(body).unwrap()));
    assert!(backup.guest_message(Body::BackupQuery(control::BackupQuery { request_id: 4 })));
    match rx.recv_timeout(WAIT).unwrap() {
        Body::BackupContents(contents) => {
            assert_eq!(contents.status, BackupStatus::NotAvailable as i32)
        }
        other => panic!("expected the contents, got {other:?}"),
    }
    assert!(
        !home.join("DSA Backup").exists(),
        "nothing is written without a key"
    );
    let _ = std::fs::remove_dir_all(&home);
}

thread_local! {
    /// Whether this thread runs with the backup folder owner's rights, in the test below.
    static AS_OWNER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A service that runs as another account (root) uses the user's folder only with the
/// user's own rights: every guest message, report and key goes through them.
#[test]
fn fr36_a_backup_folder_is_used_only_with_its_owners_rights() {
    let home = scratch("backup-owner");
    let folder = home.join("DSA Backup");
    let entered = Arc::new(Mutex::new(0u32));
    let counted = Arc::clone(&entered);
    let backup = Arc::new(
        BackupService::new(folder.clone(), Arc::new(MemoryKeys::default()), "PC").with_file_access(
            Arc::new(move |run: &mut dyn FnMut()| {
                assert!(!AS_OWNER.with(|c| c.get()), "the owner's rights never nest");
                *counted.lock().unwrap() += 1;
                AS_OWNER.with(|c| c.set(true));
                run();
                AS_OWNER.with(|c| c.set(false));
                Ok(())
            }),
        ),
    );
    backup.set_enabled(true, false).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    backup.attach(Arc::new(move |body| tx.send(body).unwrap()));
    assert!(
        backup.guest_message(Body::BackupUpload(control::BackupUpload {
            request_id: 1,
            offset: 0,
            data: vec![7; 10],
        }))
    );
    match rx.recv_timeout(WAIT).unwrap() {
        Body::BackupAnswer(answer) => assert_eq!(answer.status, BackupStatus::Ok as i32),
        other => panic!("expected an answer, got {other:?}"),
    }
    assert!(
        backup.guest_message(Body::BackupDiscard(control::BackupDiscard {
            request_id: 1
        }))
    );
    assert!(backup.report().available);
    assert!(backup.recovery_key().is_ok());
    backup.guest_gone();
    assert!(
        *entered.lock().unwrap() >= 4,
        "the upload, the discard, the report and the key went through the owner's rights"
    );
    assert!(folder.exists());

    // Rights that cannot be had: nothing is written, and the reason is told.
    let elsewhere = home.join("Other Backup");
    let refused = Arc::new(
        BackupService::new(elsewhere.clone(), Arc::new(MemoryKeys::default()), "PC")
            .with_file_access(Arc::new(|_: &mut dyn FnMut()| {
                Err("the user's identity was refused".to_string())
            })),
    );
    let report = refused.set_enabled(true, false).unwrap();
    assert!(!report.available);
    assert!(report.problem.contains("the user's identity was refused"));
    let (tx, rx) = std::sync::mpsc::channel();
    refused.attach(Arc::new(move |body| tx.send(body).unwrap()));
    assert!(
        refused.guest_message(Body::BackupUpload(control::BackupUpload {
            request_id: 2,
            offset: 0,
            data: vec![7; 10],
        }))
    );
    match rx.recv_timeout(WAIT).unwrap() {
        Body::BackupAnswer(answer) => {
            assert_eq!(answer.status, BackupStatus::NotAvailable as i32);
            assert!(answer.message.contains("the user's identity was refused"));
        }
        other => panic!("expected an answer, got {other:?}"),
    }
    assert!(refused.recovery_key().is_err());
    assert!(!elsewhere.exists(), "nothing is written without the rights");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fr36_an_upload_out_of_order_or_short_is_not_kept() {
    let home = scratch("backup-order");
    let backup = Arc::new(BackupService::new(
        home.join("DSA Backup"),
        Arc::new(MemoryKeys::default()),
        "PC",
    ));
    backup.set_enabled(true, false).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    backup.attach(Arc::new(move |body| tx.send(body).unwrap()));
    let answer = |body: Body| {
        assert!(backup.guest_message(body));
        match rx.recv_timeout(WAIT).unwrap() {
            Body::BackupAnswer(answer) => answer,
            other => panic!("expected an answer, got {other:?}"),
        }
    };
    let first = answer(Body::BackupUpload(control::BackupUpload {
        request_id: 1,
        offset: 0,
        data: vec![1; 10],
    }));
    assert_eq!(first.status, BackupStatus::Ok as i32);
    let skipped = answer(Body::BackupUpload(control::BackupUpload {
        request_id: 1,
        offset: 20,
        data: vec![1; 10],
    }));
    assert_eq!(skipped.status, BackupStatus::Error as i32);
    answer(Body::BackupUpload(control::BackupUpload {
        request_id: 2,
        offset: 0,
        data: vec![1; 10],
    }));
    let short = answer(Body::BackupCommit(control::BackupCommit {
        request_id: 2,
        package: "com.example.notes".into(),
        kind: BackupDataKind::Full as i32,
        size: 11,
        ..Default::default()
    }));
    assert_eq!(short.status, BackupStatus::Error as i32);
    let bad_name = answer(Body::BackupCommit(control::BackupCommit {
        request_id: 3,
        package: "../../etc".into(),
        kind: BackupDataKind::Full as i32,
        ..Default::default()
    }));
    assert_eq!(bad_name.status, BackupStatus::Error as i32);
    let bad_apk = answer(Body::BackupCommit(control::BackupCommit {
        request_id: 4,
        package: "com.example.notes".into(),
        kind: BackupDataKind::Apk as i32,
        apk_name: "../base.apk".into(),
        apk_names: vec!["../base.apk".into()],
        ..Default::default()
    }));
    assert_eq!(bad_apk.status, BackupStatus::Error as i32);
    assert_eq!(backup.report().packages, 0, "nothing half-sent was kept");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fr36_a_backup_file_is_bound_to_its_key_its_slot_and_its_whole_length() {
    let key = BackupKey::new(&Secret::generate().unwrap());
    let plain: Vec<u8> = (0..200_000u32).map(|i| (i * 7) as u8).collect();
    let mut sealed = Vec::new();
    let mut writer = EncryptingWriter::new(&mut sealed, &key, "slot-a").unwrap();
    std::io::Write::write_all(&mut writer, &plain).unwrap();
    writer.finish().unwrap();
    let read = DecryptingReader::new(&sealed[..], &key, "slot-a")
        .unwrap()
        .read_all()
        .unwrap();
    assert!(read == plain);
    // Another slot.
    assert!(DecryptingReader::new(&sealed[..], &key, "slot-b")
        .unwrap()
        .read_all()
        .is_err());
    // Another key.
    let other = BackupKey::new(&Secret::generate().unwrap());
    assert!(matches!(
        DecryptingReader::new(&sealed[..], &other, "slot-a"),
        Err(OpenError::OtherKey(_))
    ));
    // Cut short at a chunk's end, so every chunk left is whole.
    let cut = &sealed[..dsa_host_core::backup::crypto::HEADER + 65536 + 16];
    assert!(DecryptingReader::new(cut, &key, "slot-a")
        .unwrap()
        .read_all()
        .is_err());
    // One bit changed.
    let mut flipped = sealed.clone();
    let at = flipped.len() / 2;
    flipped[at] ^= 1;
    assert!(DecryptingReader::new(&flipped[..], &key, "slot-a")
        .unwrap()
        .read_all()
        .is_err());
    // An exact multiple of the chunk size, and nothing at all.
    for len in [65536usize, 0] {
        let mut sealed = Vec::new();
        let mut writer = EncryptingWriter::new(&mut sealed, &key, "s").unwrap();
        std::io::Write::write_all(&mut writer, &plain[..len]).unwrap();
        writer.finish().unwrap();
        let read = DecryptingReader::new(&sealed[..], &key, "s")
            .unwrap()
            .read_all()
            .unwrap();
        assert_eq!(read.len(), len);
    }
}

#[test]
fn fr36_a_recovery_key_carries_the_backup_key_and_catches_a_typo() {
    let secret = Secret::generate().unwrap();
    let text = recovery_key(&secret);
    assert_eq!(text.split('-').count(), 11);
    assert_eq!(parse_recovery_key(&text).unwrap(), secret);
    let mangled = text.replace('0', "O").replace('1', "l");
    assert_eq!(parse_recovery_key(&mangled).unwrap(), secret);
    let mut typo: Vec<char> = text.chars().collect();
    typo[3] = if typo[3] == 'A' { 'B' } else { 'A' };
    let typo: String = typo.into_iter().collect();
    assert!(parse_recovery_key(&typo).is_err());
    assert!(parse_recovery_key("ABCD").is_err());
    assert!(parse_recovery_key(&text.replace(text.chars().next().unwrap(), "U")).is_err());
}

#[test]
fn fr36_key_value_records_round_trip_and_apply_in_order() {
    let data = kv(&[("a", Some(b"1")), ("b", Some(b"")), ("a", None)]);
    let parsed = records::parse(&data).unwrap();
    assert_eq!(parsed.len(), 3);
    let mut map = BTreeMap::new();
    records::apply(&mut map, parsed);
    assert_eq!(map.len(), 1);
    assert_eq!(map["b"], b"".to_vec());
    assert!(records::parse(&data[..data.len() - 1]).is_err());
    assert!(records::parse(&[0, 0, 0, 0]).is_err(), "an empty key");
}

// ---------------------------------------------------------------------------------------------
// The credential store and the KeyMint root key.

#[test]
fn fr36_a_sealed_secret_opens_only_for_its_user_on_its_computer() {
    let dir = scratch("sealed");
    let mine = SealedFileStore::new(dir.join("alice"), BoundSealer::new("PC-1", "alice"));
    let secret = load_or_create(&mine, KeyPurpose::Backup).unwrap();
    assert_eq!(load_or_create(&mine, KeyPurpose::Backup).unwrap(), secret);
    let file = std::fs::read(mine.path(KeyPurpose::Backup)).unwrap();
    assert!(
        !contains(&file, secret.bytes()),
        "the file holds the secret sealed only"
    );

    for (computer, user) in [("PC-1", "bob"), ("PC-2", "alice")] {
        let there = dir.join(format!("{computer}-{user}"));
        std::fs::create_dir_all(&there).unwrap();
        std::fs::copy(
            mine.path(KeyPurpose::Backup),
            there.join("backup-key.sealed"),
        )
        .unwrap();
        let theirs = SealedFileStore::new(&there, BoundSealer::new(computer, user));
        assert!(matches!(
            theirs.load(KeyPurpose::Backup),
            Err(KeyError::Unreadable(_))
        ));
        assert!(
            load_or_create(&theirs, KeyPurpose::Backup).is_err(),
            "a secret that cannot be opened is never replaced"
        );
        assert_eq!(
            std::fs::read(there.join("backup-key.sealed")).unwrap(),
            file
        );
    }
    // Each purpose has its own secret.
    assert_ne!(
        load_or_create(&mine, KeyPurpose::KeyMintRoot).unwrap(),
        secret
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Asks for the root key on the KeyMint port as the guest's control daemon does.
fn ask(
    connector: &mut MemoryConnector,
    secret: &BootSecret,
) -> Result<Result<Vec<u8>, String>, String> {
    let mut connection = connector.connect(WAIT).expect("the port answers");
    ask_root_key(
        connection.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd keymint test"),
        secret,
    )
}

/// The TPM of a [`FakeHost`].
#[derive(Clone)]
enum FakeTpm {
    /// None, or turned off in the firmware.
    Absent,
    /// It works; the text names it, and a clear gives it another name.
    Working(&'static str),
    /// Locked out after too many wrong authorizations.
    LockedOut,
}

/// A computer for the KeyMint root key: a credential store bound to its user, and a TPM that
/// can be there, gone, locked out or cleared.
struct FakeHost {
    computer: &'static str,
    user: &'static str,
    tpm: Mutex<FakeTpm>,
}

impl FakeHost {
    fn new(computer: &'static str, user: &'static str, tpm: FakeTpm) -> Arc<FakeHost> {
        Arc::new(FakeHost {
            computer,
            user,
            tpm: Mutex::new(tpm),
        })
    }

    fn set_tpm(&self, tpm: FakeTpm) {
        *self.tpm.lock().unwrap() = tpm;
    }
}

impl SealerSource for FakeHost {
    fn hardware(&self, _: KeyPurpose) -> Hardware {
        match self.tpm.lock().unwrap().clone() {
            FakeTpm::Working(id) => {
                Hardware::Usable(BoundSealer::tpm(self.computer, self.user, id))
            }
            FakeTpm::Absent => Hardware::Unusable("this computer has no TPM".into()),
            FakeTpm::LockedOut => Hardware::Unusable("the TPM is locked out".into()),
        }
    }
    fn fallback(&self, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
        Ok(BoundSealer::new(self.computer, self.user))
    }
    fn named(&self, name: &str, _: KeyPurpose) -> Result<Arc<dyn Sealer>, KeyError> {
        match (name, self.tpm.lock().unwrap().clone()) {
            ("test-bound", _) => Ok(BoundSealer::new(self.computer, self.user)),
            ("test-tpm", FakeTpm::Working(id)) => {
                Ok(BoundSealer::tpm(self.computer, self.user, id))
            }
            ("test-tpm", FakeTpm::LockedOut) => Err(KeyError::Unavailable(
                "the TPM is locked out; it opens again once the lockout ends".into(),
            )),
            ("test-tpm", FakeTpm::Absent) => Err(KeyError::Unreadable(
                "the TPM that sealed it is turned off or gone".into(),
            )),
            (other, _) => Err(KeyError::Unreadable(format!("unknown sealer {other}"))),
        }
    }
}

/// Stops the session's Android if it runs, starts it again, and asks for this boot's root key.
fn boot_and_ask(
    session: &Session,
    connector: &mut MemoryConnector,
    key_connector: &mut MemoryConnector,
    launcher: &FakeLauncher,
) -> Result<Vec<u8>, String> {
    if session.state() != SessionState::Stopped {
        session.stop();
        assert!(session.wait_for(SessionState::Stopped, WAIT));
    }
    session.start();
    let _agent = connect_agent(connector, launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    ask(key_connector, &launcher.secret()).expect("the handshake passes")
}

#[test]
fn fr36_keymint_root_key_is_sealed_by_the_host() {
    let dir = scratch("keymint");
    let alice: Arc<dyn HostKeyStore> = Arc::new(SealedKeys::new(
        dir.join("alice"),
        FakeHost::new("PC-1", "alice", FakeTpm::Working("tpm-1")),
    ));
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    let (key_listener, mut key_connector) = memory::pair();
    session.set_keymint_keys(Arc::clone(&alice));
    session.serve_keymint_port(Box::new(key_listener), "dsa-host keymint test");
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let secret = launcher.secret();

    // Without the boot's secret, nothing.
    let stranger = BootSecret::generate().unwrap();
    assert!(ask(&mut key_connector, &stranger).is_err());

    // The control daemon gets the root key, once a boot.
    let key = ask(&mut key_connector, &secret)
        .expect("the handshake passes")
        .expect("a root key");
    assert_eq!(key.len(), 32);
    assert!(
        ask(&mut key_connector, &secret).is_err(),
        "a second request in the same boot gets nothing"
    );

    // The host keeps it sealed, by the computer's TPM since it has one that works: no file
    // holds it in the clear, and the settings app and dsa status can say what sealed it.
    assert!(!contains(&all_bytes(&dir), &key));
    let state = session
        .keymint_key_state()
        .expect("the session keeps a root key");
    let protection = state.protection.expect("a key is kept");
    assert!(protection.hardware, "{protection:?}");
    assert_eq!(protection.sealed_by, "tpm");
    assert!(protection.fallback_reason.is_empty());
    assert!(state.problem.is_empty(), "{}", state.problem);

    // The next boot gets the same key, so the keys KeyMint wrapped under it still open.
    drop(agent);
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    session.start();
    agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let again = ask(&mut key_connector, &launcher.secret())
        .unwrap()
        .expect("a root key");
    assert_eq!(again, key);
    drop(agent);

    // The sealed key moved to another user, or to another computer, does not open there, and
    // the guest is told why rather than given a new key.
    for (computer, user) in [("PC-1", "bob"), ("PC-2", "alice")] {
        let there = dir.join(format!("{computer}-{user}"));
        std::fs::create_dir_all(&there).unwrap();
        std::fs::copy(
            dir.join("alice").join("keymint-root-key.sealed"),
            there.join("keymint-root-key.sealed"),
        )
        .unwrap();
        let store: Arc<dyn HostKeyStore> = Arc::new(SealedKeys::new(
            &there,
            FakeHost::new(computer, user, FakeTpm::Working("tpm-1")),
        ));
        let refused = root_key(&store).expect_err("it opens only where it was sealed");
        assert!(refused.contains("cannot be opened"), "{refused}");
    }

    // A host without a credential store gives no key, and says why.
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    let (key_listener, mut key_connector) = memory::pair();
    session.set_keymint_keys(Arc::new(NoKeyStore("no keyring".into())));
    session.serve_keymint_port(Box::new(key_listener), "dsa-host keymint test");
    session.start();
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let none = ask(&mut key_connector, &launcher.secret())
        .unwrap()
        .expect_err("no key");
    assert!(none.contains("no keyring"), "{none}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A computer without a usable TPM keeps the root key with the credential store and says so;
/// once a TPM works the same key is sealed by it; a TPM locked out or cleared leaves it
/// refused with the reason, never replaced, until a data reset lets it go.
#[test]
fn fr36_keymint_root_key_falls_back_without_a_tpm_and_moves_to_one() {
    let dir = scratch("keymint-fallback");
    let host = FakeHost::new("PC-1", "alice", FakeTpm::Absent);
    let keys = Arc::new(SealedKeys::new(dir.join("alice"), host.clone()));
    let file = keys.path(KeyPurpose::KeyMintRoot);
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    let (key_listener, mut key_connector) = memory::pair();
    session.set_keymint_keys(keys.clone());
    session.serve_keymint_port(Box::new(key_listener), "dsa-host keymint test");
    let session = Arc::new(session);
    assert_eq!(
        session.keymint_key_state().unwrap().protection,
        None,
        "no key before the first start"
    );

    // No TPM: the credential store keeps it, and the reason is told.
    let first = boot_and_ask(&session, &mut connector, &mut key_connector, &launcher)
        .expect("a root key without a TPM");
    let protection = session.keymint_key_state().unwrap().protection.unwrap();
    assert!(!protection.hardware);
    assert_eq!(protection.sealed_by, "dpapi");
    assert_eq!(protection.fallback_reason, "this computer has no TPM");
    assert_eq!(
        sealer_of(&std::fs::read(&file).unwrap()),
        Some("test-bound")
    );

    // The TPM is turned on: the same key, sealed by it now.
    host.set_tpm(FakeTpm::Working("tpm-1"));
    let moved = boot_and_ask(&session, &mut connector, &mut key_connector, &launcher)
        .expect("the same root key");
    assert_eq!(
        moved, first,
        "nothing Android wrapped under the key is lost"
    );
    assert_eq!(sealer_of(&std::fs::read(&file).unwrap()), Some("test-tpm"));
    assert!(
        session
            .keymint_key_state()
            .unwrap()
            .protection
            .unwrap()
            .hardware
    );

    // Locked out: no key this boot, with the reason, and the key kept as it was.
    let sealed = std::fs::read(&file).unwrap();
    host.set_tpm(FakeTpm::LockedOut);
    let why = boot_and_ask(&session, &mut connector, &mut key_connector, &launcher)
        .expect_err("no key while the TPM is locked out");
    assert!(why.contains("locked out"), "{why}");
    assert!(session
        .keymint_key_state()
        .unwrap()
        .problem
        .contains("locked out"));
    assert_eq!(std::fs::read(&file).unwrap(), sealed);
    host.set_tpm(FakeTpm::Working("tpm-1"));
    assert_eq!(
        boot_and_ask(&session, &mut connector, &mut key_connector, &launcher).unwrap(),
        first,
        "the lockout ended and the key opens again"
    );
    assert!(session.keymint_key_state().unwrap().problem.is_empty());

    // Cleared: the TPM is another one now. The key is refused, with what to do, and never
    // replaced by a new one.
    host.set_tpm(FakeTpm::Working("tpm-2"));
    let why = boot_and_ask(&session, &mut connector, &mut key_connector, &launcher)
        .expect_err("a cleared TPM opens nothing it sealed");
    assert!(why.contains("cannot be opened"), "{why}");
    let problem = session.keymint_key_state().unwrap().problem;
    assert!(problem.contains("dsa reset-data"), "{problem}");
    assert_eq!(std::fs::read(&file).unwrap(), sealed, "never replaced");

    // A data reset leaves nothing wrapped under the old key, so it goes, and the next start
    // has a new one, sealed by the TPM there is now.
    let disk = dir.join("userdata.vhdx");
    std::fs::write(&disk, b"the old disk").unwrap();
    let reset = {
        let disk = disk.clone();
        TemplateDataDisk::new(
            Box::new(move |_user: &str| Ok(disk.clone())),
            Box::new(|path: &Path| {
                std::fs::write(path, b"a fresh disk").map_err(|e| e.to_string())
            }),
        )
    };
    reset_user_data(&*session, &reset, "alice", WAIT).expect("the data is reset");
    assert!(!file.exists(), "the old root key went with the old data");
    let fresh = boot_and_ask(&session, &mut connector, &mut key_connector, &launcher)
        .expect("a new root key after the reset");
    assert_ne!(fresh, first);
    assert_eq!(sealer_of(&std::fs::read(&file).unwrap()), Some("test-tpm"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// Passkeys.

const NOTES: &str = "com.example.notes";
const BANK: &str = "bank.example";

fn notes_cert() -> Vec<u8> {
    Sha256::digest(b"the notes app's certificate").to_vec()
}

fn statements(relation: &str, package: &str, certs: &[Vec<u8>]) -> String {
    let fingerprints: Vec<String> = certs.iter().map(|c| fingerprint(c)).collect();
    serde_json::json!([{
        "relation": [relation],
        "target": {
            "namespace": "android_app",
            "package_name": package,
            "sha256_cert_fingerprints": fingerprints,
        }
    }])
    .to_string()
}

fn create_json(rp_id: &str) -> String {
    serde_json::json!({
        "rp": { "id": rp_id, "name": "Bank" },
        "user": { "id": base64url(b"user-1"), "name": "ada", "displayName": "Ada" },
        "challenge": base64url(b"a challenge of the relying party"),
        "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }],
        "authenticatorSelection": { "residentKey": "required", "userVerification": "required" },
        "excludeCredentials": [{ "type": "public-key", "id": base64url(b"old") }],
    })
    .to_string()
}

fn get_json(rp_id: &str) -> String {
    serde_json::json!({
        "rpId": rp_id,
        "challenge": base64url(b"another challenge"),
        "allowCredentials": [],
    })
    .to_string()
}

fn passkey_request(operation: PasskeyOperation, json: String) -> control::PasskeyRequest {
    control::PasskeyRequest {
        request_id: 1,
        operation: operation as i32,
        request_json: json,
        package: NOTES.into(),
        signer_sha256: vec![notes_cert()],
        origin: String::new(),
        client_data_hash: Vec::new(),
        app_label: "Notes".into(),
    }
}

/// The CBOR of an attestation object for a new ES256 passkey with the id `id`.
fn attestation_object(id: &[u8]) -> Vec<u8> {
    let mut auth = Sha256::digest(BANK.as_bytes()).to_vec();
    auth.push(0x45); // user present, user verified, attested credential data
    auth.extend([0, 0, 0, 0]);
    auth.extend([0u8; 16]);
    auth.extend((id.len() as u16).to_be_bytes());
    auth.extend(id);
    // {1: 2, 3: -7, -1: 1, -2: x, -3: y}
    auth.extend([0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20]);
    auth.extend([7u8; 32]);
    auth.extend([0x22, 0x58, 0x20]);
    auth.extend([9u8; 32]);
    // {"fmt": "none", "attStmt": {}, "authData": auth}
    let mut object = vec![0xa3, 0x63];
    object.extend(b"fmt");
    object.push(0x64);
    object.extend(b"none");
    object.push(0x67);
    object.extend(b"attStmt");
    object.push(0xa0);
    object.push(0x68);
    object.extend(b"authData");
    object.extend([0x59, (auth.len() >> 8) as u8, auth.len() as u8]);
    object.extend(auth);
    object
}

/// An active session whose passkeys are checked against `web`.
fn passkey_session(web: Arc<FakeWeb>) -> (Arc<Session>, Box<dyn Connection>) {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    session.serve_passkeys(Arc::new(PasskeyService::new(web)));
    session.start();
    let agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    (Arc::new(session), agent)
}

fn passkey_response(agent: &mut Box<dyn Connection>, request_id: u64) -> control::PasskeyResponse {
    next_of(agent, |body| match body {
        Body::PasskeyResponse(response) if response.request_id == request_id => Some(response),
        _ => None,
    })
}

/// Sends `request` and expects it refused without any ceremony reaching the watcher.
fn refused(
    agent: &mut Box<dyn Connection>,
    watch: &std::sync::mpsc::Receiver<PasskeyEvent>,
    request: control::PasskeyRequest,
    why: &str,
) -> String {
    let id = request.request_id;
    send_body(agent, Body::PasskeyRequest(request));
    let response = passkey_response(agent, id);
    assert_eq!(
        response.status,
        PasskeyStatus::Rejected as i32,
        "{why}: {}",
        response.message
    );
    assert!(response.response_json.is_empty());
    assert!(
        watch.try_recv().is_err(),
        "{why}: a refused request reached the host's authenticator"
    );
    response.message
}

#[test]
fn fr36_passkeys_only_for_verified_relying_parties() {
    let web = Arc::new(FakeWeb::default());
    let links = format!("https://{BANK}/.well-known/assetlinks.json");
    web.serve(
        &links,
        &statements(
            "delegate_permission/common.get_login_creds",
            NOTES,
            &[notes_cert()],
        ),
    );
    let (session, mut agent) = passkey_session(Arc::clone(&web));
    let watch = session
        .watch_passkeys()
        .expect("the session serves passkeys");

    // An app the relying party lists, with its certificate: the ceremony reaches the host,
    // with the app's own origin in the client data.
    send_body(
        &mut agent,
        Body::PasskeyRequest(passkey_request(PasskeyOperation::Create, create_json(BANK))),
    );
    let (id, ceremony) = match watch.recv_timeout(WAIT).expect("a ceremony") {
        PasskeyEvent::Ceremony { id, ceremony } => (id, ceremony),
        other => panic!("expected a ceremony, got {other:?}"),
    };
    assert!(ceremony.create);
    assert_eq!(ceremony.rp_id, BANK);
    assert_eq!(ceremony.user_id, b"user-1");
    assert_eq!(ceremony.algorithms, vec![-7]);
    assert_eq!(ceremony.credential_ids, vec![b"old".to_vec()]);
    assert_eq!(ceremony.resident_key, "required");
    let origin = format!("android:apk-key-hash:{}", base64url(&notes_cert()));
    assert_eq!(ceremony.origin, origin);
    let client: Value = serde_json::from_slice(&ceremony.client_data_json).unwrap();
    assert_eq!(client["type"], "webauthn.create");
    assert_eq!(client["origin"], origin.as_str());
    // WebAuthn's own serialization, which every host authenticator signs the same.
    assert_eq!(
        String::from_utf8(ceremony.client_data_json.clone()).unwrap(),
        format!(
            "{{\"type\":\"webauthn.create\",\"challenge\":\"{}\",\"origin\":\"{origin}\",\
             \"crossOrigin\":false}}",
            base64url(b"a challenge of the relying party")
        )
    );
    assert_eq!(
        from_base64url(client["challenge"].as_str().unwrap()).unwrap(),
        b"a challenge of the relying party"
    );
    assert_eq!(
        ceremony.client_data_hash,
        Sha256::digest(&ceremony.client_data_json).to_vec()
    );
    let object = attestation_object(b"new-passkey");
    session
        .passkey_result(
            id,
            CeremonyResult::Created {
                credential_id: b"new-passkey".to_vec(),
                attestation_object: object.clone(),
                transports: vec!["internal".into()],
            },
        )
        .unwrap();
    let response = passkey_response(&mut agent, 1);
    assert_eq!(
        response.status,
        PasskeyStatus::Ok as i32,
        "{}",
        response.message
    );
    let json: Value = serde_json::from_str(&response.response_json).unwrap();
    assert_eq!(json["id"], base64url(b"new-passkey"));
    assert_eq!(json["type"], "public-key");
    assert_eq!(json["response"]["attestationObject"], base64url(&object));
    assert_eq!(json["response"]["publicKeyAlgorithm"], -7);
    assert!(json["response"]["publicKey"].is_string());
    assert_eq!(
        from_base64url(json["response"]["clientDataJSON"].as_str().unwrap()).unwrap(),
        ceremony.client_data_json
    );

    // Using it, and the person canceling the prompt.
    let mut get = passkey_request(PasskeyOperation::Get, get_json(BANK));
    get.request_id = 2;
    send_body(&mut agent, Body::PasskeyRequest(get));
    let id = match watch.recv_timeout(WAIT).unwrap() {
        PasskeyEvent::Ceremony { id, ceremony } => {
            assert!(!ceremony.create);
            id
        }
        other => panic!("expected a ceremony, got {other:?}"),
    };
    session
        .passkey_result(id, CeremonyResult::Canceled("canceled".into()))
        .unwrap();
    assert_eq!(
        passkey_response(&mut agent, 2).status,
        PasskeyStatus::Canceled as i32
    );

    // Every way a request falls short is refused before it reaches the host.
    let mut next = 10;
    let mut request = |edit: &dyn Fn(&mut control::PasskeyRequest)| {
        next += 1;
        let mut request = passkey_request(PasskeyOperation::Create, create_json(BANK));
        request.request_id = next;
        edit(&mut request);
        request
    };
    let cases: Vec<(control::PasskeyRequest, &str)> = vec![
        (
            request(&|r| r.package = "com.evil.notes".into()),
            "another package with the same certificate",
        ),
        (
            request(&|r| r.signer_sha256 = vec![Sha256::digest(b"evil").to_vec()]),
            "the same package signed by someone else",
        ),
        (
            request(&|r| r.signer_sha256 = vec![notes_cert(), Sha256::digest(b"second").to_vec()]),
            "an app with a second signer the site does not list",
        ),
        (
            request(&|r| r.signer_sha256.clear()),
            "no certificate at all",
        ),
        (
            request(&|r| r.request_json = create_json("other.example")),
            "a relying party that does not list the app",
        ),
        (
            request(&|r| r.request_json = create_json("example")),
            "a relying party ID of one label",
        ),
        (
            request(&|r| r.request_json = create_json("192.168.0.1")),
            "an address as relying party ID",
        ),
        (
            request(&|r| r.request_json = create_json("bank.example/evil")),
            "a relying party ID that is not a domain",
        ),
        (
            request(&|r| r.client_data_hash = vec![1; 32]),
            "an app that brings its own client data",
        ),
        (
            request(&|r| r.request_json = "not json".into()),
            "a request that is not JSON",
        ),
        (
            request(&|r| r.request_json = r#"{"rp":{"id":"bank.example"}}"#.into()),
            "a request without a challenge",
        ),
        (
            request(&|r| r.operation = PasskeyOperation::Unspecified as i32),
            "neither make nor use",
        ),
        (
            request(&|r| r.origin = "https://evil.example".into()),
            "a browser page of another site",
        ),
        (
            request(&|r| r.origin = "http://bank.example".into()),
            "a page that is not https",
        ),
        (
            request(&|r| r.origin = "https://notbank.example".into()),
            "a site whose name only ends like the relying party's",
        ),
        (
            request(&|r| r.request_json = "x".repeat(70 * 1024)),
            "an oversized request",
        ),
    ];
    for (request, why) in cases {
        refused(&mut agent, &watch, request, why);
    }

    // A site that lists the app only for opening its links, or whose file cannot be had (a
    // redirect counts as that), lets it sign in nowhere.
    let links_only = "links.example";
    web.serve(
        &format!("https://{links_only}/.well-known/assetlinks.json"),
        &statements(
            "delegate_permission/common.handle_all_urls",
            NOTES,
            &[notes_cert()],
        ),
    );
    web.fail(
        "https://moved.example/.well-known/assetlinks.json",
        "a redirect to https://elsewhere.example, which is not followed",
    );
    web.serve(
        "https://broken.example/.well-known/assetlinks.json",
        "{\"not\": \"a list\"}",
    );
    for (n, rp) in [
        "links.example",
        "moved.example",
        "broken.example",
        "absent.example",
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = passkey_request(PasskeyOperation::Get, get_json(rp));
        r.request_id = 100 + n as u64;
        refused(&mut agent, &watch, r, rp);
    }
    assert!(
        !web.fetched().iter().any(|url| url.contains("elsewhere")),
        "no redirect was followed"
    );

    // A browser the framework let set an origin passes for a page of the relying party.
    let mut browser = passkey_request(PasskeyOperation::Get, get_json(BANK));
    browser.request_id = 200;
    browser.package = "org.example.browser".into();
    browser.signer_sha256 = vec![Sha256::digest(b"browser").to_vec()];
    browser.origin = "https://login.bank.example".into();
    send_body(&mut agent, Body::PasskeyRequest(browser));
    match watch.recv_timeout(WAIT).expect("a ceremony") {
        PasskeyEvent::Ceremony { id, ceremony } => {
            assert_eq!(ceremony.origin, "https://login.bank.example");
            let client: Value = serde_json::from_slice(&ceremony.client_data_json).unwrap();
            assert!(client.get("androidPackageName").is_none());
            session
                .passkey_result(
                    id,
                    CeremonyResult::Asserted {
                        credential_id: b"new-passkey".to_vec(),
                        authenticator_data: vec![1, 2, 3],
                        signature: vec![4, 5, 6],
                        user_handle: b"user-1".to_vec(),
                    },
                )
                .unwrap();
        }
        other => panic!("expected a ceremony, got {other:?}"),
    }
    let response = passkey_response(&mut agent, 200);
    assert_eq!(response.status, PasskeyStatus::Ok as i32);
    let json: Value = serde_json::from_str(&response.response_json).unwrap();
    assert_eq!(json["response"]["signature"], base64url(&[4, 5, 6]));
    assert_eq!(json["response"]["userHandle"], base64url(b"user-1"));
}

#[test]
fn fr36_a_passkey_request_with_no_desktop_session_is_unavailable() {
    let web = Arc::new(FakeWeb::default());
    web.serve(
        &format!("https://{BANK}/.well-known/assetlinks.json"),
        &statements(
            "delegate_permission/common.get_login_creds",
            NOTES,
            &[notes_cert()],
        ),
    );
    let (_session, mut agent) = passkey_session(web);
    send_body(
        &mut agent,
        Body::PasskeyRequest(passkey_request(PasskeyOperation::Get, get_json(BANK))),
    );
    assert_eq!(
        passkey_response(&mut agent, 1).status,
        PasskeyStatus::Unavailable as i32
    );
}

#[test]
fn fr36_a_host_without_passkeys_says_why_a_checked_request_is_unavailable() {
    let web = Arc::new(FakeWeb::default());
    web.serve(
        &format!("https://{BANK}/.well-known/assetlinks.json"),
        &statements(
            "delegate_permission/common.get_login_creds",
            NOTES,
            &[notes_cert()],
        ),
    );
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    session.serve_passkeys(Arc::new(
        PasskeyService::new(web).with_no_authenticator_reason("this host keeps no passkeys"),
    ));
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    send_body(
        &mut agent,
        Body::PasskeyRequest(passkey_request(PasskeyOperation::Get, get_json(BANK))),
    );
    let response = passkey_response(&mut agent, 1);
    assert_eq!(response.status, PasskeyStatus::Unavailable as i32);
    assert_eq!(response.message, "this host keeps no passkeys");
}

#[test]
fn fr36_relying_party_ids_and_origins_are_checked_strictly() {
    for good in [
        "bank.example",
        "login.bank.example",
        "xn--bcher-kva.example",
        "a-b.co",
    ] {
        assert!(valid_rp_id(good), "{good}");
    }
    for bad in [
        "",
        "example",
        "Bank.example",
        "bank.example.",
        ".bank.example",
        "-a.example",
        "a_b.example",
        "10.0.0.1",
        "bank..example",
        "bank.example:443",
        "localhost",
    ] {
        assert!(!valid_rp_id(bad), "{bad}");
    }
    assert_eq!(
        https_origin_host("https://Login.Bank.example:8443").as_deref(),
        Some("login.bank.example")
    );
    for bad in [
        "http://bank.example",
        "https://bank.example/path",
        "https://user@bank.example",
        "https://bank.example:x",
        "android:apk-key-hash:abc",
    ] {
        assert_eq!(https_origin_host(bad), None, "{bad}");
    }
}

#[test]
fn fr36_webauthn_encodings_read_back() {
    for data in [&b""[..], b"f", b"fo", b"foo", b"foob", &[0xff, 0xfe, 0x00]] {
        assert_eq!(from_base64url(&base64url(data)).unwrap(), data);
    }
    assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    assert_eq!(from_base64url("-_8=").unwrap(), vec![0xfb, 0xff]);
    assert!(from_base64url("a").is_none());
    assert!(from_base64url("a*b").is_none());
    let attested = attested_credential(&attestation_object(b"id-1")).expect("it reads");
    assert_eq!(attested.credential_id, b"id-1");
    assert_eq!(attested.algorithm, -7);
    let der = attested
        .public_key_der
        .expect("an ES256 key has a DER form");
    assert_eq!(der.len(), 91);
    assert_eq!(&der[der.len() - 64..der.len() - 32], &[7u8; 32]);
    assert!(attested_credential(b"\xa0").is_none());
}

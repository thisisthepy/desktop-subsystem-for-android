//! Root elevation through the daemon's router: the host's config, as it arrives on the
//! control channel, drives the kernel's su guard (a recorder here) and the su relay, and the
//! host's decisions answer the calls the relay holds.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use dsa_guest_channeld::router::{Peer, Router};
use dsa_guest_channeld::su_relay::{ConfigApplier, GuardControl, Outcome, SuRelay};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, RootApp, RootElevationConfig, RootLevel, SuDecision};

/// The kernel's control file, as the writes made to it, each a list of commands.
#[derive(Default, Clone)]
struct Guard {
    writes: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Guard {
    /// The commands of the one write made since the last look.
    fn take(&self) -> Vec<String> {
        let writes = std::mem::take(&mut *self.writes.lock().unwrap());
        assert!(writes.len() <= 1, "one config is one write: {writes:?}");
        writes.into_iter().next().unwrap_or_default()
    }
}

impl GuardControl for Guard {
    fn write(&self, lines: &[String]) -> std::io::Result<()> {
        self.writes.lock().unwrap().push(lines.to_vec());
        Ok(())
    }
}

const PACKAGES: &str = "com.ask 10100 0 /data/user/0/com.ask default 3003\n\
                        com.remember 10102 0 /data/user/0/com.remember default 3003\n\
                        com.shared.one 10300 0 /data/user/0/com.shared.one default 3003\n\
                        com.shared.two 10300 0 /data/user/0/com.shared.two default 3003\n";

fn daemon() -> (Router, Arc<SuRelay>, Guard, Arc<Mutex<Vec<String>>>) {
    let (to_host, _host) = mpsc::channel();
    let relay = Arc::new(SuRelay::new());
    let guard = Guard::default();
    let said = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&said);
    let applier = ConfigApplier::new(
        Arc::clone(&relay),
        Box::new(guard.clone()),
        Box::new(|| Some(PACKAGES.to_string())),
        move |line| log.lock().unwrap().push(line.to_string()),
    );
    let router = Router::new(to_host, |_peer: Peer| Ok(())).on_root_elevation(
        Arc::clone(&relay),
        move |config| {
            applier.apply(config);
        },
    );
    (router, relay, guard, said)
}

/// The config as the host sends it: by package, with no uid.
fn config(enabled: bool, apps: &[(&str, RootLevel)]) -> Vec<u8> {
    dsa_protocol::encode(&Envelope {
        body: Some(Body::RootElevationConfig(RootElevationConfig {
            enabled,
            apps: apps
                .iter()
                .map(|(package, level)| RootApp {
                    package: package.to_string(),
                    uid: 0,
                    level: (*level).into(),
                })
                .collect(),
        })),
    })
}

#[test]
fn fr35_the_hosts_config_writes_the_kernel_guard() {
    let (router, relay, guard, _said) = daemon();
    router.from_host(&config(
        true,
        &[
            ("com.ask", RootLevel::Ask),
            ("com.off", RootLevel::Off),
            ("com.remember", RootLevel::Remember),
        ],
    ));
    let lines = guard.take();
    assert_eq!(lines.first().map(String::as_str), Some("reset"));
    assert_eq!(lines.last().map(String::as_str), Some("enable"));
    // The uids come from the package list.
    assert!(lines.contains(&"allow 10100".to_string()));
    assert!(lines.contains(&"allow 10102".to_string()));
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("allow ")).count(),
        2,
        "only the ask and remember apps may see su: {lines:?}"
    );
    assert!(relay.allows(10100));
    assert!(relay.allows(10102));
}

#[test]
fn fr35_a_changed_config_applies_at_once() {
    let (router, relay, guard, _said) = daemon();
    router.from_host(&config(true, &[("com.ask", RootLevel::Ask)]));
    guard.take();
    let (_, _, waiting) = relay.begin("com.ask", 10100, vec!["id".into()], false);

    // The user sets com.ask to off and com.remember to remember, with Android running.
    router.from_host(&config(true, &[("com.remember", RootLevel::Remember)]));
    let lines = guard.take();
    assert!(!lines.contains(&"allow 10100".to_string()));
    assert!(lines.contains(&"allow 10102".to_string()));
    assert_eq!(lines.first().map(String::as_str), Some("reset"));
    assert!(!relay.allows(10100));
    assert!(matches!(waiting.recv().unwrap(), Outcome::Refused(_)));

    // The administrator turns the feature off: su is hidden from every app at once.
    router.from_host(&config(false, &[("com.remember", RootLevel::Remember)]));
    let lines = guard.take();
    assert_eq!(lines.last().map(String::as_str), Some("disable"));
    assert!(lines.contains(&"path /system/bin/su".to_string()));
    assert!(!lines.iter().any(|l| l.starts_with("allow ")));
    assert!(!relay.allows(10102));
}

#[test]
fn fr35_the_daemon_hides_su_from_every_app_when_it_starts() {
    let (_router, relay, guard, _said) = daemon();
    let applier = ConfigApplier::new(
        Arc::clone(&relay),
        Box::new(guard.clone()),
        Box::new(|| None),
        |_: &str| {},
    );
    applier.hide_all();
    let lines = guard.take();
    assert_eq!(lines.first().map(String::as_str), Some("reset"));
    assert_eq!(lines.last().map(String::as_str), Some("disable"));
    for path in ["/system/bin/su", "/system/xbin/su", "/sbin/su"] {
        assert!(lines.contains(&format!("path {path}")), "{lines:?}");
    }
    assert!(!lines.iter().any(|l| l.starts_with("allow ")));
}

#[test]
fn fr35_the_kernel_guard_takes_a_config_in_one_write() {
    use dsa_guest_channeld::su_relay::KernelGuard;
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fr35-dsa-su-control");
    std::fs::write(&path, b"").unwrap();
    let guard = KernelGuard { path: path.clone() };
    let lines: Vec<String> = ["reset", "path /sbin/su", "allow 10100", "enable"]
        .iter()
        .map(|line| line.to_string())
        .collect();
    guard.write(&lines).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "reset\npath /sbin/su\nallow 10100\nenable\n"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fr35_an_app_not_installed_sees_no_su() {
    let (router, relay, guard, said) = daemon();
    router.from_host(&config(true, &[("com.missing", RootLevel::Ask)]));
    let lines = guard.take();
    assert!(!lines.iter().any(|l| l.starts_with("allow ")));
    assert!(!relay.allows(0));
    assert!(said
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.contains("com.missing")));
}

#[test]
fn fr35_the_hosts_decision_reaches_the_waiting_su_call() {
    let (router, relay, _guard, _said) = daemon();
    router.from_host(&config(true, &[("com.ask", RootLevel::Ask)]));
    let (request_id, _frame, waiting) = relay.begin("com.ask", 10100, vec!["id".into()], false);
    router.from_host(&dsa_protocol::encode(&Envelope {
        body: Some(Body::SuDecision(SuDecision {
            request_id,
            approved: true,
            reason: String::new(),
        })),
    }));
    assert_eq!(waiting.recv().unwrap(), Outcome::Approved);
}

/// The helper switch's calls: `true` to start the helper, `false` to stop it.
fn switch_recorder() -> (
    dsa_guest_channeld::su_relay::HelperSwitch,
    Arc<Mutex<Vec<bool>>>,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    (
        Box::new(move |on| {
            seen.lock().unwrap().push(on);
            Ok(())
        }),
        calls,
    )
}

fn resolved(enabled: bool) -> RootElevationConfig {
    RootElevationConfig {
        enabled,
        apps: vec![RootApp {
            package: "com.ask".into(),
            uid: 10100,
            level: RootLevel::Ask.into(),
        }],
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn fr35_a_late_helper_receives_the_latest_root_config() {
    use dsa_guest_channeld::su_relay::HelperGuard;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    let path = std::env::temp_dir().join(format!("fr35-late-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (switch, _) = switch_recorder();
    let (to_host, _from_guest) = std::sync::mpsc::channel();
    let mut guard = HelperGuard::with_host(switch, Arc::new(SuRelay::new()), to_host);
    guard.socket = path.clone();
    guard.start_wait = Duration::from_millis(10);
    guard.write_config(&resolved(true), &[]).unwrap();
    guard.write_config(&resolved(false), &[]).unwrap();
    let mut latest = resolved(true);
    latest.apps[0].level = RootLevel::Remember.into();
    guard.write_config(&latest, &[]).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // SAFETY: the address and descriptor live for the duration of the socket calls.
    let listener = unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0);
        let listener = OwnedFd::from_raw_fd(fd);
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (to, from) in address.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
            *to = *from as libc::c_char;
        }
        assert_eq!(
            libc::bind(
                listener.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t
            ),
            0
        );
        assert_eq!(libc::listen(listener.as_raw_fd(), 4), 0);
        listener
    };
    let mut poll = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll and listener are valid for the call.
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
    // SAFETY: the listener and buffer remain valid for both calls.
    let (peer, bytes) = unsafe {
        let peer = libc::accept(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        assert!(peer >= 0);
        let peer = OwnedFd::from_raw_fd(peer);
        let mut bytes = vec![0u8; 4096];
        let n = libc::recv(peer.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), 0);
        assert!(n > 8);
        bytes.truncate(n as usize);
        (peer, bytes)
    };
    assert_eq!(
        dsa_protocol::decode(&bytes[8..]).unwrap().body,
        Some(Body::RootElevationConfig(latest))
    );
    guard.write_config(&resolved(false), &[]).unwrap();
    drop(peer);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fr35_the_daemon_never_writes_the_guard_itself_but_starts_and_stops_the_helper() {
    use dsa_guest_channeld::su_relay::HelperGuard;
    let (switch, calls) = switch_recorder();
    let mut guard = HelperGuard::new(switch);
    guard.socket = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fr35-no-helper");
    guard.start_wait = std::time::Duration::ZERO;
    // The guard's lines alone are refused: the helper takes the whole config.
    assert!(guard.write(&["reset".to_string()]).is_err());
    // On: the helper is started, and with no helper listening the config is not applied.
    assert!(guard.write_config(&resolved(true), &[]).is_err());
    // Off with no helper running: su is hidden already, and the helper is told to stop.
    assert!(guard.write_config(&resolved(false), &[]).is_ok());
    assert_eq!(*calls.lock().unwrap(), vec![true, false]);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn fr35_the_helper_is_handed_the_config_with_the_apps_uids() {
    use dsa_guest_channeld::su_relay::HelperGuard;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    // A short path: a socket address holds about a hundred bytes.
    let path = std::env::temp_dir().join(format!("fr35-helper-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // A seqpacket listener in the helper's place.
    // SAFETY: plain socket calls on a descriptor owned below, and an address that outlives them.
    let listener = unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0);
        let listener = OwnedFd::from_raw_fd(fd);
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (to, from) in address.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
            *to = *from as libc::c_char;
        }
        assert_eq!(
            libc::bind(
                listener.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            ),
            0
        );
        assert_eq!(libc::listen(listener.as_raw_fd(), 4), 0);
        listener
    };
    let (switch, calls) = switch_recorder();
    let mut guard = HelperGuard::new(switch);
    guard.socket = path.clone();
    guard.start_wait = std::time::Duration::from_secs(5);
    let relay = Arc::new(SuRelay::new());
    let applier = ConfigApplier::new(
        Arc::clone(&relay),
        Box::new(guard),
        Box::new(|| Some(PACKAGES.to_string())),
        |_: &str| {},
    );
    let reader = std::thread::spawn(move || {
        let mut buffer = vec![0u8; 4096];
        // SAFETY: the listener above; a buffer that outlives the call.
        unsafe {
            let peer = libc::accept(
                listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert!(peer >= 0);
            let peer = OwnedFd::from_raw_fd(peer);
            let got = libc::recv(
                peer.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            );
            assert!(got > 0);
            buffer.truncate(got as usize);
        }
        buffer
    });
    let host = RootElevationConfig {
        enabled: true,
        apps: vec![RootApp {
            package: "com.ask".into(),
            uid: 0,
            level: RootLevel::Ask.into(),
        }],
    };
    applier.apply(&host);
    let packet = reader.join().unwrap();
    // One frame as the helper reads it: the message's length, its offset, then the message.
    assert!(packet.len() > 8, "the helper was sent {packet:?}");
    let total = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
    let offset = u32::from_be_bytes(packet[4..8].try_into().unwrap());
    assert_eq!((total, offset), (packet.len() - 8, 0));
    match dsa_protocol::decode(&packet[8..]).unwrap().body {
        Some(Body::RootElevationConfig(config)) => assert_eq!(config, resolved(true)),
        other => panic!("the helper was sent {other:?}"),
    }
    assert_eq!(*calls.lock().unwrap(), vec![true]);
    assert!(relay.allows(10100));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fr35_apps_sharing_a_user_id_are_approved_as_one_group() {
    use dsa_guest_channeld::su_relay::package_for_uid;

    let (router, relay, guard, _said) = daemon();
    router.from_host(&config(true, &[("com.shared.one", RootLevel::Ask)]));
    let lines = guard.take();
    assert!(lines.contains(&"allow 10300".to_string()), "{lines:?}");
    assert!(relay.allows(10300));
    assert_eq!(
        package_for_uid(&relay.config(), 10300),
        Some("com.shared.one, com.shared.two".to_string())
    );

    // Setting one app in the shared group to Off hides su from the entire group.
    router.from_host(&config(
        true,
        &[
            ("com.shared.one", RootLevel::Ask),
            ("com.shared.two", RootLevel::Off),
        ],
    ));
    let lines = guard.take();
    assert!(!lines.contains(&"allow 10300".to_string()), "{lines:?}");
    assert!(!relay.allows(10300));
    assert_eq!(package_for_uid(&relay.config(), 10300), None);

    // Both apps allowed again: the group sees su and asks as one.
    router.from_host(&config(
        true,
        &[
            ("com.shared.one", RootLevel::Ask),
            ("com.shared.two", RootLevel::Ask),
        ],
    ));
    assert!(guard.take().contains(&"allow 10300".to_string()));
    assert!(relay.allows(10300));
    assert_eq!(
        package_for_uid(&relay.config(), 10300),
        Some("com.shared.one, com.shared.two".to_string())
    );

    // The host names an app it set to off, even when the other app of the group is allowed,
    // so the whole group is hidden again.
    router.from_host(&config(
        true,
        &[
            ("com.shared.one", RootLevel::Off),
            ("com.shared.two", RootLevel::Remember),
        ],
    ));
    let lines = guard.take();
    assert!(!lines.contains(&"allow 10300".to_string()), "{lines:?}");
    assert!(!relay.allows(10300));
}

#[test]
fn fr35_su_takes_the_forms_root_apps_use() {
    use dsa_guest_channeld::su_args::{parse, SuCommand};

    let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    // A shell, with the options and the target root apps pass and su needs nothing from.
    for shell in [
        &[][..],
        &["-"],
        &["-l"],
        &["--login"],
        &["root"],
        &["0"],
        &["-mm"],
        &["-M", "root"],
        &["--mount-master"],
        &["-p"],
        &["--preserve-environment"],
        &["-s", "/system/bin/sh"],
        &["--shell=/system/bin/sh"],
    ] {
        assert_eq!(parse(&args(shell)), Ok(SuCommand::Shell), "{shell:?}");
    }
    // A command, by every spelling, with its own words kept.
    for (form, command) in [
        (&["-c", "id"][..], "id"),
        (&["-c", "id", "-u"], "id -u"),
        (&["root", "-c", "id"], "id"),
        // Every word after -c is the command's, as root apps written for common su expect.
        (&["-c", "echo", "root"], "echo root"),
        // Android's own form: the command after the target user.
        (&["0", "sh", "-c", "id"], "sh -c id"),
        (&["--command", "id"], "id"),
        (&["--command=id -u"], "id -u"),
        (
            &["-mm", "-c", "mount -o remount,rw /"],
            "mount -o remount,rw /",
        ),
        (&["-", "0", "-c", "id"], "id"),
    ] {
        assert_eq!(
            parse(&args(form)),
            Ok(SuCommand::Command(command.to_string())),
            "{form:?}"
        );
    }
    // Another user, an unknown option, or a missing command is refused.
    for refused in [
        &["shell"][..],
        &["2000", "-c", "id"],
        &["--unknown"],
        &["-c"],
        &["--command="],
        &["-s"],
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused:?}");
    }
    // su -v and -V say su is there without asking for root.
    assert_eq!(
        parse(&args(&["-v"])),
        Ok(SuCommand::Version { code: false })
    );
    assert_eq!(parse(&args(&["-V"])), Ok(SuCommand::Version { code: true }));
}

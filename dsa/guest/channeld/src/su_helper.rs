//! The privileged helper started by init when root elevation is enabled.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, RootElevationConfig};

use crate::su_packet::{peer_uid, Packet};
use crate::su_relay::{package_for_uid, update_guard, Outcome, SuRelay, ANSWER_TIMEOUT};
use crate::su_wire::{self, Frame};

const GUARD: &str = "/sys/kernel/security/dsa_su/control";
const SYSTEM_UID: u32 = 1000;
const MAX_CALLS: usize = 32;

fn reserve_call(active: &AtomicUsize) -> bool {
    // A compare-exchange loop rather than `fetch_update`, which newer toolchains deprecate,
    // or its replacement `try_update`, which is newer than the crates' minimum Rust version.
    let mut current = active.load(Ordering::Acquire);
    while current < MAX_CALLS {
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
    false
}

#[derive(Default)]
struct State {
    config: Option<RootElevationConfig>,
    generation: u64,
    daemon: Option<Arc<Packet>>,
    next_session: u64,
    running: Vec<(u64, u32, UnixStream)>,
}

impl State {
    fn terminate_disallowed(&mut self) {
        let allowed: std::collections::HashSet<u32> = self
            .config
            .as_ref()
            .map(crate::su_relay::allowed_uids)
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.running.retain(|(_, uid, stream)| {
            if allowed.contains(uid) {
                true
            } else {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                false
            }
        });
    }
}

fn guard(config: &RootElevationConfig) -> io::Result<()> {
    update_guard(config, |line| {
        let mut file = std::fs::OpenOptions::new().write(true).open(GUARD)?;
        file.write_all(line.as_bytes())
    })
}

fn init_fd(name: &str) -> io::Result<i32> {
    let value = std::env::var(format!("ANDROID_SOCKET_{name}"))
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "init did not pass a socket"))?;
    value
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid init socket"))
}

/// Serve the sockets init passes to the helper. No host connection is held here.
pub fn run() -> io::Result<()> {
    let app_fd = init_fd("dsa_su")?;
    let relay_fd = init_fd("dsa_su_relay")?;
    if app_fd < 3 || relay_fd < 3 || app_fd == relay_fd {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid init sockets",
        ));
    }
    for fd in [app_fd, relay_fd] {
        // SAFETY: init owns both descriptors and they are open sockets.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: init passes these descriptors as listening sockets owned by this process.
    if unsafe { libc::listen(app_fd, 32) } != 0 || unsafe { libc::listen(relay_fd, 4) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: app_fd is owned by this process and is a stream socket from init.
    let apps = unsafe { UnixListener::from_raw_fd(app_fd) };
    let mut seed = [0; 8];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let relay = Arc::new(SuRelay::with_start(u64::from_ne_bytes(seed)));
    let state = Arc::new(Mutex::new(State::default()));
    {
        let (state, relay) = (Arc::clone(&state), Arc::clone(&relay));
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in apps.incoming() {
                let Ok(mut stream) = stream else { continue };
                if !reserve_call(&active) {
                    let _ = reply(
                        &mut stream,
                        su_wire::STDERR,
                        b"su: too many requests are waiting\n",
                    );
                    let _ = reply(&mut stream, su_wire::EXIT, &1i32.to_be_bytes());
                    continue;
                }
                let (state, relay) = (Arc::clone(&state), Arc::clone(&relay));
                let calls = Arc::clone(&active);
                if std::thread::Builder::new()
                    .name("dsa-su-call".into())
                    .spawn(move || {
                        if let Err(error) = call(stream, state, relay) {
                            eprintln!("su helper: {error}");
                        }
                        calls.fetch_sub(1, Ordering::AcqRel);
                    })
                    .is_err()
                {
                    active.fetch_sub(1, Ordering::AcqRel);
                }
            }
        });
    }
    loop {
        // SAFETY: relay_fd is an init-owned listening seqpacket socket.
        let fd = unsafe {
            libc::accept4(
                relay_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: accept4 returned a new owned descriptor.
        let daemon = Arc::new(unsafe { Packet::from_raw_fd(fd) });
        if daemon.peer_uid()? != SYSTEM_UID {
            continue;
        }
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.daemon = Some(Arc::clone(&daemon));
        }
        while let Ok(bytes) = daemon.recv() {
            match dsa_protocol::decode(&bytes).ok().and_then(|e| e.body) {
                Some(Body::RootElevationConfig(config)) => {
                    relay.apply_config(&config);
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    state.config = None;
                    state.generation = state.generation.wrapping_add(1);
                    if let Err(error) = guard(&config) {
                        state.terminate_disallowed();
                        eprintln!("su helper: could not update the guard: {error}");
                        break;
                    }
                    state.config = Some(config.clone());
                    state.terminate_disallowed();
                    if daemon
                        .send(&dsa_protocol::encode(&Envelope {
                            body: Some(Body::RootElevationConfig(config)),
                        }))
                        .is_err()
                    {
                        break;
                    }
                }
                Some(Body::SuDecision(decision)) => {
                    relay.decide(&decision);
                }
                _ => break,
            }
        }
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.config = None;
            state.daemon = None;
            state.generation = state.generation.wrapping_add(1);
            state.terminate_disallowed();
            if let Err(error) = guard(&RootElevationConfig::default()) {
                eprintln!("su helper: could not clear the guard: {error}");
            }
        }
        relay.cancel_all();
    }
}

fn reply(stream: &mut UnixStream, kind: u8, payload: &[u8]) -> io::Result<()> {
    su_wire::write_frame(
        stream,
        &Frame {
            kind,
            payload: payload.to_vec(),
        },
    )
}

fn call(stream: UnixStream, state: Arc<Mutex<State>>, relay: Arc<SuRelay>) -> io::Result<()> {
    call_with(stream, state, relay, crate::su_process::run)
}

fn call_with(
    mut stream: UnixStream,
    state: Arc<Mutex<State>>,
    relay: Arc<SuRelay>,
    execute: fn(UnixStream, Option<String>, bool, u16, u16) -> io::Result<()>,
) -> io::Result<()> {
    let uid = peer_uid(stream.as_raw_fd())?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let start = su_wire::read_frame(&mut stream)?;
    if start.kind != su_wire::START
        || start.payload.len() < 6
        || start.payload[0] > 1
        || start.payload[1] > 2
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid su invocation",
        ));
    }
    let interactive = start.payload[0] == 1;
    let rows = u16::from_be_bytes([start.payload[2], start.payload[3]]);
    let cols = u16::from_be_bytes([start.payload[4], start.payload[5]]);
    // What the host is asked about, and what the shell runs: the same text in each case, so
    // nothing runs that the approval did not name.
    let (report_command, exec_command) = match start.payload[1] {
        0 if start.payload.len() == 6 => (None, None),
        1 => {
            let cmd = std::str::from_utf8(&start.payload[6..])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "command is not UTF-8"))?
                .to_owned();
            (Some(cmd.clone()), Some(cmd))
        }
        2 if start.payload.len() > 6 => {
            // The whole of the app's standard input, which had ended: its commands are named
            // one after another, and the shell runs that text as its script, taking no more
            // input from the app.
            let script = std::str::from_utf8(&start.payload[6..])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "command is not UTF-8"))?
                .to_owned();
            (Some(script.clone()), Some(script))
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid shell invocation",
            ))
        }
    };
    let (package, generation, daemon) = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        let package = state
            .config
            .as_ref()
            .and_then(|config| package_for_uid(config, uid))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "this app cannot request root",
                )
            })?;
        let daemon = state
            .daemon
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "host is disconnected"))?;
        (package.to_owned(), state.generation, daemon)
    };
    let (id, frame, wait) = relay.begin(
        &package,
        uid,
        report_command
            .as_deref()
            .map(crate::su_args::command_words)
            .unwrap_or_default(),
        interactive,
    );
    if let Err(error) = daemon.send(&frame) {
        relay.expire(id);
        return Err(error);
    }
    let answer = match wait.recv_timeout(ANSWER_TIMEOUT) {
        Ok(answer) => answer,
        Err(_) => {
            relay.expire(id);
            Outcome::Refused("the host did not answer in time".into())
        }
    };
    let reason = match answer {
        Outcome::Approved => None,
        Outcome::Refused(reason) => Some(reason),
    };
    let session_stream = stream.try_clone()?;
    let (still_allowed, session_id) = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let allowed = state.generation == generation
            && state
                .config
                .as_ref()
                .and_then(|config| package_for_uid(config, uid))
                .as_deref()
                == Some(package.as_str());
        if reason.is_none() && allowed {
            state.next_session = state.next_session.wrapping_add(1);
            let sid = state.next_session;
            state.running.push((sid, uid, session_stream));
            (true, Some(sid))
        } else {
            (false, None)
        }
    };
    if reason.is_some() || !still_allowed {
        let reason = reason.unwrap_or_else(|| "the app's root permission changed".into());
        reply(
            &mut stream,
            su_wire::STDERR,
            format!("su: {reason}\n").as_bytes(),
        )?;
        return reply(&mut stream, su_wire::EXIT, &1i32.to_be_bytes());
    }
    stream.set_read_timeout(None)?;
    let mut error_socket = stream.try_clone()?;
    let run_res = execute(stream, exec_command, interactive, rows, cols);
    if let Some(sid) = session_id {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.running.retain(|(id, _, _)| *id != sid);
    }
    if let Err(error) = run_res {
        reply(
            &mut error_socket,
            su_wire::STDERR,
            format!("su: {error}\n").as_bytes(),
        )?;
        reply(&mut error_socket, su_wire::EXIT, &1i32.to_be_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr35_too_many_su_calls_do_not_spawn_unbounded_workers() {
        let active = AtomicUsize::new(0);
        for _ in 0..MAX_CALLS {
            assert!(reserve_call(&active));
        }
        assert!(!reserve_call(&active));
        active.fetch_sub(1, Ordering::AcqRel);
        assert!(reserve_call(&active));
    }

    #[test]
    fn fr35_the_helper_waits_for_the_matching_host_decision() {
        use dsa_protocol::control::{RootApp, RootLevel, SuDecision};
        let mut fd = [-1; 2];
        // SAFETY: fd points to two writable descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fd.as_mut_ptr()) },
            0
        );
        // SAFETY: socketpair returned distinct owned descriptors.
        let (helper, host) = unsafe { (Packet::from_raw_fd(fd[0]), Packet::from_raw_fd(fd[1])) };
        let relay = Arc::new(SuRelay::new());
        // SAFETY: geteuid takes no pointers and always succeeds.
        let uid = unsafe { libc::geteuid() };
        let state = Arc::new(Mutex::new(State {
            config: Some(RootElevationConfig {
                enabled: true,
                apps: vec![RootApp {
                    package: "com.example.ask".into(),
                    uid,
                    level: RootLevel::Ask.into(),
                }],
            }),
            generation: 1,
            daemon: Some(Arc::new(helper)),
            ..Default::default()
        }));
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker_relay = Arc::clone(&relay);
        let worker = std::thread::spawn(move || {
            call_with(
                server,
                state,
                worker_relay,
                |mut stream, command, _, _, _| {
                    assert_eq!(command.as_deref(), Some("id"));
                    reply(&mut stream, su_wire::STDOUT, b"uid=0")?;
                    reply(&mut stream, su_wire::EXIT, &0i32.to_be_bytes())
                },
            )
            .unwrap()
        });
        su_wire::write_frame(
            &mut client,
            &Frame {
                kind: su_wire::START,
                payload: [vec![0, 1, 0, 0, 0, 0], b"id".to_vec()].concat(),
            },
        )
        .unwrap();
        let request = dsa_protocol::decode(&host.recv().unwrap()).unwrap();
        let Some(Body::SuRequest(request)) = request.body else {
            panic!("expected a root request")
        };
        assert_eq!(request.package, "com.example.ask");
        assert_eq!(request.uid, uid);
        relay.decide(&SuDecision {
            request_id: request.request_id,
            approved: true,
            reason: String::new(),
        });
        assert_eq!(su_wire::read_frame(&mut client).unwrap().payload, b"uid=0");
        assert_eq!(
            su_wire::read_frame(&mut client).unwrap().payload,
            0i32.to_be_bytes()
        );
        worker.join().unwrap();
    }

    #[test]
    fn fr35_a_refusal_never_starts_the_requested_command() {
        use dsa_protocol::control::{RootApp, RootLevel, SuDecision};
        let mut fd = [-1; 2];
        // SAFETY: fd points to two writable descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fd.as_mut_ptr()) },
            0
        );
        // SAFETY: socketpair returned distinct owned descriptors.
        let (helper, host) = unsafe { (Packet::from_raw_fd(fd[0]), Packet::from_raw_fd(fd[1])) };
        let relay = Arc::new(SuRelay::new());
        // SAFETY: geteuid takes no pointers and always succeeds.
        let uid = unsafe { libc::geteuid() };
        let state = Arc::new(Mutex::new(State {
            config: Some(RootElevationConfig {
                enabled: true,
                apps: vec![RootApp {
                    package: "com.example.ask".into(),
                    uid,
                    level: RootLevel::Ask.into(),
                }],
            }),
            generation: 1,
            daemon: Some(Arc::new(helper)),
            ..Default::default()
        }));
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker_relay = Arc::clone(&relay);
        let worker = std::thread::spawn(move || {
            call_with(server, state, worker_relay, |_, _, _, _, _| {
                panic!("a refused command was started")
            })
            .unwrap()
        });
        su_wire::write_frame(
            &mut client,
            &Frame {
                kind: su_wire::START,
                payload: [vec![0, 1, 0, 0, 0, 0], b"id".to_vec()].concat(),
            },
        )
        .unwrap();
        let request = dsa_protocol::decode(&host.recv().unwrap()).unwrap();
        let Some(Body::SuRequest(request)) = request.body else {
            panic!("expected a root request")
        };
        relay.decide(&SuDecision {
            request_id: request.request_id,
            approved: false,
            reason: "refused".into(),
        });
        assert_eq!(
            su_wire::read_frame(&mut client).unwrap().kind,
            su_wire::STDERR
        );
        assert_eq!(
            su_wire::read_frame(&mut client).unwrap().payload,
            1i32.to_be_bytes()
        );
        worker.join().unwrap();
    }

    #[test]
    fn fr35_an_approval_after_the_configuration_changes_does_not_run() {
        use dsa_protocol::control::{RootApp, RootLevel, SuDecision};
        let mut fd = [-1; 2];
        // SAFETY: fd points to two writable descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fd.as_mut_ptr()) },
            0
        );
        // SAFETY: socketpair returned distinct owned descriptors.
        let (helper, host) = unsafe { (Packet::from_raw_fd(fd[0]), Packet::from_raw_fd(fd[1])) };
        // SAFETY: geteuid takes no pointers and always succeeds.
        let uid = unsafe { libc::geteuid() };
        let state = Arc::new(Mutex::new(State {
            config: Some(RootElevationConfig {
                enabled: true,
                apps: vec![RootApp {
                    package: "com.example.ask".into(),
                    uid,
                    level: RootLevel::Ask.into(),
                }],
            }),
            generation: 1,
            daemon: Some(Arc::new(helper)),
            ..Default::default()
        }));
        let relay = Arc::new(SuRelay::new());
        let (mut client, server) = UnixStream::pair().unwrap();
        let (worker_state, worker_relay) = (Arc::clone(&state), Arc::clone(&relay));
        let worker = std::thread::spawn(move || {
            call_with(server, worker_state, worker_relay, |_, _, _, _, _| {
                panic!("a stale approval was executed")
            })
            .unwrap()
        });
        su_wire::write_frame(
            &mut client,
            &Frame {
                kind: su_wire::START,
                payload: vec![0, 0, 0, 0, 0, 0],
            },
        )
        .unwrap();
        let request = dsa_protocol::decode(&host.recv().unwrap()).unwrap();
        let Some(Body::SuRequest(request)) = request.body else {
            panic!("expected a root request")
        };
        state.lock().unwrap().generation += 1;
        relay.decide(&SuDecision {
            request_id: request.request_id,
            approved: true,
            reason: String::new(),
        });
        assert_eq!(
            su_wire::read_frame(&mut client).unwrap().kind,
            su_wire::STDERR
        );
        assert_eq!(
            su_wire::read_frame(&mut client).unwrap().payload,
            1i32.to_be_bytes()
        );
        worker.join().unwrap();
    }

    #[test]
    fn fr35_commands_on_standard_input_are_asked_for_and_run_as_approved() {
        use dsa_protocol::control::{RootApp, RootLevel, SuDecision};
        let mut fd = [-1; 2];
        // SAFETY: fd points to two writable descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fd.as_mut_ptr()) },
            0
        );
        // SAFETY: socketpair returned distinct owned descriptors.
        let (helper, host) = unsafe { (Packet::from_raw_fd(fd[0]), Packet::from_raw_fd(fd[1])) };
        let relay = Arc::new(SuRelay::new());
        // SAFETY: geteuid takes no pointers and always succeeds.
        let uid = unsafe { libc::geteuid() };
        let state = Arc::new(Mutex::new(State {
            config: Some(RootElevationConfig {
                enabled: true,
                apps: vec![RootApp {
                    package: "com.example.ask".into(),
                    uid,
                    level: RootLevel::Ask.into(),
                }],
            }),
            generation: 1,
            daemon: Some(Arc::new(helper)),
            ..Default::default()
        }));
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker_relay = Arc::clone(&relay);
        let worker = std::thread::spawn(move || {
            call_with(
                server,
                state,
                worker_relay,
                |mut stream, command, _, _, _| {
                    // Exactly the text the host was asked about, and nothing more.
                    assert_eq!(
                        command.as_deref(),
                        Some(
                            "id -u
echo done
"
                        )
                    );
                    reply(&mut stream, su_wire::STDOUT, b"uid=0")?;
                    reply(&mut stream, su_wire::EXIT, &0i32.to_be_bytes())
                },
            )
            .unwrap()
        });
        su_wire::write_frame(
            &mut client,
            &Frame {
                kind: su_wire::START,
                payload: [
                    vec![0, 2, 0, 0, 0, 0],
                    b"id -u
echo done
"
                    .to_vec(),
                ]
                .concat(),
            },
        )
        .unwrap();
        let request = dsa_protocol::decode(&host.recv().unwrap()).unwrap();
        let Some(Body::SuRequest(request)) = request.body else {
            panic!("expected a root request")
        };
        assert_eq!(
            request.command,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "id -u; echo done".to_string()
            ]
        );
        relay.decide(&SuDecision {
            request_id: request.request_id,
            approved: true,
            reason: String::new(),
        });
        assert_eq!(su_wire::read_frame(&mut client).unwrap().payload, b"uid=0");
        assert_eq!(
            su_wire::read_frame(&mut client).unwrap().payload,
            0i32.to_be_bytes()
        );
        worker.join().unwrap();
    }
}

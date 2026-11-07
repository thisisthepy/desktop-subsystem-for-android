//! Developer mode's auto-connect against a fake adb server that speaks the server protocol on
//! the loopback address.

use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::adb_discovery::fake::FakeAdbServer;
use dsa_host_core::adb_discovery::{AutoConnect, AutoConnectState, AutoConnectWatcher, Wanted};

const PORT: u16 = 58526;
const SERIAL: &str = "127.0.0.1:58526";
const CONNECT: &str = "host:connect:127.0.0.1:58526";
const DISCONNECT: &str = "host:disconnect:127.0.0.1:58526";

fn on() -> Wanted {
    Wanted {
        developer_mode_port: Some(PORT),
        auto_connect: true,
        guest_ready: true,
    }
}

fn developer_mode_off() -> Wanted {
    Wanted {
        developer_mode_port: None,
        ..on()
    }
}

/// Gives the end of a server instance time to reach the host's tracking connection.
fn settle() {
    thread::sleep(Duration::from_millis(200));
}

#[test]
fn fr20_auto_connect_attaches_dsa_when_developer_mode_turns_on() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    assert_eq!(auto.look(on()), AutoConnectState::WaitingForAndroid);
    assert_eq!(server.requests_starting("host:connect:"), vec![CONNECT]);
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    assert_eq!(
        server.requests_starting("host:connect:").len(),
        1,
        "a listed device is not connected again"
    );
    assert!(server
        .requests()
        .iter()
        .any(|request| request == "host:devices"));
}

#[test]
fn fr20_auto_connect_leaves_a_device_the_user_connected_as_it_is() {
    let server = FakeAdbServer::start();
    server.add(SERIAL, "device");
    let mut auto = AutoConnect::new(server.link());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    assert!(server.requests_starting("host:connect:").is_empty());
}

#[test]
fn fr20_auto_connect_waits_for_an_adb_server_and_attaches_when_one_starts() {
    let server = FakeAdbServer::start();
    server.stop();
    let mut auto = AutoConnect::new(server.link());
    assert_eq!(auto.look(on()), AutoConnectState::WaitingForServer);
    server.restart();
    assert_eq!(auto.look(on()), AutoConnectState::WaitingForAndroid);
    assert_eq!(server.requests_starting("host:connect:"), vec![CONNECT]);
}

#[test]
fn fr20_auto_connect_attaches_again_after_the_adb_server_restarts() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);

    // Stopped and seen stopped.
    server.stop();
    settle();
    assert_eq!(auto.look(on()), AutoConnectState::WaitingForServer);
    server.restart();
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    assert_eq!(server.requests_starting("host:connect:").len(), 2);

    // Restarted between two looks: the new server lists nothing, and is a new instance.
    server.restart();
    settle();
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    assert_eq!(server.requests_starting("host:connect:").len(), 3);
}

#[test]
fn fr20_auto_connect_stays_away_after_the_users_disconnect_until_the_server_restarts() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);

    // adb disconnect 127.0.0.1:58526
    server.forget(SERIAL);
    for _ in 0..5 {
        assert_eq!(auto.look(on()), AutoConnectState::Detached);
    }
    assert_eq!(
        server.requests_starting("host:connect:").len(),
        1,
        "no connect while the same server runs"
    );

    server.restart();
    settle();
    assert_eq!(auto.look(on()), AutoConnectState::WaitingForAndroid);
    assert_eq!(server.requests_starting("host:connect:").len(), 2);
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
}

#[test]
fn fr20_a_device_that_never_became_ready_is_not_taken_for_the_users_disconnect() {
    let server = FakeAdbServer::start();
    server.connect_as("offline");
    let mut auto = AutoConnect::new(server.link());
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::WaitingForAndroid);
    // The server gives up on it.
    server.forget(SERIAL);
    server.connect_as("device");
    let mut state = AutoConnectState::Off;
    for _ in 0..4 {
        state = auto.look(on());
        if state == AutoConnectState::Attached {
            break;
        }
    }
    assert_eq!(state, AutoConnectState::Attached);
    assert_eq!(server.requests_starting("host:connect:").len(), 2);
}

#[test]
fn fr20_auto_connect_detaches_dsa_when_developer_mode_turns_off() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    assert_eq!(auto.look(developer_mode_off()), AutoConnectState::Off);
    assert_eq!(
        server.requests_starting("host:disconnect:"),
        vec![DISCONNECT]
    );
    assert!(server.devices().is_empty());

    // Off stays quiet.
    let before = server.connections();
    auto.look(developer_mode_off());
    auto.look(developer_mode_off());
    assert_eq!(server.connections(), before);
}

#[test]
fn fr20_a_new_adb_port_detaches_the_old_one_and_attaches_the_new_one() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    auto.look(on());
    auto.look(on());
    let moved = Wanted {
        developer_mode_port: Some(58527),
        ..on()
    };
    auto.look(moved);
    assert_eq!(
        server.requests_starting("host:disconnect:"),
        vec![DISCONNECT]
    );
    assert_eq!(
        server.requests_starting("host:connect:"),
        vec![CONNECT, "host:connect:127.0.0.1:58527"]
    );
    assert_eq!(auto.look(moved), AutoConnectState::Attached);
}

#[test]
fn fr20_auto_connect_does_not_connect_while_android_is_not_running() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    let stopped = Wanted {
        guest_ready: false,
        ..on()
    };
    assert_eq!(auto.look(stopped), AutoConnectState::WaitingForAndroid);
    assert!(server.requests_starting("host:connect:").is_empty());

    // Attached, then Android stops and the server drops it: not the user's doing.
    auto.look(on());
    assert_eq!(auto.look(on()), AutoConnectState::Attached);
    auto.look(stopped);
    server.forget(SERIAL);
    auto.look(stopped);
    auto.look(on());
    assert_eq!(server.requests_starting("host:connect:").len(), 2);
}

#[test]
fn fr20_nothing_is_sent_when_auto_connect_is_off() {
    let server = FakeAdbServer::start();
    let mut auto = AutoConnect::new(server.link());
    let off = Wanted {
        auto_connect: false,
        ..on()
    };
    for _ in 0..3 {
        assert_eq!(auto.look(off), AutoConnectState::Off);
    }
    // Developer mode turning off with auto-connect off sends nothing either.
    auto.look(Wanted {
        developer_mode_port: None,
        ..off
    });
    assert_eq!(server.connections(), 0);
    assert!(server.requests().is_empty());
}

fn wait_for(watcher: &AutoConnectWatcher, state: AutoConnectState) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if watcher.state() == state {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn fr20_the_watcher_attaches_on_enable_and_detaches_on_disable() {
    let server = FakeAdbServer::start();
    let watcher = AutoConnectWatcher::start(
        server.link(),
        std::sync::Arc::new(|| true),
        Duration::from_millis(20),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(server.connections(), 0, "developer mode starts off");
    watcher.set_developer_mode(Some(PORT));
    assert!(wait_for(&watcher, AutoConnectState::Attached));
    watcher.set_developer_mode(None);
    assert!(wait_for(&watcher, AutoConnectState::Off));
    assert_eq!(
        server.requests_starting("host:disconnect:"),
        vec![DISCONNECT]
    );
}

#[test]
fn fr20_the_watcher_sends_nothing_while_auto_connect_is_off() {
    let server = FakeAdbServer::start();
    let watcher = AutoConnectWatcher::start(
        server.link(),
        std::sync::Arc::new(|| true),
        Duration::from_millis(20),
    )
    .unwrap();
    watcher.set_auto_connect(false);
    watcher.set_developer_mode(Some(PORT));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(watcher.state(), AutoConnectState::Off);
    drop(watcher);
    assert_eq!(server.connections(), 0);
}

#[test]
fn fr20_ending_the_watcher_detaches_dsa() {
    let server = FakeAdbServer::start();
    let watcher = AutoConnectWatcher::start(
        server.link(),
        std::sync::Arc::new(|| true),
        Duration::from_millis(20),
    )
    .unwrap();
    watcher.set_developer_mode(Some(PORT));
    assert!(wait_for(&watcher, AutoConnectState::Attached));
    drop(watcher);
    assert_eq!(
        server.requests_starting("host:disconnect:"),
        vec![DISCONNECT]
    );
}

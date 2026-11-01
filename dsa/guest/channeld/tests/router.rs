//! The daemon's routing between the host and the guest clients that reach it over binder,
//! with in-memory listeners standing in for the clients' callbacks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use dsa_guest_channeld::router::{
    agent_caller, Delivery, Listener, Peer, Router, SendError, AGENT_PROCESS,
};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    AgentConnected, AppInfo, AuthProof, CloseTask, Envelope, GuestBoot, Hello, LaunchApp, Rect,
    TaskFrames, TaskRemoved, TaskUpdated,
};

/// A client's callback that keeps what it is given, and can be made to die.
#[derive(Default)]
struct Recorder {
    frames: Mutex<Vec<Vec<u8>>>,
    dead: AtomicBool,
}

impl Recorder {
    fn frames(&self) -> Vec<Vec<u8>> {
        self.frames.lock().unwrap().clone()
    }
    fn die(&self) {
        self.dead.store(true, Ordering::Relaxed);
    }
}

impl Listener for Recorder {
    fn deliver(&self, frame: &[u8]) -> Delivery {
        if self.dead.load(Ordering::Relaxed) {
            return Delivery::Gone;
        }
        self.frames.lock().unwrap().push(frame.to_vec());
        Delivery::Delivered
    }
}

fn frame(body: Body) -> Vec<u8> {
    dsa_protocol::encode(&Envelope { body: Some(body) })
}

fn task_updated(task_id: i32) -> Vec<u8> {
    frame(Body::TaskUpdated(TaskUpdated {
        task_id,
        bounds: Some(Rect {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        }),
        package_name: "org.example.notes".into(),
        label: "Notes".into(),
        focused: true,
        ..TaskUpdated::default()
    }))
}

const AGENT: Peer = Peer(1200);
const COMPOSER: Peer = Peer(800);

fn router() -> (Router, mpsc::Receiver<Vec<u8>>) {
    let (to_host, host) = mpsc::channel();
    let is_agent = |peer| {
        if peer == AGENT {
            Ok(())
        } else {
            Err("not the agent".to_string())
        }
    };
    (Router::new(to_host, is_agent), host)
}

#[test]
fn fr3_frames_from_the_host_reach_every_listener() {
    let (router, _host) = router();
    let agent = Arc::new(Recorder::default());
    let composer = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent.clone());
    router.subscribe(COMPOSER, composer.clone());

    let close = frame(Body::CloseTask(CloseTask { task_id: 7 }));
    let launch = frame(Body::LaunchApp(LaunchApp {
        package_name: "org.example.notes".into(),
    }));
    router.from_host(&close);
    router.from_host(&launch);

    assert_eq!(agent.frames(), vec![close.clone(), launch.clone()]);
    assert_eq!(composer.frames(), vec![close, launch]);
}

#[test]
fn fr3_task_bounds_go_to_the_host_and_to_the_other_processes() {
    let (router, host) = router();
    let agent = Arc::new(Recorder::default());
    let composer = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent.clone());
    router.subscribe(COMPOSER, composer.clone());

    let updated = task_updated(7);
    let removed = frame(Body::TaskRemoved(TaskRemoved { task_id: 7 }));
    router.from_guest(AGENT, updated.clone()).unwrap();
    router.from_guest(AGENT, removed.clone()).unwrap();

    assert_eq!(host.try_recv().unwrap(), updated);
    assert_eq!(host.try_recv().unwrap(), removed);
    assert_eq!(composer.frames(), vec![updated, removed]);
    assert!(
        agent.frames().is_empty(),
        "the sender does not hear its own messages back"
    );
}

#[test]
fn fr3_other_guest_messages_go_to_the_host_alone() {
    let (router, host) = router();
    let agent = Arc::new(Recorder::default());
    let composer = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent.clone());
    router.subscribe(COMPOSER, composer.clone());

    let frames = frame(Body::TaskFrames(TaskFrames {
        task_id: 7,
        display_id: 3,
    }));
    router.from_guest(COMPOSER, frames.clone()).unwrap();

    assert_eq!(host.try_recv().unwrap(), frames);
    assert!(agent.frames().is_empty());
    assert!(composer.frames().is_empty());
}

#[test]
fn fr3_a_dead_listener_is_dropped() {
    let (router, _host) = router();
    let agent = Arc::new(Recorder::default());
    let composer = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent.clone());
    router.subscribe(COMPOSER, composer.clone());

    agent.die();
    let close = frame(Body::CloseTask(CloseTask { task_id: 7 }));
    router.from_host(&close);

    assert_eq!(router.listener_count(), 1);
    assert_eq!(composer.frames(), vec![close]);
}

#[test]
fn fr3_a_listener_whose_process_died_is_removed() {
    let (router, _host) = router();
    let agent = Arc::new(Recorder::default());
    let id = router.subscribe(AGENT, agent.clone());

    assert!(router.remove(id));
    assert!(!router.remove(id), "already gone");
    router.from_host(&frame(Body::CloseTask(CloseTask { task_id: 7 })));
    assert!(agent.frames().is_empty());
    assert_eq!(router.listener_count(), 0);
}

#[test]
fn fr3_a_listener_that_missed_a_frame_keeps_its_subscription() {
    struct Busy;
    impl Listener for Busy {
        fn deliver(&self, _: &[u8]) -> Delivery {
            Delivery::Failed
        }
    }
    let (router, _host) = router();
    router.subscribe(AGENT, Arc::new(Busy));
    router.from_host(&frame(Body::CloseTask(CloseTask { task_id: 7 })));
    assert_eq!(router.listener_count(), 1);
}

#[test]
fn fr3_a_late_listener_learns_the_tasks_already_there() {
    // A hwcomposer that restarts must still know each task's bounds; the agent sends them
    // only when they change.
    let (router, _host) = router();
    router.from_guest(AGENT, task_updated(7)).unwrap();
    router.from_guest(AGENT, task_updated(8)).unwrap();
    router
        .from_guest(AGENT, frame(Body::TaskRemoved(TaskRemoved { task_id: 8 })))
        .unwrap();
    let newer = task_updated(7);

    let composer = Arc::new(Recorder::default());
    router.subscribe(COMPOSER, composer.clone());
    assert_eq!(composer.frames(), vec![newer]);

    // The agent itself is not told back what it reported.
    let agent = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent.clone());
    assert!(agent.frames().is_empty());
}

#[test]
fn fr3_a_host_that_connects_again_learns_the_live_tasks() {
    let (router, host) = router();
    let updated = task_updated(7);
    let frames = frame(Body::TaskFrames(TaskFrames {
        task_id: 7,
        display_id: 3,
    }));
    router.from_guest(AGENT, updated.clone()).unwrap();
    router.from_guest(COMPOSER, frames.clone()).unwrap();
    router.from_guest(AGENT, task_updated(9)).unwrap();
    router
        .from_guest(AGENT, frame(Body::TaskRemoved(TaskRemoved { task_id: 9 })))
        .unwrap();
    let sent_before: Vec<_> = host.try_iter().collect();
    assert_eq!(sent_before.len(), 4);

    router.host_connected();
    assert_eq!(apart_from_boot(&host), vec![updated, frames]);
}

#[test]
fn fr3_a_client_cannot_send_a_malformed_frame() {
    let (router, host) = router();
    let composer = Arc::new(Recorder::default());
    router.subscribe(COMPOSER, composer.clone());

    let error = router
        .from_guest(AGENT, vec![0xff, 0xff, 0xff])
        .unwrap_err();
    assert!(matches!(error, SendError::Malformed(_)), "{error:?}");
    assert!(host.try_recv().is_err());
    assert!(composer.frames().is_empty());
}

#[test]
fn fr3_a_client_cannot_speak_the_handshake() {
    // Only the daemon proves this guest to the host; a client's Hello or proof would
    // confuse the host's view of the connection.
    let (router, host) = router();
    for body in [
        Body::Hello(Hello::default()),
        Body::AuthProof(AuthProof { mac: vec![0; 32] }),
    ] {
        let error = router.from_guest(AGENT, frame(body)).unwrap_err();
        assert!(matches!(error, SendError::NotAllowed(_)), "{error:?}");
    }
    let error = router
        .from_guest(AGENT, dsa_protocol::encode(&Envelope { body: None }))
        .unwrap_err();
    assert!(matches!(error, SendError::NotAllowed(_)), "{error:?}");
    assert!(host.try_recv().is_err());
}

fn app_info(package: &str, label: &str) -> Vec<u8> {
    frame(Body::AppInfo(AppInfo {
        package_name: package.into(),
        label: label.into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
    }))
}

#[test]
fn fr3_a_host_that_connects_again_learns_the_apps_before_their_tasks() {
    let (router, host) = router();
    let notes = app_info("org.example.notes", "Notes");
    router.from_guest(AGENT, notes.clone()).unwrap();
    let updated = task_updated(7);
    router.from_guest(AGENT, updated.clone()).unwrap();
    // An app's name and icon go to the host alone.
    let composer = Arc::new(Recorder::default());
    router.subscribe(COMPOSER, composer.clone());
    assert_eq!(host.try_iter().count(), 2);

    router.host_connected();
    assert_eq!(apart_from_boot(&host), vec![notes, updated]);
    assert!(!composer
        .frames()
        .contains(&app_info("org.example.notes", "Notes")));

    // Once the app's last task ends, it is forgotten.
    router
        .from_guest(AGENT, frame(Body::TaskRemoved(TaskRemoved { task_id: 7 })))
        .unwrap();
    host.try_iter().count();
    router.host_connected();
    assert!(apart_from_boot(&host).is_empty());
}

/// What the host was sent, without the reports of how far the guest has got.
fn apart_from_boot(host: &mpsc::Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
    host.try_iter()
        .filter(|frame| boot_report(frame).is_none())
        .collect()
}

/// `(boot_completed, agent_connected)`, if the frame is a report of how far the guest has got.
fn boot_report(frame: &[u8]) -> Option<(bool, bool)> {
    match dsa_protocol::decode(frame).unwrap().body {
        Some(Body::GuestBoot(boot)) => Some((boot.boot_completed, boot.agent_connected)),
        _ => None,
    }
}

fn reports(host: &mpsc::Receiver<Vec<u8>>) -> Vec<(bool, bool)> {
    host.try_iter()
        .filter_map(|frame| boot_report(&frame))
        .collect()
}

fn agent_connected() -> Vec<u8> {
    frame(Body::AgentConnected(AgentConnected {}))
}

#[test]
fn fr2_the_host_hears_when_android_has_booted_and_the_agent_has_connected() {
    let (router, host) = router();
    let agent = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent);
    router.host_connected();
    assert_eq!(reports(&host), vec![(false, false)]);

    router.from_guest(AGENT, agent_connected()).unwrap();
    assert_eq!(
        reports(&host),
        vec![(false, true)],
        "only the report goes to the host, not the agent's own message"
    );

    router.boot_completed();
    router.boot_completed();
    assert_eq!(reports(&host), vec![(true, true)], "told once");

    // A host that connects again is told where things stand.
    router.host_connected();
    assert_eq!(reports(&host), vec![(true, true)]);
}

#[test]
fn fr2_the_composer_does_not_count_as_the_agent() {
    let (router, host) = router();
    router.subscribe(COMPOSER, Arc::new(Recorder::default()));
    router.boot_completed();
    router.host_connected();
    assert_eq!(reports(&host), vec![(true, false), (true, false)]);
}

#[test]
fn fr2_only_the_agent_may_say_it_is_connected() {
    let (router, host) = router();
    router.subscribe(COMPOSER, Arc::new(Recorder::default()));
    assert!(matches!(
        router.from_guest(COMPOSER, agent_connected()),
        Err(SendError::NotAllowed(_))
    ));
    assert!(reports(&host).is_empty(), "the host hears nothing of it");

    router.host_connected();
    assert_eq!(reports(&host), vec![(false, false)]);
}

#[test]
fn fr2_the_agent_is_told_by_its_domain_and_its_process() {
    let agent = format!("{AGENT_PROCESS}\0");
    let agent = Some(agent.as_bytes());
    assert_eq!(
        agent_caller(Some("u:r:platform_app:s0:c512,c768"), agent),
        Ok(())
    );
    assert_eq!(agent_caller(Some("u:r:system_app:s0"), agent), Ok(()));

    // The hwcomposer, and an agent whose certificates the package manager no longer trusts.
    assert!(agent_caller(Some("u:r:hal_graphics_composer_default:s0"), agent).is_err());
    assert!(agent_caller(Some("u:r:priv_app:s0:c512,c768"), agent).is_err());
    // Another platform-signed app, and one whose name only starts the same.
    let other = Some(b"com.android.settings\0".as_slice());
    assert!(agent_caller(Some("u:r:system_app:s0"), other).is_err());
    let longer = format!("{AGENT_PROCESS}.helper\0");
    assert!(agent_caller(Some("u:r:platform_app:s0"), Some(longer.as_bytes())).is_err());
    // What cannot be told is not the agent.
    assert!(agent_caller(None, agent).is_err());
    assert!(agent_caller(Some("u:r:platform_app:s0"), None).is_err());
}

#[test]
fn fr2_an_agent_must_subscribe_before_it_says_it_is_connected() {
    let (router, host) = router();
    assert!(matches!(
        router.from_guest(AGENT, agent_connected()),
        Err(SendError::NotAllowed(_))
    ));
    assert!(reports(&host).is_empty());
}

#[test]
fn fr2_a_client_cannot_report_how_far_the_guest_has_got() {
    let (router, host) = router();
    router.subscribe(COMPOSER, Arc::new(Recorder::default()));
    let forged = frame(Body::GuestBoot(GuestBoot {
        boot_completed: true,
        agent_connected: true,
    }));
    assert!(matches!(
        router.from_guest(COMPOSER, forged),
        Err(SendError::NotAllowed(_))
    ));
    assert!(reports(&host).is_empty());
}

#[test]
fn fr2_the_host_hears_when_the_agent_dies() {
    let (router, host) = router();
    let agent = Arc::new(Recorder::default());
    let id = router.subscribe(AGENT, agent);
    router.from_guest(AGENT, agent_connected()).unwrap();
    host.try_iter().count();

    assert!(router.remove(id));
    assert_eq!(reports(&host), vec![(false, false)]);

    // Its process is found dead when a frame cannot be delivered, too.
    let agent = Arc::new(Recorder::default());
    router.subscribe(AGENT, agent.clone());
    router.from_guest(AGENT, agent_connected()).unwrap();
    host.try_iter().count();
    agent.die();
    router.from_host(&frame(Body::CloseTask(CloseTask { task_id: 1 })));
    assert_eq!(reports(&host), vec![(false, false)]);
}

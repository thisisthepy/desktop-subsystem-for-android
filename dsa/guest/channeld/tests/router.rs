//! The daemon's routing between the host and the guest clients that reach it over binder,
//! with in-memory listeners standing in for the clients' callbacks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use dsa_guest_channeld::router::{Delivery, Listener, Peer, Router, SendError};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    AuthProof, CloseTask, Envelope, Hello, LaunchApp, Rect, TaskFrames, TaskRemoved, TaskUpdated,
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
    }))
}

const AGENT: Peer = Peer(1200);
const COMPOSER: Peer = Peer(800);

fn router() -> (Router, mpsc::Receiver<Vec<u8>>) {
    let (to_host, host) = mpsc::channel();
    (Router::new(to_host), host)
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
    assert_eq!(host.try_iter().collect::<Vec<_>>(), vec![updated, frames]);
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

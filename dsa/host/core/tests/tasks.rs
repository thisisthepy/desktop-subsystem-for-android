//! The guest's tasks from end to end: a guest's agent on the control channel, a real
//! session in the service, and a client of the local API, all over the in-memory
//! transports.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{ClientError, HostApiClient, PerUser, UserAndroid};
use dsa_host_core::host_api_conformance::RunningService;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_core::tasks::{Bounds, Task, TaskEvent};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_protocol::host_api::types::ErrorCode;
use dsa_transport::local;
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector};

const WAIT: Duration = Duration::from_secs(10);

/// A guest that only remembers the secret it was started with.
#[derive(Clone, Default)]
struct Guest(Arc<Mutex<Option<BootSecret>>>);

impl GuestLauncher for Guest {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

impl Guest {
    fn secret(&self) -> BootSecret {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(secret) = self.0.lock().unwrap().clone() {
                return secret;
            }
            assert!(Instant::now() < deadline, "the guest was never started");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// The service with one user's real session, the agent's end of its control channel, and
/// a connector to the service's local API.
struct Setup {
    session: Arc<Session>,
    agent: Box<dyn Connection>,
    api: local::MemoryLocalConnector,
    _service: RunningService,
}

fn setup() -> Setup {
    let guest = Guest::default();
    let (listener, mut control) = memory::pair();
    let session = Arc::new(Session::new(
        guest.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
        },
    ));
    let (api_listener, api) = local::pair();
    let for_service = Arc::clone(&session);
    let service = RunningService::start(
        Box::new(api_listener),
        Arc::new(PerUser::new(move |_: &str| {
            Ok(Arc::clone(&for_service) as Arc<dyn UserAndroid>)
        })),
    );
    session.start();
    let agent = connect_agent(&mut control, &guest);
    assert!(session.wait_for(SessionState::Active, WAIT));
    Setup {
        session,
        agent,
        api,
        _service: service,
    }
}

fn connect_agent(control: &mut MemoryConnector, guest: &Guest) -> Box<dyn Connection> {
    let secret = guest.secret();
    let mut agent = control.connect(WAIT).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the agent");
    agent
}

fn client(api: &mut local::MemoryLocalConnector) -> HostApiClient {
    HostApiClient::connect(api, "test client", WAIT).expect("connecting to the service")
}

fn send(agent: &mut Box<dyn Connection>, body: Body) {
    agent
        .send(&dsa_protocol::encode(&Envelope { body: Some(body) }))
        .unwrap();
}

fn updated(task_id: i32, label: &str) -> Body {
    Body::TaskUpdated(control::TaskUpdated {
        task_id,
        bounds: Some(control::Rect {
            left: 0,
            top: 0,
            right: 720,
            bottom: 1280,
        }),
        package_name: format!("com.example.app{task_id}"),
        label: label.into(),
        ..control::TaskUpdated::default()
    })
}

fn task(task_id: i32, label: &str) -> Task {
    Task {
        task_id,
        bounds: Bounds {
            left: 0,
            top: 0,
            right: 720,
            bottom: 1280,
        },
        package_name: format!("com.example.app{task_id}"),
        label: label.into(),
        ..Task::default()
    }
}

#[test]
fn fr3_the_guests_task_messages_reach_a_watching_client() {
    let mut setup = setup();
    let mut stream = client(&mut setup.api).watch_tasks().expect("watching");

    send(&mut setup.agent, updated(1, "Clock"));
    send(&mut setup.agent, updated(2, "Maps"));
    send(
        &mut setup.agent,
        Body::TaskFrames(control::TaskFrames {
            task_id: 1,
            display_id: 3,
        }),
    );
    send(
        &mut setup.agent,
        Body::TaskRemoved(control::TaskRemoved { task_id: 2 }),
    );

    let events: Vec<TaskEvent> = (0..4)
        .map(|_| stream.next(WAIT).unwrap().expect("a task event in time"))
        .collect();
    assert_eq!(
        events,
        vec![
            TaskEvent::Updated(task(1, "Clock")),
            TaskEvent::Updated(task(2, "Maps")),
            TaskEvent::Frames {
                task_id: 1,
                display_id: 3
            },
            TaskEvent::Removed { task_id: 2 },
        ]
    );
}

#[test]
fn fr3_a_compositor_that_connects_late_gets_the_current_tasks() {
    let mut setup = setup();
    let early = setup.session.watch_tasks();
    send(&mut setup.agent, updated(5, "Clock"));
    send(
        &mut setup.agent,
        Body::TaskFrames(control::TaskFrames {
            task_id: 5,
            display_id: 1,
        }),
    );
    for _ in 0..2 {
        early.events.recv_timeout(WAIT).unwrap();
    }

    let stream = client(&mut setup.api).watch_tasks().expect("watching");
    assert_eq!(stream.current().tasks, vec![task(5, "Clock")]);
    assert_eq!(stream.current().frames, vec![(5, 1)]);
}

#[test]
fn fr3_close_task_and_launch_app_from_a_client_reach_the_guest() {
    let mut setup = setup();
    let mut client = client(&mut setup.api);
    client.launch_app("com.example.clock").expect("the launch");
    client.close_task(5).expect("the close");

    let launch = dsa_protocol::decode(&setup.agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        launch.body,
        Some(Body::LaunchApp(control::LaunchApp {
            package_name: "com.example.clock".into()
        }))
    );
    let close = dsa_protocol::decode(&setup.agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        close.body,
        Some(Body::CloseTask(control::CloseTask { task_id: 5 }))
    );
}

#[test]
fn nfr5_a_guest_that_dies_closes_every_task_a_client_watches() {
    let mut setup = setup();
    let mut stream = client(&mut setup.api).watch_tasks().expect("watching");
    send(&mut setup.agent, updated(1, "Clock"));
    stream.next(WAIT).unwrap().expect("the task");

    setup.agent.close();
    assert_eq!(
        stream.next(WAIT).unwrap(),
        Some(TaskEvent::Removed { task_id: 1 })
    );
    assert!(setup.session.wait_for(SessionState::Stopped, WAIT));

    // Asking a stopped Android to launch an app says why it cannot.
    match client(&mut setup.api).launch_app("com.example.clock") {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::NotRunning);
            assert!(detail.contains("dsa start"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
}

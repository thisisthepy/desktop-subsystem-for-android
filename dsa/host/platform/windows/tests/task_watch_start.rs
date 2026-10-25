//! A compositor that starts before the user's Android does: its task windows open once
//! Android runs and reports a task. The service, its named pipe and the user's session are
//! real; only the VM and the guest's agent are fakes.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::FakeRenderer;
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{PerUser, UserAndroid};
use dsa_host_core::host_api_conformance::RunningService;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_host_windows::pipe::{NamedPipeConnector, NamedPipeListener};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

/// Grants the current user too, since CI runs tests in a service session.
const TEST_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;OW)(A;;GRGW;;;AU)";

fn fresh_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "dsa-watch-start-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

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
    fn vm_id(&self) -> Option<String> {
        Some("6d3a1f2e-0b6c-4c1e-9a55-3f0e7d1c2b4a".into())
    }
}

fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn connect_agent(control: &mut MemoryConnector, guest: &Guest) -> Box<dyn Connection> {
    let secret = wait_for("the guest to start", || guest.0.lock().unwrap().clone());
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

#[test]
fn fr3_a_compositor_started_before_android_opens_a_window_once_android_reports_a_task() {
    let guest = Guest::default();
    let (control_listener, mut control) = memory::pair();
    let session = Arc::new(Session::new(
        guest.clone(),
        Box::new(control_listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
        },
    ));
    let name = fresh_name();
    let listener = NamedPipeListener::bind(&name, TEST_SDDL).expect("creating the pipe");
    let for_service = Arc::clone(&session);
    let _service = RunningService::start(
        Box::new(listener),
        Arc::new(PerUser::new(move |_: &str| {
            Ok(Arc::clone(&for_service) as Arc<dyn UserAndroid>)
        })),
    );

    let unbound = || -> Result<Box<dyn Listener>, TransportError> {
        Err(TransportError::Unsupported("no VM in this test".into()))
    };
    let pipe_name = name.clone();
    let compositor = Compositor::with_custom_transport(
        CompositorConfig::default(),
        Arc::new(FakeRenderer::new()),
        Box::new(move || {
            Box::new(NamedPipeConnector {
                name: pipe_name.clone(),
            }) as Box<dyn Connector>
        }),
        Box::new(move |_| unbound()),
        Box::new(move |_| unbound()),
    );
    let windows = compositor.open_windows();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let running = thread::spawn(move || compositor.run(flag));

    // The compositor watches while Android is stopped, for a while.
    assert_eq!(session.state(), SessionState::Stopped);
    thread::sleep(Duration::from_secs(3));

    session.start();
    let mut agent = connect_agent(&mut control, &guest);
    assert!(session.wait_for(SessionState::Active, WAIT));
    thread::sleep(Duration::from_secs(1));
    agent
        .send(&dsa_protocol::encode(&Envelope {
            body: Some(Body::TaskUpdated(control::TaskUpdated {
                task_id: 7,
                bounds: Some(control::Rect {
                    left: 0,
                    top: 0,
                    right: 548,
                    bottom: 974,
                }),
                package_name: "com.android.settings".into(),
                label: "Settings".into(),
                focused: true,
            })),
        }))
        .unwrap();

    let open = wait_for("the task's window", || {
        let open = windows.lock().unwrap().clone();
        (!open.is_empty()).then_some(open)
    });
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].task_id, 7);

    stop.store(true, Ordering::SeqCst);
    running.join().unwrap();
}

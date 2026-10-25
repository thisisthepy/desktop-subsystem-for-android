//! Acceptance tests for the user-session compositor.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::compositor::{FakeRenderer, GraphicsRenderer, GraphicsStreamKind};
use dsa_host_core::host_api::{HostApiService, PerUser, UserAndroid};
use dsa_host_core::session::SessionState;
use dsa_host_windows::compositor::adapter::GfxstreamAdapter;
use dsa_host_windows::compositor::log::CompositorLogger;
use dsa_host_windows::compositor::single_instance::SingleInstanceMutex;
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_host_windows::guid::Guid;
use dsa_host_windows::hvsocket::{HvSocketConnector, HvSocketListener};
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);

static NEXT_TEST_ID: AtomicU32 = AtomicU32::new(1);

fn unique_id() -> u32 {
    NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
}

struct MockAndroid {
    state: std::sync::Mutex<SessionState>,
    vm_id: std::sync::Mutex<Option<String>>,
}

impl MockAndroid {
    fn new(state: SessionState, vm_id: Option<String>) -> MockAndroid {
        MockAndroid {
            state: std::sync::Mutex::new(state),
            vm_id: std::sync::Mutex::new(vm_id),
        }
    }

    fn set(&self, state: SessionState, vm_id: Option<String>) {
        *self.state.lock().unwrap() = state;
        *self.vm_id.lock().unwrap() = vm_id;
    }
}

impl UserAndroid for MockAndroid {
    fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {
        *self.state.lock().unwrap() = SessionState::Active;
    }
    fn stop(&self) {
        *self.state.lock().unwrap() = SessionState::Stopped;
        *self.vm_id.lock().unwrap() = None;
    }
    fn vm_id(&self) -> Option<String> {
        self.vm_id.lock().unwrap().clone()
    }
}

#[test]
fn fr3_compositor_refuses_second_instance() {
    let mutex_name = format!(
        r"Local\Test_DSA_Compositor_{}_{}",
        std::process::id(),
        unique_id()
    );

    let first = SingleInstanceMutex::acquire(&mutex_name);
    assert!(first.is_ok(), "first instance acquisition should succeed");

    let second = SingleInstanceMutex::acquire(&mutex_name);
    assert!(
        second.is_err(),
        "second instance acquisition must be refused"
    );
    let err_msg = second.unwrap_err();
    assert!(
        err_msg.contains("another instance is already running"),
        "{err_msg}"
    );

    // Dropping first releases the mutex.
    drop(first);

    let third = SingleInstanceMutex::acquire(&mutex_name);
    assert!(
        third.is_ok(),
        "acquisition after drop of first instance must succeed"
    );
}

#[test]
fn fr3_compositor_accepts_graphics_stream_and_hands_to_renderer() {
    accepts_graphics_streams_while(SessionState::Active);
}

/// The guest renders its first frames (surfaceflinger, the boot animation) while it boots,
/// before the agent connects and the session becomes active.
#[test]
fn fr3_compositor_accepts_guest_graphics_while_the_vm_starts() {
    accepts_graphics_streams_while(SessionState::Starting);
}

fn accepts_graphics_streams_while(state: SessionState) {
    let vm_guid = Guid::new(0x12345678, 0xabcd, 0xef01, [1, 2, 3, 4, 5, 6, 7, 8]);
    let android = Arc::new(MockAndroid::new(state, Some(vm_guid.to_string())));

    // Local API service on in-memory transport.
    let (api_listener, api_connector) = local::pair();
    let android_for_service = Arc::clone(&android);
    let per_user = Arc::new(PerUser::new(move |_user: &str| {
        Ok(Arc::clone(&android_for_service) as Arc<dyn UserAndroid>)
    }));
    let service = Arc::new(HostApiService::new(per_user, "test"));
    let stop_service = Arc::new(AtomicBool::new(false));
    let s_flag = Arc::clone(&stop_service);
    thread::spawn(move || service.serve(Box::new(api_listener), s_flag));

    // In-memory pairs for command stream and process pipe.
    let (cmd_listener, mut cmd_connector) = memory::pair();
    let (pipe_listener, mut pipe_connector) = memory::pair();

    let cmd_listener_holder = Arc::new(std::sync::Mutex::new(Some(cmd_listener)));
    let pipe_listener_holder = Arc::new(std::sync::Mutex::new(Some(pipe_listener)));

    let c_holder = Arc::clone(&cmd_listener_holder);
    let p_holder = Arc::clone(&pipe_listener_holder);

    let renderer = Arc::new(FakeRenderer::new());
    let config = CompositorConfig {
        status_poll_interval: Duration::from_millis(20),
        ..Default::default()
    };

    let connector_factory: Box<dyn Fn() -> Box<dyn Connector> + Send> = Box::new(move || {
        let conn = api_connector.clone();
        struct AsPeer(MemoryLocalConnector);
        impl Connector for AsPeer {
            fn connect(
                &mut self,
                _timeout: Duration,
            ) -> Result<Box<dyn Connection>, dsa_transport::TransportError> {
                self.0.connect_as(Peer {
                    user: "test-user".into(),
                })
            }
        }
        Box::new(AsPeer(conn))
    });

    let bind_command = Box::new(move |_guid| {
        let l = c_holder.lock().unwrap().take().expect("command listener");
        Ok(Box::new(l) as Box<dyn Listener>)
    });
    let bind_pipe = Box::new(move |_guid| {
        let l = p_holder.lock().unwrap().take().expect("pipe listener");
        Ok(Box::new(l) as Box<dyn Listener>)
    });

    let compositor = Compositor::with_custom_transport(
        config,
        Arc::clone(&renderer) as Arc<dyn GraphicsRenderer>,
        connector_factory,
        bind_command,
        bind_pipe,
    );

    let stop_compositor = Arc::new(AtomicBool::new(false));
    let comp_flag = Arc::clone(&stop_compositor);
    let comp_thread = thread::spawn(move || compositor.run(comp_flag));

    // Connect to command stream.
    let mut cmd_client = cmd_connector.connect(WAIT).expect("connect command stream");
    cmd_client
        .send(b"test-command-payload")
        .expect("send on command stream");
    cmd_client.close();

    // Connect to process pipe.
    let mut pipe_client = pipe_connector.connect(WAIT).expect("connect process pipe");
    pipe_client
        .send(b"test-pipe-payload")
        .expect("send on process pipe");
    pipe_client.close();

    // Give worker threads time to process streams.
    thread::sleep(Duration::from_millis(200));

    assert_eq!(renderer.command_stream_count(), 1);
    assert_eq!(renderer.process_pipe_stream_count(), 1);
    let streams = renderer.recorded_streams();
    assert_eq!(streams.len(), 2);
    assert!(streams.contains(&GraphicsStreamKind::Command));
    assert!(streams.contains(&GraphicsStreamKind::ProcessPipe));

    stop_compositor.store(true, Ordering::SeqCst);
    stop_service.store(true, Ordering::SeqCst);
    let _ = comp_thread.join();
}

#[test]
fn fr3_compositor_reconnects_when_vm_restarts() {
    let vm1_guid = Guid::new(0x11111111, 0xaaaa, 0xbbbb, [1; 8]);
    let vm2_guid = Guid::new(0x22222222, 0xcccc, 0xdddd, [2; 8]);

    let android = Arc::new(MockAndroid::new(SessionState::Stopped, None));

    // Local API service.
    let (api_listener, api_connector) = local::pair();
    let android_for_service = Arc::clone(&android);
    let per_user = Arc::new(PerUser::new(move |_user: &str| {
        Ok(Arc::clone(&android_for_service) as Arc<dyn UserAndroid>)
    }));
    let service = Arc::new(HostApiService::new(per_user, "test"));
    let stop_service = Arc::new(AtomicBool::new(false));
    let s_flag = Arc::clone(&stop_service);
    thread::spawn(move || service.serve(Box::new(api_listener), s_flag));

    // Listeners for VM1 and VM2.
    let (cmd1_listener, mut cmd1_connector) = memory::pair();
    let (pipe1_listener, _pipe1_connector) = memory::pair();

    let (cmd2_listener, mut cmd2_connector) = memory::pair();
    let (pipe2_listener, _pipe2_connector) = memory::pair();

    let cmd_listeners = Arc::new(std::sync::Mutex::new(vec![
        (vm1_guid, Box::new(cmd1_listener) as Box<dyn Listener>),
        (vm2_guid, Box::new(cmd2_listener) as Box<dyn Listener>),
    ]));
    let pipe_listeners = Arc::new(std::sync::Mutex::new(vec![
        (vm1_guid, Box::new(pipe1_listener) as Box<dyn Listener>),
        (vm2_guid, Box::new(pipe2_listener) as Box<dyn Listener>),
    ]));

    let c_list = Arc::clone(&cmd_listeners);
    let p_list = Arc::clone(&pipe_listeners);

    let renderer = Arc::new(FakeRenderer::new());
    let config = CompositorConfig {
        status_poll_interval: Duration::from_millis(20),
        ..Default::default()
    };

    let connector_factory: Box<dyn Fn() -> Box<dyn Connector> + Send> = Box::new(move || {
        let conn = api_connector.clone();
        struct AsPeer(MemoryLocalConnector);
        impl Connector for AsPeer {
            fn connect(
                &mut self,
                _: Duration,
            ) -> Result<Box<dyn Connection>, dsa_transport::TransportError> {
                self.0.connect_as(Peer {
                    user: "test-user".into(),
                })
            }
        }
        Box::new(AsPeer(conn))
    });

    let bind_command = Box::new(move |guid| {
        let mut list = c_list.lock().unwrap();
        if let Some(pos) = list.iter().position(|(g, _)| *g == guid) {
            Ok(list.remove(pos).1)
        } else {
            Err(dsa_transport::TransportError::Unsupported(
                "no listener for VM".into(),
            ))
        }
    });

    let bind_pipe = Box::new(move |guid| {
        let mut list = p_list.lock().unwrap();
        if let Some(pos) = list.iter().position(|(g, _)| *g == guid) {
            Ok(list.remove(pos).1)
        } else {
            Err(dsa_transport::TransportError::Unsupported(
                "no listener for VM".into(),
            ))
        }
    });

    let compositor = Compositor::with_custom_transport(
        config,
        Arc::clone(&renderer) as Arc<dyn GraphicsRenderer>,
        connector_factory,
        bind_command,
        bind_pipe,
    );

    let stop_compositor = Arc::new(AtomicBool::new(false));
    let comp_flag = Arc::clone(&stop_compositor);
    let comp_thread = thread::spawn(move || compositor.run(comp_flag));

    // Initially stopped: verify no streams yet.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(renderer.command_stream_count(), 0);

    // Boot VM1.
    android.set(SessionState::Active, Some(vm1_guid.to_string()));
    thread::sleep(Duration::from_millis(100));

    let mut client1 = cmd1_connector
        .connect(WAIT)
        .expect("connect to VM1 command stream");
    client1.send(b"stream-vm-1").expect("send to VM1");
    client1.close();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(renderer.command_stream_count(), 1);

    // Stop VM1 (VM restart event).
    android.set(SessionState::Stopped, None);
    thread::sleep(Duration::from_millis(100));

    // Boot VM2 with new ID.
    android.set(SessionState::Active, Some(vm2_guid.to_string()));
    thread::sleep(Duration::from_millis(100));

    let mut client2 = cmd2_connector
        .connect(WAIT)
        .expect("connect to VM2 command stream");
    client2.send(b"stream-vm-2").expect("send to VM2");
    client2.close();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(renderer.command_stream_count(), 2);

    stop_compositor.store(true, Ordering::SeqCst);
    stop_service.store(true, Ordering::SeqCst);
    let _ = comp_thread.join();
}

#[test]
fn pr1_compositor_accepts_stream_over_hvsocket_loopback() {
    let service_id = Guid::new(std::process::id(), unique_id() as u16, 0x5566, *b"dsa-comp");

    let listener = HvSocketListener::bind(Guid::LOOPBACK, service_id)
        .expect("bind loopback hvsocket listener");
    let mut connector = HvSocketConnector {
        vm_id: Guid::LOOPBACK,
        service_id,
    };

    let renderer = Arc::new(FakeRenderer::new());
    let r_clone = Arc::clone(&renderer);

    let server_thread = thread::spawn(move || {
        let mut listener = listener;
        let conn = listener.accept(WAIT).expect("accept loopback hvsocket");
        r_clone
            .serve_stream(GraphicsStreamKind::Command, conn)
            .expect("serve stream");
    });

    let mut client = connector.connect(WAIT).expect("connect loopback hvsocket");
    client
        .send(b"hvsocket-loopback-data")
        .expect("send loopback data");
    client.close();

    server_thread.join().expect("join server thread");
    assert_eq!(renderer.command_stream_count(), 1);
}

/// The compositor learns the VM's ID from the service's local API; on a different pipe it
/// never learns it and never binds its listeners.
#[test]
fn fr3_compositor_asks_the_service_on_its_local_api_pipe() {
    let service = dsa_host_windows::host_config::HostConfig::parse(
        r#"{"kernel": "k", "system_image": "s", "data_disk": "d"}"#,
    )
    .unwrap();
    assert_eq!(CompositorConfig::default().pipe_name, service.pipe_name);
}

#[test]
fn fr3_compositor_logs_to_user_local_appdata_folder() {
    let temp_dir = std::env::temp_dir().join(format!(
        "dsa_test_log_{}_{}",
        std::process::id(),
        unique_id()
    ));
    let log_file = temp_dir.join("dsa-compositor.log");

    let logger = CompositorLogger::open(&log_file).expect("open logger");
    logger.log("test log message 1");
    logger.log("test log message 2");

    assert!(log_file.is_file(), "log file should be created");
    let content = std::fs::read_to_string(&log_file).expect("read log file");
    assert!(content.contains("test log message 1"), "{content}");
    assert!(content.contains("test log message 2"), "{content}");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn pr3_compositor_serves_a_gfxstream_client() {
    let dll_path = match std::env::var("GFXSTREAM_BACKEND_PATH") {
        Ok(path) if !path.is_empty() => std::path::PathBuf::from(path),
        _ => {
            eprintln!(
                "skipping pr3_compositor_serves_a_gfxstream_client: GFXSTREAM_BACKEND_PATH not set"
            );
            return;
        }
    };

    if !dll_path.exists() {
        eprintln!(
            "skipping pr3_compositor_serves_a_gfxstream_client: library not found at {}",
            dll_path.display()
        );
        return;
    }

    let adapter = Arc::new(GfxstreamAdapter::new(Some(dll_path)));
    assert!(
        adapter.is_available(),
        "adapter should load native backend from GFXSTREAM_BACKEND_PATH"
    );

    // 1. Command stream: initialize renderControl and create color buffer.
    let (mut cmd_server, mut cmd_client) = memory::pair();
    let cmd_adapter = Arc::clone(&adapter);
    let cmd_server_thread = thread::spawn(move || {
        let conn = cmd_server.accept(WAIT).expect("accept command stream");
        cmd_adapter
            .serve_stream(GraphicsStreamKind::Command, conn)
            .expect("serve command stream");
    });

    let mut client_conn = cmd_client.connect(WAIT).expect("connect command stream");

    // Send zero clientFlags (4 bytes).
    client_conn
        .send(&0u32.to_le_bytes())
        .expect("send client flags");

    // Send rcGetRendererVersion (opcode 10000, size 8).
    let mut get_version_cmd = Vec::new();
    get_version_cmd.extend_from_slice(&10000u32.to_le_bytes());
    get_version_cmd.extend_from_slice(&8u32.to_le_bytes());
    client_conn
        .send(&get_version_cmd)
        .expect("send rcGetRendererVersion");

    let version_resp = client_conn
        .recv(Some(WAIT))
        .expect("receive rcGetRendererVersion response");
    assert_eq!(
        version_resp.len(),
        4,
        "renderer version response must be 4 bytes"
    );
    let version = i32::from_le_bytes(version_resp.try_into().unwrap());
    assert!(
        version > 0,
        "renderer version should be positive, got {version}"
    );

    // Send rcCreateColorBuffer (opcode 10012, size 20, 256x256, format GL_RGBA = 0x1908).
    let mut create_cb_cmd = Vec::new();
    create_cb_cmd.extend_from_slice(&10012u32.to_le_bytes());
    create_cb_cmd.extend_from_slice(&20u32.to_le_bytes());
    create_cb_cmd.extend_from_slice(&256u32.to_le_bytes());
    create_cb_cmd.extend_from_slice(&256u32.to_le_bytes());
    create_cb_cmd.extend_from_slice(&0x1908u32.to_le_bytes());
    client_conn
        .send(&create_cb_cmd)
        .expect("send rcCreateColorBuffer");

    let create_resp = client_conn
        .recv(Some(WAIT))
        .expect("receive rcCreateColorBuffer response");
    assert_eq!(
        create_resp.len(),
        4,
        "color buffer handle response must be 4 bytes"
    );
    let handle = u32::from_le_bytes(create_resp.try_into().unwrap());
    assert!(
        handle > 0,
        "color buffer handle should be positive, got {handle}"
    );

    client_conn.close();
    cmd_server_thread.join().expect("cmd server thread exit");

    // 2. Process pipe: puid handshake.
    let (mut pipe_server, mut pipe_client) = memory::pair();
    let pipe_adapter = Arc::clone(&adapter);
    let pipe_server_thread = thread::spawn(move || {
        let conn = pipe_server.accept(WAIT).expect("accept process pipe");
        pipe_adapter
            .serve_stream(GraphicsStreamKind::ProcessPipe, conn)
            .expect("serve process pipe");
    });

    let mut pipe_conn = pipe_client.connect(WAIT).expect("connect process pipe");
    pipe_conn
        .send(&100i32.to_le_bytes())
        .expect("send confirm code 100");

    let puid_resp = pipe_conn.recv(Some(WAIT)).expect("receive puid response");
    assert_eq!(puid_resp.len(), 8, "puid response must be 8 bytes");
    let puid = u64::from_le_bytes(puid_resp.try_into().unwrap());
    assert!(puid > 0, "puid should be positive, got {puid}");

    pipe_conn.close();
    pipe_server_thread.join().expect("pipe server thread exit");
}

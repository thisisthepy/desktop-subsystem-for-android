//! Acceptance tests for the user-session compositor.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::compositor::{FakeRenderer, GraphicsRenderer, GraphicsStreamKind};
use dsa_host_core::host_api::{HostApiService, PerUser, UserAndroid};
use dsa_host_core::session::SessionState;
use dsa_host_windows::compositor::adapter::{DisplayPresenter, DisplaySink, GfxstreamAdapter};
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
    let log_file = temp_dir.join("dsa-wm.log");

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

    // 3. Display presentation: the backend takes the compositor's sink, and lets it go.
    struct NoWindows;
    impl DisplaySink for NoWindows {
        fn target(&self, _: u32, _: u32, _: u32) -> Option<isize> {
            None
        }
        fn frame(&self, _: u32) {}
    }
    adapter
        .present_displays(Some(Arc::new(NoWindows)))
        .expect("the backend presents displays");
    eprintln!(
        "the renderer's GPU LUID: {:?}",
        DisplayPresenter::adapter_luid(&*adapter)
    );
    adapter
        .present_displays(None)
        .expect("the backend stops presenting displays");
}

/// A guest command stream speaking gfxstream's wire format directly, so a test can send the
/// host exactly the message it wants, including ones a correct guest driver never sends.
struct GfxstreamWire {
    connection: Box<dyn Connection>,
    received: Vec<u8>,
    vulkan_seqno: u32,
}

impl GfxstreamWire {
    const RC_GET_RENDERER_VERSION: u32 = 10000;
    const RC_SET_PUID: u32 = 10033;

    const VK_CREATE_INSTANCE: u32 = 20000;
    const VK_ENUMERATE_PHYSICAL_DEVICES: u32 = 20002;
    const VK_CREATE_DEVICE: u32 = 20011;
    const VK_BIND_BUFFER_MEMORY: u32 = 20028;
    const VK_BIND_IMAGE_MEMORY: u32 = 20029;
    const VK_GET_BUFFER_MEMORY_REQUIREMENTS: u32 = 20030;
    const VK_GET_IMAGE_MEMORY_REQUIREMENTS: u32 = 20031;
    const VK_CREATE_IMAGE_VIEW: u32 = 20057;
    const VK_GET_IMAGE_MEMORY_REQUIREMENTS_2: u32 = 20144;

    fn new(mut connection: Box<dyn Connection>) -> GfxstreamWire {
        // Every stream starts with the guest's client flags.
        connection
            .send(&0u32.to_le_bytes())
            .expect("send client flags");
        GfxstreamWire {
            connection,
            received: Vec::new(),
            vulkan_seqno: 0,
        }
    }

    fn render_control(&mut self, opcode: u32, args: &[u8]) {
        let mut packet = Vec::with_capacity(8 + args.len());
        packet.extend_from_slice(&opcode.to_le_bytes());
        packet.extend_from_slice(&((8 + args.len()) as u32).to_le_bytes());
        packet.extend_from_slice(args);
        self.connection.send(&packet).expect("send renderControl");
    }

    /// Sends one Vulkan command. The host runs with queue-submit-with-commands, so every
    /// Vulkan packet carries the guest process's next sequence number.
    fn vulkan(&mut self, opcode: u32, body: &[u8]) {
        self.vulkan_seqno += 1;
        let mut packet = Vec::with_capacity(12 + body.len());
        packet.extend_from_slice(&opcode.to_le_bytes());
        packet.extend_from_slice(&((12 + body.len()) as u32).to_le_bytes());
        packet.extend_from_slice(&self.vulkan_seqno.to_le_bytes());
        packet.extend_from_slice(body);
        self.connection.send(&packet).expect("send Vulkan command");
    }

    fn read(&mut self, len: usize) -> Vec<u8> {
        while self.received.len() < len {
            let frame = self
                .connection
                .recv(Some(WAIT))
                .expect("the host renderer answers");
            assert!(!frame.is_empty(), "the host renderer closed the stream");
            self.received.extend_from_slice(&frame);
        }
        self.received.drain(..len).collect()
    }

    fn read_u32(&mut self) -> u32 {
        u32::from_le_bytes(self.read(4).try_into().unwrap())
    }

    fn read_u64(&mut self) -> u64 {
        u64::from_le_bytes(self.read(8).try_into().unwrap())
    }

    fn read_be_u64(&mut self) -> u64 {
        u64::from_be_bytes(self.read(8).try_into().unwrap())
    }

    fn renderer_version(&mut self) -> i32 {
        self.render_control(Self::RC_GET_RENDERER_VERSION, &[]);
        self.read_u32() as i32
    }
}

/// Builds a Vulkan command body in gfxstream's marshalling: plain fields little endian,
/// pointer presence and `pNext` sizes big endian.
#[derive(Default)]
struct VkBody(Vec<u8>);

impl VkBody {
    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn f32(mut self, value: f32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    /// Whether a pointer is present: zero for NULL.
    fn ptr(mut self, present: bool) -> Self {
        self.0.extend_from_slice(&u64::from(present).to_be_bytes());
        self
    }
    /// The start of a structure with no `pNext` chain.
    fn s_type(mut self, s_type: u32) -> Self {
        self.0.extend_from_slice(&s_type.to_le_bytes());
        self.0.extend_from_slice(&0u32.to_be_bytes());
        self
    }
    /// An empty list of layer or extension names: the count, then the array's length.
    fn no_strings(mut self) -> Self {
        self.0.extend_from_slice(&0u32.to_le_bytes());
        self.0.extend_from_slice(&0u32.to_be_bytes());
        self
    }
    fn done(self) -> Vec<u8> {
        self.0
    }
}

const VK_SUCCESS: u32 = 0;
const VK_INCOMPLETE: u32 = 5;
const VK_STRUCTURE_TYPE_APPLICATION_INFO: u32 = 0;
const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;
const VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO: u32 = 2;
const VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO: u32 = 3;
const VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO: u32 = 15;
const VK_STRUCTURE_TYPE_IMAGE_MEMORY_REQUIREMENTS_INFO_2: u32 = 1000146001;
const VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2: u32 = 1000146003;

/// Creates a Vulkan instance and device on the host, as the guest driver does, and returns
/// the device's handle.
fn create_vulkan_device(wire: &mut GfxstreamWire) -> u64 {
    let create_instance = VkBody::default()
        .s_type(VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO)
        .u32(0) // flags
        .ptr(true) // pApplicationInfo
        .s_type(VK_STRUCTURE_TYPE_APPLICATION_INFO)
        .ptr(false) // pApplicationName
        .u32(0)
        .ptr(false) // pEngineName
        .u32(0)
        .u32((1 << 22) | (1 << 12)) // Vulkan 1.1
        .no_strings() // layers
        .no_strings() // extensions
        .ptr(false) // pAllocator
        .u64(0) // pInstance
        .done();
    wire.vulkan(GfxstreamWire::VK_CREATE_INSTANCE, &create_instance);
    let instance = wire.read_u64();
    assert_eq!(wire.read_u32(), VK_SUCCESS, "vkCreateInstance");

    let enumerate = VkBody::default()
        .u64(instance)
        .ptr(true)
        .u32(4)
        .ptr(true)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0)
        .done();
    wire.vulkan(GfxstreamWire::VK_ENUMERATE_PHYSICAL_DEVICES, &enumerate);
    wire.read_be_u64();
    let count = wire.read_u32();
    assert!(count >= 1, "the host offers a physical device");
    wire.read_be_u64();
    let physical_device = wire.read_u64();
    for _ in 1..count {
        wire.read_u64();
    }
    let result = wire.read_u32();
    assert!(
        result == VK_SUCCESS || result == VK_INCOMPLETE,
        "vkEnumeratePhysicalDevices: {result}"
    );

    let create_device = VkBody::default()
        .u64(physical_device)
        .s_type(VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO)
        .u32(0) // flags
        .u32(1) // queueCreateInfoCount
        .s_type(VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO)
        .u32(0) // flags
        .u32(0) // queueFamilyIndex
        .u32(1) // queueCount
        .f32(1.0)
        .no_strings() // layers
        .no_strings() // extensions
        .ptr(false) // pEnabledFeatures
        .ptr(false) // pAllocator
        .u64(0) // pDevice
        .done();
    wire.vulkan(GfxstreamWire::VK_CREATE_DEVICE, &create_device);
    let device = wire.read_u64();
    assert_eq!(wire.read_u32(), VK_SUCCESS, "vkCreateDevice");
    assert_ne!(device, 0, "vkCreateDevice returns a device");
    device
}

/// Reads a VkMemoryRequirements: size, alignment and memoryTypeBits.
fn read_memory_requirements(wire: &mut GfxstreamWire) -> (u64, u64, u32) {
    (wire.read_u64(), wire.read_u64(), wire.read_u32())
}

#[test]
fn nfr5_unknown_vulkan_image_is_rejected_without_reaching_the_driver() {
    let dll_path = match std::env::var("GFXSTREAM_BACKEND_PATH") {
        Ok(path) if !path.is_empty() => std::path::PathBuf::from(path),
        _ => {
            eprintln!("skipping: GFXSTREAM_BACKEND_PATH not set");
            return;
        }
    };
    if !dll_path.exists() {
        eprintln!("skipping: library not found at {}", dll_path.display());
        return;
    }
    let adapter = Arc::new(GfxstreamAdapter::new(Some(dll_path)));
    assert!(adapter.is_available(), "the host renderer loads");

    // The guest process the commands belong to, as its process pipe announces it. The pipe
    // stays open for the whole test: closing it ends the process on the host.
    let (mut pipe_server, mut pipe_client) = memory::pair();
    let pipe_adapter = Arc::clone(&adapter);
    let pipe_thread = thread::spawn(move || {
        let conn = pipe_server.accept(WAIT).expect("accept process pipe");
        pipe_adapter
            .serve_stream(GraphicsStreamKind::ProcessPipe, conn)
            .expect("serve process pipe");
    });
    let mut pipe = pipe_client.connect(WAIT).expect("connect process pipe");
    pipe.send(&100i32.to_le_bytes()).expect("send confirm code");
    let puid = u64::from_le_bytes(
        pipe.recv(Some(WAIT))
            .expect("receive puid")
            .try_into()
            .expect("an 8-byte puid"),
    );

    let (mut cmd_server, mut cmd_client) = memory::pair();
    let cmd_adapter = Arc::clone(&adapter);
    let cmd_thread = thread::spawn(move || {
        let conn = cmd_server.accept(WAIT).expect("accept command stream");
        cmd_adapter
            .serve_stream(GraphicsStreamKind::Command, conn)
            .expect("serve command stream");
    });
    let mut wire = GfxstreamWire::new(cmd_client.connect(WAIT).expect("connect command stream"));
    wire.render_control(GfxstreamWire::RC_SET_PUID, &puid.to_le_bytes());
    assert!(wire.renderer_version() > 0, "renderControl answers");

    let device = create_vulkan_device(&mut wire);

    // A guest that lost track of its handles names an image the host never made: NULL, as
    // on the machine where this crashed the renderer inside the GPU driver, and a stale one.
    for image in [0u64, 0x0006_0000_dead_beef] {
        let info = VkBody::default()
            .u64(device)
            .s_type(VK_STRUCTURE_TYPE_IMAGE_MEMORY_REQUIREMENTS_INFO_2)
            .u64(image)
            .s_type(VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2)
            .u64(0)
            .u64(0)
            .u32(0)
            .done();
        wire.vulkan(GfxstreamWire::VK_GET_IMAGE_MEMORY_REQUIREMENTS_2, &info);
        assert_eq!(wire.read_u32(), VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2);
        wire.read(4); // no pNext
        let (size, _, memory_type_bits) = read_memory_requirements(&mut wire);
        assert_eq!(
            (size, memory_type_bits),
            (0, 0),
            "an unknown image {image:#x} has no memory requirements"
        );

        let v1 = VkBody::default()
            .u64(device)
            .u64(image)
            .u64(0)
            .u64(0)
            .u32(0)
            .done();
        wire.vulkan(GfxstreamWire::VK_GET_IMAGE_MEMORY_REQUIREMENTS, &v1);
        let (size, _, memory_type_bits) = read_memory_requirements(&mut wire);
        assert_eq!(
            (size, memory_type_bits),
            (0, 0),
            "vkGetImageMemoryRequirements of image {image:#x}"
        );

        let bind = VkBody::default()
            .u64(device)
            .u64(image)
            .u64(0) // memory
            .u64(0) // memoryOffset
            .done();
        wire.vulkan(GfxstreamWire::VK_BIND_IMAGE_MEMORY, &bind);
        assert_ne!(
            wire.read_u32(),
            VK_SUCCESS,
            "binding memory to image {image:#x}"
        );

        let view = VkBody::default()
            .u64(device)
            .s_type(VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO)
            .u32(0) // flags
            .u64(image)
            .u32(1) // VK_IMAGE_VIEW_TYPE_2D
            .u32(37) // VK_FORMAT_R8G8B8A8_UNORM
            .u32(0)
            .u32(0)
            .u32(0)
            .u32(0)
            .u32(1) // VK_IMAGE_ASPECT_COLOR_BIT
            .u32(0)
            .u32(1)
            .u32(0)
            .u32(1)
            .ptr(false) // pAllocator
            .u64(0) // pView
            .done();
        wire.vulkan(GfxstreamWire::VK_CREATE_IMAGE_VIEW, &view);
        wire.read_u64();
        assert_ne!(wire.read_u32(), VK_SUCCESS, "a view of image {image:#x}");
    }

    // The same for a buffer.
    for buffer in [0u64, 0x0006_0000_dead_beef] {
        let reqs = VkBody::default()
            .u64(device)
            .u64(buffer)
            .u64(0)
            .u64(0)
            .u32(0)
            .done();
        wire.vulkan(GfxstreamWire::VK_GET_BUFFER_MEMORY_REQUIREMENTS, &reqs);
        let (size, _, memory_type_bits) = read_memory_requirements(&mut wire);
        assert_eq!(
            (size, memory_type_bits),
            (0, 0),
            "an unknown buffer {buffer:#x} has no memory requirements"
        );

        let bind = VkBody::default()
            .u64(device)
            .u64(buffer)
            .u64(0) // memory
            .u64(0) // memoryOffset
            .done();
        wire.vulkan(GfxstreamWire::VK_BIND_BUFFER_MEMORY, &bind);
        assert_ne!(
            wire.read_u32(),
            VK_SUCCESS,
            "binding memory to buffer {buffer:#x}"
        );
    }

    // The renderer is still there and still serving this guest.
    assert!(
        wire.renderer_version() > 0,
        "the renderer keeps serving the guest"
    );

    drop(wire);
    cmd_thread.join().expect("command stream ends");
    pipe.close();
    pipe_thread.join().expect("process pipe ends");
}

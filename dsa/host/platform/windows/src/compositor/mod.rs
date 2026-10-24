//! The user-session compositor for Windows.
//!
//! Runs in the user session, listens on Hyper-V sockets for guest graphics streams
//! (ports 5566 and 5567), and delegates accepted streams to a [`GraphicsRenderer`].

pub mod adapter;
pub mod log;
pub mod shutdown;
pub mod single_instance;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::compositor::{
    GraphicsRenderer, GraphicsStreamKind, StreamTracker, GRAPHICS_COMMAND_PORT,
    GRAPHICS_PROCESS_PIPE_PORT,
};
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::session::SessionState;
use dsa_transport::{Connector, Listener, TransportError};

use self::log::CompositorLogger;
use self::single_instance::DEFAULT_MUTEX_NAME;
use crate::guid::Guid;
use crate::hvsocket::RawHvSocketListener;
use crate::pipe::NamedPipeConnector;
use crate::vm_listener::{BindFn, VmBoundListener, VmIdSlot};

/// Configuration for the user-session compositor.
#[derive(Debug, Clone)]
pub struct CompositorConfig {
    /// Local API named pipe name.
    pub pipe_name: String,
    /// vsock port for the command stream (5566).
    pub command_port: u32,
    /// vsock port for the process pipe (5567).
    pub process_pipe_port: u32,
    /// Named mutex for single-instance per session enforcement.
    pub mutex_name: String,
    /// Optional log folder (defaults to `%LOCALAPPDATA%\DSA\logs`).
    pub log_dir: Option<PathBuf>,
    /// How often to poll the local API for VM state updates.
    pub status_poll_interval: Duration,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        CompositorConfig {
            pipe_name: "dsa".into(),
            command_port: GRAPHICS_COMMAND_PORT,
            process_pipe_port: GRAPHICS_PROCESS_PIPE_PORT,
            mutex_name: DEFAULT_MUTEX_NAME.into(),
            log_dir: None,
            status_poll_interval: Duration::from_millis(100),
        }
    }
}

/// The Windows user-session compositor.
pub struct Compositor {
    config: CompositorConfig,
    renderer: Arc<dyn GraphicsRenderer>,
    tracker: Arc<StreamTracker>,
    vm_id_slot: VmIdSlot,
    local_api_connector: Box<dyn Fn() -> Box<dyn Connector> + Send>,
    bind_command: Option<BindFn>,
    bind_process_pipe: Option<BindFn>,
    logger: Option<Arc<CompositorLogger>>,
}

impl Compositor {
    /// Creates a compositor with standard Windows Hyper-V socket binding functions.
    pub fn new(config: CompositorConfig, renderer: Arc<dyn GraphicsRenderer>) -> Compositor {
        let pipe_name = config.pipe_name.clone();
        let cmd_port = config.command_port;
        let proc_port = config.process_pipe_port;

        let local_api_connector: Box<dyn Fn() -> Box<dyn Connector> + Send> = Box::new(move || {
            Box::new(NamedPipeConnector {
                name: pipe_name.clone(),
            })
        });

        let bind_command: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(cmd_port);
            RawHvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        let bind_process_pipe: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(proc_port);
            RawHvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        Compositor {
            config,
            renderer,
            tracker: Arc::new(StreamTracker::new()),
            vm_id_slot: VmIdSlot::default(),
            local_api_connector,
            bind_command: Some(bind_command),
            bind_process_pipe: Some(bind_process_pipe),
            logger: None,
        }
    }

    /// Creates a compositor using custom connector and listener binding functions for testing.
    pub fn with_custom_transport(
        config: CompositorConfig,
        renderer: Arc<dyn GraphicsRenderer>,
        local_api_connector: Box<dyn Fn() -> Box<dyn Connector> + Send>,
        bind_command: BindFn,
        bind_process_pipe: BindFn,
    ) -> Compositor {
        Compositor {
            config,
            renderer,
            tracker: Arc::new(StreamTracker::new()),
            vm_id_slot: VmIdSlot::default(),
            local_api_connector,
            bind_command: Some(bind_command),
            bind_process_pipe: Some(bind_process_pipe),
            logger: None,
        }
    }

    /// Sets an optional file logger.
    pub fn with_logger(mut self, logger: Arc<CompositorLogger>) -> Compositor {
        self.logger = Some(logger);
        self
    }

    /// The stream tracker recording active and total connections.
    pub fn stream_tracker(&self) -> Arc<StreamTracker> {
        Arc::clone(&self.tracker)
    }

    /// The shared slot holding the current VM ID.
    pub fn vm_id_slot(&self) -> VmIdSlot {
        Arc::clone(&self.vm_id_slot)
    }

    /// Runs the compositor until `stop` is set.
    pub fn run(mut self, stop: Arc<AtomicBool>) {
        if let Some(logger) = &self.logger {
            logger.log("dsa-compositor starting");
        }

        let bind_cmd = self.bind_command.take().expect("bind_command present");
        let bind_proc = self
            .bind_process_pipe
            .take()
            .expect("bind_process_pipe present");

        let mut cmd_listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind_cmd);
        let mut proc_listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind_proc);

        // 1. Monitor thread that tracks the VM ID and state via the local API.
        let monitor_stop = Arc::clone(&stop);
        let monitor_slot = Arc::clone(&self.vm_id_slot);
        let monitor_connector_factory = (self.local_api_connector)();
        let poll_interval = self.config.status_poll_interval;
        let monitor_logger = self.logger.clone();

        let monitor_thread = thread::Builder::new()
            .name("dsa-compositor-monitor".into())
            .spawn(move || {
                let mut connector = monitor_connector_factory;
                let mut client_opt: Option<HostApiClient> = None;

                while !monitor_stop.load(Ordering::Relaxed) {
                    let client = match client_opt.as_mut() {
                        Some(c) => c,
                        None => {
                            match HostApiClient::connect(
                                &mut *connector,
                                "dsa-compositor",
                                Duration::from_secs(1),
                            ) {
                                Ok(c) => {
                                    client_opt = Some(c);
                                    client_opt.as_mut().expect("client set")
                                }
                                Err(_) => {
                                    *monitor_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                    thread::sleep(poll_interval);
                                    continue;
                                }
                            }
                        }
                    };

                    match client.status() {
                        Ok(status) => {
                            let new_id = if status.state == SessionState::Active
                                || status.state == SessionState::Dozing
                            {
                                client
                                    .vm_id()
                                    .ok()
                                    .flatten()
                                    .and_then(|id_str| id_str.parse::<Guid>().ok())
                            } else {
                                None
                            };

                            let mut slot = monitor_slot.lock().unwrap_or_else(|e| e.into_inner());
                            if *slot != new_id {
                                if let Some(logger) = &monitor_logger {
                                    logger.log(&format!(
                                        "VM state changed: {:?}, ID: {:?}",
                                        status.state, new_id
                                    ));
                                }
                                *slot = new_id;
                            }
                        }
                        Err(_) => {
                            client_opt = None;
                            *monitor_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        }
                    }

                    thread::sleep(poll_interval);
                }
            })
            .expect("spawning compositor monitor thread");

        // 2. Command stream listener thread.
        let cmd_stop = Arc::clone(&stop);
        let cmd_renderer = Arc::clone(&self.renderer);
        let cmd_tracker = Arc::clone(&self.tracker);
        let cmd_logger = self.logger.clone();

        let cmd_thread = thread::Builder::new()
            .name("dsa-compositor-cmd".into())
            .spawn(move || {
                while !cmd_stop.load(Ordering::Relaxed) {
                    match cmd_listener.accept(Duration::from_millis(100)) {
                        Ok(conn) => {
                            let renderer = Arc::clone(&cmd_renderer);
                            let tracker = Arc::clone(&cmd_tracker);
                            let logger = cmd_logger.clone();

                            let _ = thread::Builder::new().name("dsa-stream-cmd".into()).spawn(
                                move || {
                                    tracker.stream_started(GraphicsStreamKind::Command);
                                    if let Err(e) =
                                        renderer.serve_stream(GraphicsStreamKind::Command, conn)
                                    {
                                        if let Some(log) = logger {
                                            log.log(&format!("error serving command stream: {e}"));
                                        }
                                    }
                                    tracker.stream_ended(GraphicsStreamKind::Command);
                                },
                            );
                        }
                        Err(TransportError::Timeout) => {}
                        Err(e) => {
                            if let Some(log) = &cmd_logger {
                                log.log(&format!("command stream accept error: {e}"));
                            }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .expect("spawning compositor command thread");

        // 3. Process pipe listener thread.
        let proc_stop = Arc::clone(&stop);
        let proc_renderer = Arc::clone(&self.renderer);
        let proc_tracker = Arc::clone(&self.tracker);
        let proc_logger = self.logger.clone();

        let proc_thread = thread::Builder::new()
            .name("dsa-compositor-pipe".into())
            .spawn(move || {
                while !proc_stop.load(Ordering::Relaxed) {
                    match proc_listener.accept(Duration::from_millis(100)) {
                        Ok(conn) => {
                            let renderer = Arc::clone(&proc_renderer);
                            let tracker = Arc::clone(&proc_tracker);
                            let logger = proc_logger.clone();

                            let _ = thread::Builder::new().name("dsa-stream-pipe".into()).spawn(
                                move || {
                                    tracker.stream_started(GraphicsStreamKind::ProcessPipe);
                                    if let Err(e) =
                                        renderer.serve_stream(GraphicsStreamKind::ProcessPipe, conn)
                                    {
                                        if let Some(log) = logger {
                                            log.log(&format!("error serving process pipe: {e}"));
                                        }
                                    }
                                    tracker.stream_ended(GraphicsStreamKind::ProcessPipe);
                                },
                            );
                        }
                        Err(TransportError::Timeout) => {}
                        Err(e) => {
                            if let Some(log) = &proc_logger {
                                log.log(&format!("process pipe accept error: {e}"));
                            }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .expect("spawning compositor process pipe thread");

        // Wait until stop signal.
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
        }

        let _ = monitor_thread.join();
        let _ = cmd_thread.join();
        let _ = proc_thread.join();

        if let Some(logger) = &self.logger {
            logger.log("dsa-compositor stopped cleanly");
        }
    }
}

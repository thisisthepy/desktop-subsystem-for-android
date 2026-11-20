//! The DSA Window Manager (`dsa-wm`), the user-session compositor of Desktop Subsystem for Android.
//!
//! Runs in the user session without a console window, starts the graphics renderer
//! (`dsa-gfx.exe`, which takes the guest's graphics streams on the VM's Hyper-V sockets) and
//! starts it again when it ends, shows one window per Android task with that task's frames,
//! follows the VM across restarts, and exits cleanly on sign-out.
#![windows_subsystem = "windows"]

/// Counts what the window manager's own code allocates, so its memory breakdown can tell that
/// apart from what drivers and the renderer hold in the same heap.
#[cfg(windows)]
#[global_allocator]
static ALLOCATOR: dsa_host_windows::compositor::committed::CountingAllocator =
    dsa_host_windows::compositor::committed::CountingAllocator;

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-wm runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn main() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use dsa_host_core::graphics_state::GraphicsState;
    use dsa_host_core::host_api::HostApiClient;
    use dsa_host_core::renderer::spawn_supervisor;
    use dsa_host_windows::compositor::display_gpu::{adapter_names, display_sources};
    use dsa_host_windows::compositor::graphics_notice::{self, GraphicsNotices};
    use dsa_host_windows::compositor::log::{default_log_file, CompositorLogger};
    use dsa_host_windows::compositor::notifications::{user_language, DsaNews};
    use dsa_host_windows::compositor::shutdown::SignOutListener;
    use dsa_host_windows::compositor::single_instance::SingleInstanceMutex;
    use dsa_host_windows::compositor::{Compositor, CompositorConfig};
    use dsa_host_windows::pipe::NamedPipeConnector;
    use dsa_host_windows::render_gpu::{
        choose, describe_request, render_gpu_setting, CompositorSettings,
    };
    use dsa_host_windows::renderer::client::{
        ProgramLauncher, RendererClient, RendererProcess, ServedByRenderer,
    };
    use dsa_host_windows::vulkan_driver::VulkanDriverSetting;

    // The window manager runs itself with this argument to find which video formats the GPU
    // decodes, so the decoders never load into the long-running process until they are used.
    if std::env::args().any(|arg| arg == dsa_host_windows::video_decoder::PROBE_ARGUMENT) {
        use std::io::Write;
        let report = dsa_host_windows::video_decoder::probe_here();
        let _ = std::io::stdout().write_all(report.as_bytes());
        return;
    }

    // The same for the cameras: Media Foundation and the camera drivers load only into this
    // child until an app opens a camera.
    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|arg| arg == dsa_host_windows::camera_capture::LIST_ARGUMENT)
    {
        use std::io::Write;
        let known = dsa_host_windows::camera_capture::known_links(&args);
        let report = dsa_host_windows::camera_capture::list_here(&known);
        let _ = std::io::stdout().write_all(report.as_bytes());
        return;
    }

    let log_file = default_log_file();
    let logger = CompositorLogger::open(&log_file).ok().map(Arc::new);

    // Windows runs `dsa-wm --notification <uri>` when the user clicks a notification's toast:
    // the click goes to the service, and this process ends without starting another window
    // manager.
    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args
        .iter()
        .position(|arg| arg == dsa_host_windows::compositor::notifications::ARGUMENT)
    {
        let uri = args.get(at + 1).map(String::as_str).unwrap_or("");
        let mut connector = NamedPipeConnector {
            name: CompositorConfig::default().pipe_name,
        };
        if let Err(why) = dsa_host_windows::compositor::notifications::activate(uri, &mut connector)
        {
            if let Some(log) = &logger {
                log.log(&format!("a clicked notification did not open: {why}"));
            }
        }
        return;
    }

    dsa_host_windows::compositor::log::input_trace_to(logger.clone());
    // A panic, and a fatal error in the graphics renderer, say why in the log before the
    // process ends: it has no console.
    dsa_host_windows::compositor::log::log_panics_to(logger.clone());
    // A crash inside a driver or Windows never reaches the panic hook; this writes it too.
    dsa_host_windows::compositor::log::log_crashes_to(logger.clone());
    dsa_host_windows::compositor::log::renderer_log_to(logger.clone());

    let config = CompositorConfig::default();

    // Enforce single instance per user session.
    // No window of its own, yet every frame passes through this process.
    if let Err(why) = dsa_host_windows::scheduling::raise_this_process() {
        if let Some(log) = &logger {
            log.log(&why);
        }
    }

    let _instance_guard = match SingleInstanceMutex::acquire(&config.mutex_name) {
        Ok(guard) => guard,
        Err(why) => {
            if let Some(log) = &logger {
                log.log(&format!("exiting: {why}"));
            }
            return;
        }
    };

    if let Some(log) = &logger {
        log.log(&format!(
            "dsa-wm starting (version {})",
            dsa_host_windows::VERSION
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let _signout = SignOutListener::start(Arc::clone(&stop));

    // The GPU the renderer runs on: the one driving the primary monitor unless the user's
    // settings name another. It is chosen once, before the renderer starts. The settings are
    // the service's; the window manager's own compositor.json counts only when the service
    // cannot be asked.
    let log = |message: &str| {
        if let Some(log) = &logger {
            log.log(message);
        }
    };
    let compositor_settings = dsa_host_windows::paths::UserRoot::current()
        .map(|root| root.compositor_settings())
        .unwrap_or_else(|| std::path::PathBuf::from("compositor.json"));
    let from_service = (|| {
        let mut connector = NamedPipeConnector {
            name: config.pipe_name.clone(),
        };
        let mut client = HostApiClient::connect(
            &mut connector,
            &format!("dsa-wm {}", dsa_host_windows::VERSION),
            std::time::Duration::from_secs(5),
        )
        .map_err(|e| e.to_string())?;
        client
            .settings()
            .map(|report| {
                // The trace switches too, before the first trace line.
                dsa_host_windows::compositor::log::use_trace_settings(
                    report.settings.input_trace,
                    report.settings.resize_trace,
                );
                report.settings
            })
            .map_err(|e| e.to_string())
    })();
    // Which Vulkan the renderer may use; `auto` when the service cannot be asked.
    let vulkan = from_service
        .as_ref()
        .map(|settings| VulkanDriverSetting::parse(&settings.vulkan_driver))
        .unwrap_or_default();
    log(&format!(
        "Vulkan driver setting {}: the graphics renderer says what it runs on once it starts",
        vulkan.name()
    ));
    let (render_gpu, notes) =
        render_gpu_setting(from_service.map(|settings| settings.render_gpu), || {
            CompositorSettings::load(&compositor_settings)
        });
    for note in &notes {
        log(note);
    }
    let settings = CompositorSettings { render_gpu };
    let sources = display_sources().unwrap_or_else(|why| {
        log(&why);
        Vec::new()
    });
    let request = choose(&settings.render_gpu, &sources);
    log(&describe_request(
        &settings.render_gpu,
        &request,
        &adapter_names(),
    ));

    // The renderer runs in a process of its own, dsa-gfx.exe, which this process starts, and
    // starts again whenever it ends: a crash in the renderer or the GPU driver then ends only
    // that process, and the app windows stay with their last frame.
    let ports = (config.command_port, config.process_pipe_port);
    let process: RendererProcess = Arc::default();
    // What each renderer runs on goes to the service, which keeps Android stopped while there
    // is no Vulkan to draw on, and to the user where they must act.
    let news = DsaNews::default();
    let on_graphics = {
        let news = Arc::clone(&news);
        let pipe = config.pipe_name.clone();
        let logger = logger.clone();
        let notices = std::sync::Mutex::new(GraphicsNotices::new(
            dsa_host_windows::paths::UserRoot::current().and_then(|root| {
                root.compositor_settings()
                    .parent()
                    .map(GraphicsNotices::marker_in)
            }),
        ));
        Box::new(move |state: &GraphicsState| {
            if notices
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .should_tell(state)
            {
                if let Some(notice) = graphics_notice::notification(state, user_language()) {
                    news.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(notice);
                }
            }
            // The service is told on a thread of its own: the renderer's connection waits
            // for nothing.
            let state = state.clone();
            let pipe = pipe.clone();
            let logger = logger.clone();
            let _ = std::thread::Builder::new()
                .name("dsa-wm-graphics-report".into())
                .spawn(move || {
                    let mut connector = NamedPipeConnector { name: pipe };
                    let told = HostApiClient::connect(
                        &mut connector,
                        &format!("dsa-wm {}", dsa_host_windows::VERSION),
                        std::time::Duration::from_secs(5),
                    )
                    .and_then(|mut client| client.report_graphics(&state));
                    if let Err(why) = told {
                        if let Some(log) = &logger {
                            log.log(&format!(
                                "the service was not told what the graphics renderer runs on: \
                                 {why}"
                            ));
                        }
                    }
                });
        }) as dsa_host_windows::renderer::client::GraphicsListener
    };
    let client = RendererClient::with_vulkan(
        request,
        vulkan,
        Some(on_graphics),
        ports,
        Arc::clone(&process),
        logger.clone(),
    );
    // Android that crashes is said in a toast, as DSA's own news.
    {
        let news = Arc::clone(&news);
        let tell: Arc<dyn Fn(dsa_host_core::notifications::Notification) + Send + Sync> =
            Arc::new(move |notice| {
                news.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(notice);
            });
        let logger = logger.clone();
        let told: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
            if let Some(log) = &logger {
                log.log(line);
            }
        });
        if let Err(why) = dsa_host_core::guest_crash::watch_in_background(
            Box::new(NamedPipeConnector {
                name: config.pipe_name.clone(),
            }),
            format!("dsa-wm {} crash watch", dsa_host_windows::VERSION),
            Arc::new(user_language),
            tell,
            told,
        ) {
            log(&format!("a crash of Android will not be told: {why}"));
        }
    }
    let mut compositor = Compositor::new(config, Arc::new(ServedByRenderer))
        .without_graphics_streams()
        .with_dsa_news(news)
        .with_display_presenter(client.clone());
    if let Some(log) = logger.clone() {
        compositor = compositor.with_logger(log);
    }
    let supervisor = spawn_supervisor(
        Box::new(ProgramLauncher::installed(process)),
        client.clone(),
        format!("dsa-wm {}", dsa_host_windows::VERSION),
        Arc::clone(&stop),
    );
    if let Err(why) = &supervisor {
        log(&format!("the graphics renderer cannot be started: {why}"));
    }
    let vm_thread = client.follow_vm(compositor.vm_id_slot(), Arc::clone(&stop));
    // The windows' textures are made on the GPU the renderer runs on, which it names once it
    // has started.
    if !client.wait_started(std::time::Duration::from_secs(30)) {
        log(
            "the graphics renderer has not started yet; the task windows' textures are made \
             on the default GPU",
        );
    }

    compositor.run(Arc::clone(&stop));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Ok(supervisor) = supervisor {
        let _ = supervisor.join();
    }
    if let Ok(vm_thread) = vm_thread {
        let _ = vm_thread.join();
    }
}

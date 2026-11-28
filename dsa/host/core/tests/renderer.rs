//! The window manager keeps its graphics renderer process running: it starts it, checks its
//! handshake, hands it its start, answers its frames' requests, and starts it again when it
//! ends, saying why in the log. The renderer here is a thread on the in-memory transport,
//! standing in for `dsa-gfx`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::android_presence::SharedPresence;
use dsa_host_core::renderer::vm_after_asking;
use dsa_host_core::renderer::{
    adopted_line, adopted_started, adoption_refusal, adoption_wait, describe_exit_code,
    describe_signal, open_renderer, restart_line, spawn_supervisor, spawn_supervisor_for, Given,
    HeldTargets, Launched, Pending, RendererChild, RendererHandler, RendererLauncher, RendererLink,
    ServedVm, TextureBook, WindowManagerLeft, WindowManagerLink, ADOPTION_WAIT,
};
use dsa_host_core::session::SessionState;
use dsa_protocol::renderer::types::{
    envelope::Body, FrameDone, Start, Started, Target, TargetRequest,
};
use dsa_protocol::renderer::{decode_body, send};
use dsa_transport::{memory, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// One fake renderer: whether it was told to end, and whether it has.
#[derive(Default)]
struct Life {
    killed: AtomicBool,
    exited: AtomicBool,
    stopped_by_window_manager: AtomicBool,
    targets: Mutex<Vec<Target>>,
}

struct FakeChild {
    pid: u32,
    life: Arc<Life>,
    adopted: bool,
}

impl RendererChild for FakeChild {
    fn id(&self) -> u32 {
        self.pid
    }
    fn exited(&mut self) -> Option<String> {
        self.life
            .exited
            .load(Ordering::SeqCst)
            .then(|| describe_signal(9))
    }
    fn kill(&mut self) {
        self.life.killed.store(true, Ordering::SeqCst);
    }
    fn adopted(&self) -> bool {
        self.adopted
    }
}

/// Starts fake renderers on threads, each of which asks for one frame's texture and then
/// serves until it is killed.
#[derive(Default)]
struct FakeLauncher {
    lives: Arc<Mutex<Vec<Arc<Life>>>>,
    next_pid: Arc<AtomicU32>,
    /// Each renderer is one that kept running after the window manager before ended.
    adopting: bool,
}

fn fake_renderer(connector: &mut dyn Connector, life: Arc<Life>, pid: u32) {
    let Ok(mut connection) = connector.connect(WAIT) else {
        life.exited.store(true, Ordering::SeqCst);
        return;
    };
    let start = open_renderer(&mut *connection, "fake dsa-gfx").expect("the start");
    assert_eq!(start.display_width, 1280);
    let link = WindowManagerLink::new(connection.try_clone_sender().expect("a sender"));
    send(
        &mut *connection,
        Body::Started(Started {
            process_id: pid,
            ..Started::default()
        }),
    )
    .unwrap();
    // The presentation thread: one frame for display 3.
    let asking = Arc::clone(&link);
    let asked_life = Arc::clone(&life);
    let presenter = thread::spawn(move || {
        if let Some(target) = asking.target(3, 640, 480, true, Vec::new()) {
            asking.send(Body::FrameDone(FrameDone {
                display_id: 3,
                texture_id: target.texture_id,
                width: 640,
                height: 480,
            }));
            asked_life.targets.lock().unwrap().push(target);
        }
    });
    while !life.killed.load(Ordering::SeqCst) {
        match connection.recv(Some(Duration::from_millis(20))) {
            Ok(frame) => {
                if let Some(Body::Stop(_)) = link.take(decode_body(&frame).unwrap()) {
                    life.stopped_by_window_manager.store(true, Ordering::SeqCst);
                    break;
                }
            }
            Err(TransportError::Timeout) => {}
            Err(_) => break,
        }
    }
    link.close();
    let _ = presenter.join();
    connection.close();
    life.exited.store(true, Ordering::SeqCst);
}

impl RendererLauncher for FakeLauncher {
    fn launch(&mut self) -> Result<Launched, String> {
        let (mut listener, mut connector) = memory::pair();
        let life = Arc::new(Life::default());
        self.lives.lock().unwrap().push(Arc::clone(&life));
        let pid = 100 + self.next_pid.fetch_add(1, Ordering::SeqCst);
        let running = Arc::clone(&life);
        thread::spawn(move || fake_renderer(&mut connector, running, pid));
        let connection = listener.accept(WAIT).map_err(|e| e.to_string())?;
        Ok(Launched {
            process: Box::new(FakeChild {
                pid,
                life,
                adopted: self.adopting,
            }),
            connection,
        })
    }
}

/// The window manager's side: one window, showing display 3 in texture 0x1234.
#[derive(Default)]
struct Window {
    connected: AtomicUsize,
    disconnected: AtomicUsize,
    frames: Mutex<Vec<(u32, u64)>>,
    log: Mutex<Vec<String>>,
    book: Mutex<TextureBook<isize>>,
}

impl RendererHandler for Window {
    fn start(&self) -> Start {
        Start {
            display_width: 1280,
            display_height: 720,
            ..Start::default()
        }
    }
    fn connected(&self, _: &Started, _: &RendererLink) {
        self.book.lock().unwrap().clear();
        self.connected.fetch_add(1, Ordering::SeqCst);
    }
    fn message(&self, body: Body, link: &RendererLink) {
        match body {
            Body::TargetRequest(request) => {
                let given = self.book.lock().unwrap().give(
                    request.display_id,
                    Some(0x1234),
                    &request.held_textures,
                );
                let (texture_id, handle) = match given {
                    Given::New(id) => (id, 0x55),
                    Given::Same(id) => (id, 0),
                    Given::Drop => (0, 0),
                };
                link.send(Body::Target(Target {
                    request: request.request,
                    drop: given == Given::Drop,
                    texture_id,
                    handle,
                    width: request.width,
                    height: request.height,
                }));
            }
            Body::FrameDone(done) => self
                .frames
                .lock()
                .unwrap()
                .push((done.display_id, done.texture_id)),
            _ => {}
        }
    }
    fn disconnected(&self) {
        self.disconnected.fetch_add(1, Ordering::SeqCst);
    }
    fn log(&self, line: &str) {
        self.log.lock().unwrap().push(line.to_string());
    }
}

#[test]
fn nfr5_a_renderer_that_ends_is_started_again_and_frames_resume() {
    let launcher = FakeLauncher::default();
    let lives = Arc::clone(&launcher.lives);
    let window = Arc::new(Window::default());
    let stop = Arc::new(AtomicBool::new(false));
    let supervisor = spawn_supervisor(
        Box::new(launcher),
        Arc::clone(&window) as Arc<dyn RendererHandler>,
        "dsa-wm test".into(),
        Arc::clone(&stop),
    )
    .unwrap();

    wait_until("the first renderer's frame", || {
        window.frames.lock().unwrap().len() == 1
    });
    let first = Arc::clone(&lives.lock().unwrap()[0]);
    // The texture crossed once, with its handle.
    assert_eq!(first.targets.lock().unwrap()[0].handle, 0x55);

    // The renderer dies. The window manager notices, says why, and starts another, which
    // gets the texture over again and fills it.
    first.killed.store(true, Ordering::SeqCst);
    wait_until("the renderer to start again", || {
        window.connected.load(Ordering::SeqCst) == 2
    });
    wait_until("the second renderer's frame", || {
        window.frames.lock().unwrap().len() == 2
    });
    assert_eq!(window.disconnected.load(Ordering::SeqCst), 1);
    let second = Arc::clone(&lives.lock().unwrap()[1]);
    assert_eq!(second.targets.lock().unwrap()[0].handle, 0x55);
    let log = window.log.lock().unwrap().join("\n");
    assert!(
        log.contains("the graphics renderer (dsa-gfx, pid 100) ended: it was killed (SIGKILL)"),
        "{log}"
    );
    assert!(log.contains("restart 1"), "{log}");

    // The window manager stops: the renderer is told to, and ends.
    stop.store(true, Ordering::SeqCst);
    supervisor.join().unwrap();
    assert!(second.stopped_by_window_manager.load(Ordering::SeqCst));
    assert!(second.exited.load(Ordering::SeqCst));
}

#[test]
fn nfr1_the_renderer_runs_only_while_android_runs() {
    let launcher = FakeLauncher::default();
    let lives = Arc::clone(&launcher.lives);
    let window = Arc::new(Window::default());
    let stop = Arc::new(AtomicBool::new(false));
    let presence = SharedPresence::with_linger(Duration::from_millis(200));
    presence.observe(Some(SessionState::Stopped));
    let supervisor = spawn_supervisor_for(
        Box::new(launcher),
        Arc::clone(&window) as Arc<dyn RendererHandler>,
        "dsa-wm test".into(),
        Arc::clone(&stop),
        Arc::clone(&presence),
    )
    .unwrap();

    // Android is stopped: the one probe starts, says what it runs on, and is told to stop.
    wait_until("the probe to start", || {
        window.connected.load(Ordering::SeqCst) == 1
    });
    let probe = Arc::clone(&lives.lock().unwrap()[0]);
    wait_until("the probe to end", || probe.exited.load(Ordering::SeqCst));
    assert!(probe.stopped_by_window_manager.load(Ordering::SeqCst));
    // No other renderer runs while Android stays stopped.
    thread::sleep(Duration::from_millis(400));
    assert_eq!(
        lives.lock().unwrap().len(),
        1,
        "a renderer ran while Android was stopped"
    );
    let log = window.log.lock().unwrap().join("\n");
    assert!(log.contains("stopped while Android is stopped"), "{log}");

    // Android starts: a renderer starts at once and frames flow.
    presence.observe(Some(SessionState::Starting));
    wait_until("the renderer to start with Android", || {
        window.connected.load(Ordering::SeqCst) == 2
    });
    let running = Arc::clone(&lives.lock().unwrap()[1]);

    // Android stops: after the linger the renderer is told to stop, ends, and is not started
    // again.
    presence.observe(Some(SessionState::Stopped));
    wait_until("the renderer to end", || {
        running.exited.load(Ordering::SeqCst)
    });
    assert!(running.stopped_by_window_manager.load(Ordering::SeqCst));
    thread::sleep(Duration::from_millis(400));
    assert_eq!(lives.lock().unwrap().len(), 2, "it was started again");

    // Android starts again: so does a renderer.
    presence.observe(Some(SessionState::Starting));
    wait_until("a third renderer", || {
        window.connected.load(Ordering::SeqCst) == 3
    });

    stop.store(true, Ordering::SeqCst);
    supervisor.join().unwrap();
}

/// A launcher whose renderer cannot start.
struct Broken(Arc<AtomicUsize>);

impl RendererLauncher for Broken {
    fn launch(&mut self) -> Result<Launched, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("dsa-gfx.exe is not installed beside dsa-wm.exe".into())
    }
}

#[test]
fn nfr5_a_renderer_that_cannot_start_is_tried_again_later_and_says_why() {
    let tries = Arc::new(AtomicUsize::new(0));
    let window = Arc::new(Window::default());
    let stop = Arc::new(AtomicBool::new(false));
    let supervisor = spawn_supervisor(
        Box::new(Broken(Arc::clone(&tries))),
        Arc::clone(&window) as Arc<dyn RendererHandler>,
        "dsa-wm test".into(),
        Arc::clone(&stop),
    )
    .unwrap();
    wait_until("a second try", || tries.load(Ordering::SeqCst) >= 2);
    stop.store(true, Ordering::SeqCst);
    supervisor.join().unwrap();
    let log = window.log.lock().unwrap().join("\n");
    assert!(
        log.contains("could not be started: dsa-gfx.exe is not installed"),
        "{log}"
    );
    // Not a busy loop: two tries take at least the shortest wait.
    assert!(tries.load(Ordering::SeqCst) < 50);
}

#[test]
fn nfr5_an_answer_reaches_the_thread_that_waits_and_a_late_one_is_dropped() {
    let pending: Arc<Pending<u32>> = Arc::new(Pending::new());
    let request = pending.begin();
    let answering = Arc::clone(&pending);
    let answer = thread::spawn(move || answering.complete(request, 7));
    assert_eq!(pending.wait(request, WAIT), Some(7));
    answer.join().unwrap();
    // Nobody answers: the wait gives up.
    let unanswered = pending.begin();
    assert_eq!(pending.wait(unanswered, Duration::from_millis(30)), None);
    pending.complete(unanswered, 8);
    // A closed connection wakes everyone at once.
    let request = pending.begin();
    pending.close();
    assert_eq!(pending.wait(request, WAIT), None);
}

#[test]
fn nfr5_a_texture_crosses_to_the_renderer_once_and_again_after_a_restart() {
    let mut book: TextureBook<isize> = TextureBook::new();
    let first = book.give(3, Some(0x10), &[]);
    let Given::New(a) = first else {
        panic!("expected a new texture, got {first:?}");
    };
    assert_eq!(book.give(3, Some(0x10), &[a]), Given::Same(a));
    // The display takes turns between two textures: each crosses once.
    let second = book.give(3, Some(0x11), &[a]);
    let Given::New(b) = second else {
        panic!("expected a new texture, got {second:?}");
    };
    assert_eq!(book.give(3, Some(0x10), &[a, b]), Given::Same(a));
    assert_eq!(book.give(3, Some(0x11), &[a, b]), Given::Same(b));
    assert_eq!(book.texture_of(3), Some(b));
    // The answer that carried a texture came too late, so the renderer does not hold it: it
    // goes over again.
    assert!(matches!(book.give(3, Some(0x10), &[b]), Given::New(other) if other != a));
    // A display without a window drops its frames.
    assert_eq!(book.give(4, None, &[]), Given::Drop);
    // A new renderer gets every texture again.
    book.clear();
    assert!(matches!(book.give(3, Some(0x11), &[]), Given::New(_)));
}

#[test]
fn nfr5_the_renderer_lets_go_of_a_texture_only_after_it_has_the_next() {
    let mut held: HeldTargets<isize> = HeldTargets::new();
    let target = |texture_id, handle| Target {
        texture_id,
        handle,
        ..Target::default()
    };
    let open = |t: &Target| (t.handle != 0).then_some(t.handle as isize);
    assert_eq!(held.take(3, &target(1, 0x40), open), (Some(0x40), vec![]));
    // The same texture again: no handle comes, and the one held is used.
    assert_eq!(held.take(3, &target(1, 0), open), (Some(0x40), vec![]));
    assert_eq!(held.held(3), vec![1]);
    // Textures beyond what a display keeps go, oldest first, once the newest is open.
    for id in 2..=4 {
        held.take(3, &target(id, 0x40 + id), open);
    }
    assert_eq!(
        held.take(3, &target(5, 0x45), open),
        (Some(0x45), vec![0x40])
    );
    assert_eq!(held.current(3), Some((5, 0x45)));
    // Dropped: every texture held goes.
    let dropped = Target {
        drop: true,
        ..Target::default()
    };
    assert_eq!(held.take(3, &dropped, open).1.len(), 4);
    assert!(held.forget(3).is_empty());
}

#[test]
fn nfr1_a_renderer_keeping_one_texture_closes_the_last_size_once_the_next_is_open() {
    let mut held: HeldTargets<isize> = HeldTargets::keeping(1);
    let target = |texture_id, handle| Target {
        texture_id,
        handle,
        ..Target::default()
    };
    let open = |t: &Target| (t.handle != 0).then_some(t.handle as isize);
    assert_eq!(held.take(3, &target(1, 0x40), open), (Some(0x40), vec![]));
    // The window outgrew its texture: the new one is opened, and only then the old one goes.
    assert_eq!(
        held.take(3, &target(2, 0x41), open),
        (Some(0x41), vec![0x40])
    );
    assert_eq!(held.held(3), vec![2]);
    // A texture that could not be opened takes nothing away.
    assert_eq!(held.take(3, &target(3, 0), open), (None, vec![]));
    assert_eq!(held.held(3), vec![2]);
    // Another display keeps its own.
    assert_eq!(held.take(4, &target(4, 0x50), open), (Some(0x50), vec![]));
    assert_eq!(held.held(3), vec![2]);
    // Asking for none still keeps the one being written.
    let mut least: HeldTargets<isize> = HeldTargets::keeping(0);
    least.take(3, &target(1, 0x40), open);
    assert_eq!(least.held(3), vec![1]);
}

#[test]
fn nfr5_the_log_says_why_the_renderer_ended_and_when_it_starts_again() {
    assert_eq!(
        describe_exit_code(0xC000_0005),
        "it crashed: an access violation (exit code 0xC0000005)"
    );
    assert!(describe_exit_code(1).contains("End task"));
    assert_eq!(describe_exit_code(7), "it exited with code 7");
    assert_eq!(
        describe_signal(11),
        "it crashed: a segmentation fault (SIGSEGV)"
    );
    assert_eq!(describe_signal(42), "it was ended by signal 42");
    assert_eq!(
        restart_line(77, "it aborted (SIGABRT)", Duration::from_millis(500), 3),
        "the graphics renderer (dsa-gfx, pid 77) ended: it aborted (SIGABRT); the windows stay \
         and keep their last frame; starting it again in 0.5 s (restart 3)"
    );
    assert!(restart_line(77, "x", Duration::ZERO, 1).contains("starting it again now"));
}

#[test]
fn nfr2_a_renderer_ends_once_the_vm_it_served_is_gone() {
    let mut served = ServedVm::new();
    // Before any VM, and while the first one runs, it keeps serving.
    assert!(!served.told(None), "no VM yet");
    assert!(!served.told(Some("vm-a")), "the first VM is taken");
    assert!(!served.told(Some("vm-a")), "the same VM again");
    // The VM stops: the renderer ends, so the next VM gets a fresh one.
    assert!(served.told(None), "the VM it served stopped");

    // A VM that is replaced at once by another ends it too.
    let mut served = ServedVm::new();
    assert!(!served.told(Some("vm-a")));
    assert!(
        served.told(Some("vm-b")),
        "another VM took the place of the one it served"
    );
}

/// Plays a window manager on `connection`: answers every frame request for display 3 with a
/// texture `handle` under `texture_id`, until `done`.
fn answer_targets(
    mut connection: Box<dyn dsa_transport::Connection>,
    texture_id: u64,
    handle: u64,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<TargetRequest>> {
    thread::spawn(move || {
        let mut asked = Vec::new();
        while !done.load(Ordering::SeqCst) {
            match connection.recv(Some(Duration::from_millis(20))) {
                Ok(frame) => {
                    if let Ok(Body::TargetRequest(request)) = decode_body(&frame) {
                        send(
                            &mut *connection,
                            Body::Target(Target {
                                request: request.request,
                                texture_id,
                                handle,
                                width: request.width,
                                height: request.height,
                                ..Target::default()
                            }),
                        )
                        .unwrap();
                        asked.push(request);
                    }
                }
                Err(TransportError::Timeout) => {}
                Err(_) => break,
            }
        }
        asked
    })
}

/// Reads what the window manager sends the renderer on `connection` and hands it to `link`,
/// as the renderer's main loop does, until the connection breaks or `done`.
fn renderer_reads(
    mut connection: Box<dyn dsa_transport::Connection>,
    link: Arc<WindowManagerLink>,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            match connection.recv(Some(Duration::from_millis(20))) {
                Ok(frame) => {
                    let _ = link.take(decode_body(&frame).unwrap());
                }
                Err(TransportError::Timeout) => {}
                Err(_) => break,
            }
        }
    })
}

#[test]
fn nfr5_a_renderer_whose_window_manager_was_lost_serves_the_next_one_on_the_same_link() {
    // The first window manager.
    let (mut listener, mut connector) = memory::pair();
    let renderer_side = connector.connect(WAIT).unwrap();
    let window_manager = listener.accept(WAIT).unwrap();
    let link = WindowManagerLink::new(renderer_side.try_clone_sender().unwrap());
    let done = Arc::new(AtomicBool::new(false));
    let first = answer_targets(window_manager, 1, 0x40, Arc::clone(&done));
    let reading = renderer_reads(renderer_side, Arc::clone(&link), Arc::clone(&done));
    let target = link
        .target(3, 640, 480, true, Vec::new())
        .expect("an answer");
    assert_eq!((target.texture_id, target.handle), (1, 0x40));

    // It is ended from outside: its connection breaks without a Stop.
    done.store(true, Ordering::SeqCst);
    first.join().unwrap();
    reading.join().unwrap();
    link.lost();
    assert!(link.is_gone());
    assert!(!link.is_ended(), "the renderer waits to be taken over");
    // Frames meanwhile go nowhere, at once rather than after a timeout.
    let asked_at = Instant::now();
    assert!(link.target(3, 640, 480, true, Vec::new()).is_none());
    assert!(asked_at.elapsed() < Duration::from_millis(500));

    // The next window manager takes the renderer over: the same link, and so every part of
    // the renderer that holds it, now asks the new one.
    let (mut listener, mut connector) = memory::pair();
    let renderer_side = connector.connect(WAIT).unwrap();
    let window_manager = listener.accept(WAIT).unwrap();
    link.attach(renderer_side.try_clone_sender().unwrap());
    assert!(!link.is_gone());
    let done = Arc::new(AtomicBool::new(false));
    let second = answer_targets(window_manager, 1, 0x77, Arc::clone(&done));
    let reading = renderer_reads(renderer_side, Arc::clone(&link), Arc::clone(&done));
    let target = link
        .target(3, 640, 480, true, Vec::new())
        .expect("the new window manager answers");
    assert_eq!((target.texture_id, target.handle), (1, 0x77));
    done.store(true, Ordering::SeqCst);
    assert_eq!(second.join().unwrap().len(), 1);
    reading.join().unwrap();

    // A link closed for good takes no window manager any more.
    link.close();
    let (_listener, mut connector) = memory::pair();
    let renderer_side = connector.connect(WAIT).unwrap();
    link.attach(renderer_side.try_clone_sender().unwrap());
    assert!(link.is_ended() && link.is_gone());
}

#[test]
fn nfr5_only_a_window_manager_that_was_lost_leaves_its_renderer_waiting_to_be_taken_over() {
    assert_eq!(adoption_wait(WindowManagerLeft::Lost), Some(ADOPTION_WAIT));
    assert_eq!(adoption_wait(WindowManagerLeft::Stopped), None);
    assert_eq!(adoption_wait(WindowManagerLeft::VmGone), None);
    // Long enough for the service to start the next window manager, which takes seconds.
    assert!(ADOPTION_WAIT >= Duration::from_secs(30));
}

#[test]
fn nfr5_a_renderer_is_taken_over_only_by_a_window_manager_that_asks_for_the_same_start() {
    let first = Start {
        gpu_rule: 1,
        gpu_luid: vec![1, 2, 3, 4, 5, 6, 7, 8],
        command_port: 5566,
        process_pipe_port: 5567,
        frames_on_gpu: true,
        ..Start::default()
    };
    // The same start: the window manager only restarted.
    assert_eq!(adoption_refusal(&first, &first.clone()), None);
    // A display size is not part of what the renderer runs on.
    let resized = Start {
        display_width: 1920,
        ..first.clone()
    };
    assert_eq!(adoption_refusal(&first, &resized), None);
    for changed in [
        Start {
            gpu_luid: vec![8, 7, 6, 5, 4, 3, 2, 1],
            ..first.clone()
        },
        Start {
            vulkan_driver: 2,
            ..first.clone()
        },
        Start {
            command_port: 6000,
            ..first.clone()
        },
        Start {
            frames_on_gpu: false,
            ..first.clone()
        },
    ] {
        let why = adoption_refusal(&first, &changed).expect("a fresh renderer");
        assert!(
            why.starts_with("the new window manager asks for another")
                || why.starts_with("the new window manager takes"),
            "{why}"
        );
    }
}

#[test]
fn nfr5_a_taken_over_renderer_keeps_its_vm_until_the_new_window_manager_names_one() {
    let mut served = ServedVm::new();
    assert!(!served.told(Some("vm-a")));
    served.adopted();
    // The new window manager has not learned the VM yet: the renderer keeps serving it.
    assert!(served.awaits_vm());
    assert!(!served.told(None), "no VM known yet is not the VM gone");
    assert!(!served.told(None));
    assert!(!served.told(Some("vm-a")), "the same VM: it serves on");
    assert!(!served.awaits_vm());
    // From then on, as before: the VM stops, and the renderer ends.
    assert!(served.told(None), "the VM it served stopped");

    // A taken-over renderer whose VM was replaced meanwhile ends at the first word of it.
    let mut served = ServedVm::new();
    assert!(!served.told(Some("vm-a")));
    served.adopted();
    assert!(served.told(Some("vm-b")));
}

#[test]
fn nfr5_a_service_that_does_not_answer_does_not_end_the_renderer_of_a_running_vm() {
    // The service says the VM runs, then times out once, then answers again.
    let mut told = vm_after_asking(None, Some(Some("vm-a")));
    assert_eq!(told, Some("vm-a"));
    told = vm_after_asking(told, None);
    assert_eq!(told, Some("vm-a"), "no answer keeps the VM");
    let mut served = ServedVm::new();
    assert!(!served.told(told));
    told = vm_after_asking(told, Some(Some("vm-a")));
    assert!(!served.told(told), "the renderer serves on");
    // The service says Android stopped: now the VM is gone, and the renderer ends.
    told = vm_after_asking(told, Some(None));
    assert_eq!(told, None);
    assert!(served.told(told));
}

#[test]
fn nfr5_a_taken_over_renderer_lets_go_of_every_texture_the_lost_window_manager_gave() {
    let mut held: HeldTargets<isize> = HeldTargets::keeping(1);
    let target = |texture_id, handle| Target {
        texture_id,
        handle,
        ..Target::default()
    };
    let open = |t: &Target| (t.handle != 0).then_some(t.handle as isize);
    held.take(3, &target(1, 0x40), open);
    held.take(5, &target(2, 0x50), open);
    assert_eq!(held.displays(), vec![3, 5]);
    // The new window manager numbers its textures from 1 again: had texture 1 been kept, its
    // first frame would have gone into the lost window manager's texture, which nothing shows.
    let released: Vec<isize> = held
        .displays()
        .into_iter()
        .flat_map(|display| held.forget(display))
        .collect();
    assert_eq!(released, vec![0x40, 0x50]);
    assert!(held.displays().is_empty());
    assert_eq!(held.take(3, &target(1, 0x60), open), (Some(0x60), vec![]));
}

#[test]
fn nfr5_a_taken_over_renderer_tells_its_new_window_manager_what_it_runs_on() {
    let first = Started {
        process_id: 42,
        gpu_known: true,
        gpu_name: "Intel(R) Arc(TM) Graphics".into(),
        adapter_luid: vec![1, 0, 0, 0, 0, 0, 0, 0],
        notes: vec!["graphics memory: 256 MiB shared with the guest".into()],
        ..Started::default()
    };
    let again = adopted_started(&first);
    assert_eq!(again.process_id, 42);
    assert_eq!(again.gpu_name, first.gpu_name);
    assert_eq!(again.adapter_luid, first.adapter_luid);
    assert_eq!(again.notes.len(), 1);
    assert!(again.notes[0].starts_with("kept running while the window manager restarted"));
}

#[test]
fn nfr5_the_window_manager_says_it_took_over_a_renderer_that_kept_running() {
    let launcher = FakeLauncher {
        adopting: true,
        ..FakeLauncher::default()
    };
    let lives = Arc::clone(&launcher.lives);
    let window = Arc::new(Window::default());
    let stop = Arc::new(AtomicBool::new(false));
    let supervisor = spawn_supervisor(
        Box::new(launcher),
        Arc::clone(&window) as Arc<dyn RendererHandler>,
        "dsa-wm test".into(),
        Arc::clone(&stop),
    )
    .unwrap();
    wait_until("the renderer's frame", || {
        window.frames.lock().unwrap().len() == 1
    });
    let log = window.log.lock().unwrap().join("\n");
    assert!(log.contains(&adopted_line(100)), "{log}");
    assert!(log.contains("this window manager took it over"), "{log}");
    stop.store(true, Ordering::SeqCst);
    supervisor.join().unwrap();
    assert!(lives.lock().unwrap()[0]
        .stopped_by_window_manager
        .load(Ordering::SeqCst));
}

//! Resizing a real app's window on the real image, as a user's machine does it, with the host
//! renderer on software Vulkan (Mesa Lavapipe).
//!
//! The service boots the image under crosvm, with the window manager and the devices
//! service, and opens Settings, an app that lays itself out again at every size and is
//! recreated when its window crosses one of Android's size classes. A host accessibility
//! client watches the task the whole time, as one does on most desktops (a screen reader, a
//! password manager, the text cursor indicator): reading the tree of an app that is busy
//! laying itself out is slow, and while the agent read it on the thread that also takes the
//! host's requests, each new size waited behind it for up to a second and a half. The test
//! then gives the window [`SIZES`] sizes, each twice, [`STEP`] apart, the way the window
//! manager does: live first, then once more (not live) when the guest reports the task at
//! that size, which reconfigures the task for it. No size follows itself ([`size_at`]): the
//! window manager never asks for the size a task has, which changes nothing, so no frame of
//! it comes. A size the guest keeps within the task's limits is waited for at the size the
//! guest gives ([`guest_choice`]), as the window manager takes it.
//!
//! For each resize it measures, on the host's clock, how long the guest took to report the
//! task at the new size (`reported`): the request's way to the agent, its wait for the
//! agent's thread, and the agent's change to the task. From the guest's resize trace it reads
//! how long the request waited for the agent's thread (`queued`), how long the task took to
//! take the size (`bounds`) and how long the app then took until the hwcomposer posted its
//! first frame of that size (`since_bounds`). The frame time is their sum. A resize whose
//! frame never came in [`FRAME_WAIT`] fails the check, as does a 95th percentile of
//! `reported` above [`REPORTED_BUDGET`], of `queued` above [`QUEUED_BUDGET`], or of the frame
//! time above [`FRAME_BUDGET`]. The numbers go to `resize.md` for the job's summary, with
//! the agent's lines about work that held its thread.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM`, `DSA_WIDGET_TEST_IMAGE` and, when the
//! image folder carries no kernel, `DSA_WIDGET_TEST_KERNEL`. It needs `/dev/kvm` and
//! `/dev/vhost-vsock`. What it read stays in `/tmp/resize-test-<pid>` for the job to keep.
#![cfg(target_os = "linux")]

mod boot_check;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boot_check::{lock, Process as Service, Watchdog};

use dsa_host_core::compositor::window_sizing::guest_choice;
use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::tasks::Task;
use dsa_host_unix::socket::UnixLocalConnector;

/// The whole check, boot included.
const BUDGET: Duration = Duration::from_secs(900);
/// How long past its budget the check may run before it stops what it started and ends.
const OVERRUN: Duration = Duration::from_secs(60);
/// The app: a real one, which relays out at every size and is recreated across size classes.
const APP: &str = "com.android.settings";
/// How long the app has to get a task and its frames a window.
const APP_WAIT: Duration = Duration::from_secs(120);
/// The window's sizes, as parts of the largest the guest can draw: across Android's size
/// classes in both directions, so the app is relaid out at each and recreated at some.
const SIZES: [(f64, f64); 8] = [
    (0.45, 0.60),
    (0.80, 0.70),
    (0.35, 0.85),
    (0.95, 0.90),
    (0.60, 0.45),
    (0.40, 0.95),
    (0.75, 0.55),
    (0.90, 0.65),
];
/// How long the accessibility client may wait for the app's first tree: long enough for the
/// guest to bind its accessibility service right after boot, and to ask again once.
const TREE_WAIT: Duration = Duration::from_secs(60);
/// The time between two resizes, as the owner's measurement on a PC used.
const STEP: Duration = Duration::from_millis(1500);
/// How long a resize's frame may take before the resize counts as never shown.
const FRAME_WAIT: Duration = Duration::from_secs(5);
/// The most the guest may take, at the 95th percentile, to report a task at its new size: on
/// any machine it is a message each way and a change of the task's bounds.
const REPORTED_BUDGET: Duration = Duration::from_millis(150);
/// The most a new size may wait for the agent's thread, at the 95th percentile.
const QUEUED_BUDGET: f64 = 50.0;
/// The most the frame of a new size may take, at the 95th percentile, on the runner's software
/// Vulkan with two processors. A PC's GPU must meet the requirement's 200 ms; this is the
/// runner's equivalent, past which a regression like the one this check guards against (a new
/// size waiting a second for the agent) shows plainly.
const FRAME_BUDGET: f64 = 1000.0;

/// Which of [`SIZES`] the window takes at `step`: each in turn, then each again the other way,
/// starting from the one before the last, so every size follows another one, never itself.
/// Asking for the size a task has changes nothing in the guest, and no frame of it comes.
fn size_at(step: usize) -> usize {
    let count = SIZES.len();
    if step < count {
        step
    } else {
        (2 * count - 2 - (step - count)) % count
    }
}

fn scratch() -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("resize-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn client(socket: &Path, wait: Duration) -> HostApiClient {
    let mut connector = UnixLocalConnector {
        path: socket.to_path_buf(),
    };
    let deadline = Instant::now() + wait;
    loop {
        match HostApiClient::connect(&mut connector, "resize-test", Duration::from_secs(30)) {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("the service did not answer on {}: {why}", socket.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Fails the check with what the service and the window manager printed and the guest log's
/// last lines.
fn fail(why: String, service: &Service, log: &Mutex<Vec<String>>) -> ! {
    let tail = {
        let log = lock(log);
        let _ = std::fs::write(service.log.with_file_name("guest.log"), log.join("\n"));
        log[log.len().saturating_sub(200)..].join("\n")
    };
    let compositor =
        std::fs::read_to_string(service.log.with_file_name("compositor.log")).unwrap_or_default();
    panic!(
        "{why}\n--- the service's output ---\n{}\n--- the window manager's output ---\n{compositor}\n--- the guest log's last lines ---\n{tail}",
        service.log(),
    )
}

// ---------------------------------------------------------------- reading the resize trace

/// One line of the guest's resize trace: its task, size, stage and `key=value` details.
#[derive(Debug, Clone, PartialEq)]
struct TraceLine {
    task: i32,
    size: (u32, u32),
    stage: String,
    details: HashMap<String, String>,
}

impl TraceLine {
    fn number(&self, key: &str) -> Option<f64> {
        self.details.get(key)?.parse().ok()
    }
}

/// The resize trace line in a log line, if it holds one.
fn trace_line(line: &str) -> Option<TraceLine> {
    let start = line.find("resize-trace ")?;
    let mut task = None;
    let mut size = None;
    let mut stage = None;
    let mut details = HashMap::new();
    for word in line[start + "resize-trace ".len()..].split_whitespace() {
        let Some((key, value)) = word.split_once('=') else {
            continue;
        };
        match key {
            "task" => task = value.parse().ok(),
            "size" => {
                let (width, height) = value.split_once('x')?;
                size = Some((width.parse().ok()?, height.parse().ok()?));
            }
            "stage" => stage = Some(value.to_string()),
            _ => {
                details.insert(key.to_string(), value.to_string());
            }
        }
    }
    Some(TraceLine {
        task: task?,
        size: size?,
        stage: stage?,
        details,
    })
}

/// What the guest said of one resize: how long it waited for the agent's thread, how long the
/// task took to take the size, and how long the app then took to post a frame of it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GuestResize {
    queued: f64,
    bounds: f64,
    since_bounds: f64,
}

impl GuestResize {
    /// From the request reaching the agent to the frame of its size, in milliseconds.
    fn frame(&self) -> f64 {
        self.queued + self.bounds + self.since_bounds
    }
}

/// The guest's account of the resize of `task` to `size` in `lines`, from the latest agent
/// and hwcomposer summaries of that size; `None` until the frame was posted.
fn guest_resize(lines: &[TraceLine], task: i32, size: (u32, u32)) -> Option<GuestResize> {
    let latest = |stage: &str| {
        lines
            .iter()
            .rev()
            .find(|line| line.task == task && line.size == size && line.stage == stage)
    };
    let agent = latest("agent.summary")?;
    let hwc = latest("hwc.summary")?;
    Some(GuestResize {
        queued: agent.number("queued").unwrap_or(0.0),
        bounds: agent.number("bounds")?,
        since_bounds: hwc.number("since_bounds")?,
    })
}

/// The value below which 95 of 100 of `values` lie (the nearest rank).
fn p95(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((sorted.len() as f64) * 0.95).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[(sorted.len() - 1) / 2]
}

/// One resize as the check measured it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measured {
    size: (u32, u32),
    /// From the request to the guest reporting the task at the size, on the host's clock.
    reported: f64,
    /// The guest's account, when its frame came.
    guest: Option<GuestResize>,
}

/// What is wrong with `measured`, against the budgets.
fn verdict(measured: &[Measured]) -> Vec<String> {
    let mut problems = Vec::new();
    for resize in measured.iter().filter(|resize| resize.guest.is_none()) {
        problems.push(format!(
            "no frame of {}x{} came within {} s of asking for it",
            resize.size.0,
            resize.size.1,
            FRAME_WAIT.as_secs()
        ));
    }
    let reported: Vec<f64> = measured.iter().map(|resize| resize.reported).collect();
    let guest: Vec<GuestResize> = measured.iter().filter_map(|resize| resize.guest).collect();
    let queued: Vec<f64> = guest.iter().map(|resize| resize.queued).collect();
    let frame: Vec<f64> = guest.iter().map(GuestResize::frame).collect();
    let reported_budget = REPORTED_BUDGET.as_secs_f64() * 1000.0;
    if p95(&reported) > reported_budget {
        problems.push(format!(
            "the guest took {:.0} ms (95th percentile) to report a task at its new size, more than {reported_budget:.0} ms",
            p95(&reported)
        ));
    }
    if p95(&queued) > QUEUED_BUDGET {
        problems.push(format!(
            "a new size waited {:.0} ms (95th percentile) for the agent's thread, more than {QUEUED_BUDGET:.0} ms",
            p95(&queued)
        ));
    }
    if p95(&frame) > FRAME_BUDGET {
        problems.push(format!(
            "the frame of a new size came {:.0} ms (95th percentile) after the agent had it, more than {FRAME_BUDGET:.0} ms",
            p95(&frame)
        ));
    }
    problems
}

/// The measurement as Markdown, for the job's summary.
fn report(measured: &[Measured], slow_work: &[String]) -> String {
    let reported: Vec<f64> = measured.iter().map(|resize| resize.reported).collect();
    let guest: Vec<GuestResize> = measured.iter().filter_map(|resize| resize.guest).collect();
    let column = |pick: fn(&GuestResize) -> f64| guest.iter().map(pick).collect::<Vec<f64>>();
    let rows = [
        ("reported (host clock)", reported),
        ("queued for the agent's thread", column(|g| g.queued)),
        ("bounds taken", column(|g| g.bounds)),
        (
            "app's first frame after the bounds",
            column(|g| g.since_bounds),
        ),
        ("frame (queued + bounds + app)", column(GuestResize::frame)),
    ];
    let mut out = format!(
        "### Resizing {APP} with an accessibility client watching\n\n{} resizes, {} with a frame, {} ms apart, on the runner's software Vulkan.\n\n| stage | median ms | 95th percentile ms |\n|---|---|---|\n",
        measured.len(),
        guest.len(),
        STEP.as_millis()
    );
    for (name, values) in rows {
        out.push_str(&format!(
            "| {name} | {:.0} | {:.0} |\n",
            median(&values),
            p95(&values)
        ));
    }
    out.push_str(
        "\n| size | reported | queued | bounds | app | frame |\n|---|---|---|---|---|---|\n",
    );
    for resize in measured {
        match resize.guest {
            Some(g) => out.push_str(&format!(
                "| {}x{} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} |\n",
                resize.size.0,
                resize.size.1,
                resize.reported,
                g.queued,
                g.bounds,
                g.since_bounds,
                g.frame()
            )),
            None => out.push_str(&format!(
                "| {}x{} | {:.0} | | | | none |\n",
                resize.size.0, resize.size.1, resize.reported
            )),
        }
    }
    if !slow_work.is_empty() {
        out.push_str("\nWork that held a thread of the agent:\n\n");
        for line in slow_work {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out
}

#[test]
fn fr4_the_check_reads_each_stage_of_a_resize_from_the_guests_trace() {
    let lines: Vec<TraceLine> = [
        "11-12 00:33:13.124  1106  1133 I DsaAgent: resize-trace task=4 size=600x800 stage=agent.summary utc=2026-11-12T00:33:13.124Z mono=1.0 applied=none bounds=1.5 queued=640.2",
        "11-12 00:33:14.288   536   542 I dsa-hwcomposer: resize-trace task=4 size=600x800 stage=hwc.summary utc=2026-11-12T00:33:14.288Z mono=2.0 since_bounds=1168.2 held=1162.1 late=false skipped=1",
        "11-12 00:33:15.000   536   542 I dsa-hwcomposer: resize-trace task=4 size=700x800 stage=hwc.summary utc=2026-11-12T00:33:15.000Z mono=3.0 since_bounds=99.0 held=90.0 late=false",
        "a line that is no trace at all",
    ]
    .into_iter()
    .filter_map(trace_line)
    .collect();
    assert_eq!(lines.len(), 3);
    let resize = guest_resize(&lines, 4, (600, 800)).expect("both summaries of 600x800");
    assert_eq!(
        resize,
        GuestResize {
            queued: 640.2,
            bounds: 1.5,
            since_bounds: 1168.2
        }
    );
    assert!((resize.frame() - 1809.9).abs() < 0.01);
    // No agent summary of 700x800, and nothing of another task.
    assert_eq!(guest_resize(&lines, 4, (700, 800)), None);
    assert_eq!(guest_resize(&lines, 5, (600, 800)), None);
}

#[test]
fn fr4_the_check_asks_each_size_twice_and_never_the_size_the_task_has() {
    let order: Vec<usize> = (0..2 * SIZES.len()).map(size_at).collect();
    for index in 0..SIZES.len() {
        assert_eq!(
            order.iter().filter(|&&at| at == index).count(),
            2,
            "{order:?}"
        );
    }
    // Run 43223832949 asked for the last size of the first round again at the start of the
    // second; the guest had it, drew nothing new, and the check waited for a frame in vain.
    assert!(order.windows(2).all(|pair| pair[0] != pair[1]), "{order:?}");
    assert_eq!(&order[SIZES.len()..], &[6, 5, 4, 3, 2, 1, 0, 7]);
    // A size the guest keeps within the task's smallest is waited for at the size it gives.
    assert_eq!(
        guest_choice((300, 900), Some((400, 300)), Some((1920, 1008))),
        (400, 900)
    );
}

#[test]
fn fr4_the_check_fails_what_the_owners_pc_measured_and_passes_a_fast_guest() {
    // v0.2.3 on the owner's PC: Google Play's new sizes waited up to 1.4 s for the agent.
    let slow: Vec<Measured> = [1380.0, 450.0, 900.0, 160.0]
        .iter()
        .map(|&queued| Measured {
            size: (1574, 1187),
            reported: queued + 5.0,
            guest: Some(GuestResize {
                queued,
                bounds: 2.0,
                since_bounds: 100.0,
            }),
        })
        .collect();
    let problems = verdict(&slow);
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(problems[0].contains("to report a task at its new size"));
    assert!(problems[1].contains("for the agent's thread"));

    let fast: Vec<Measured> = (0..16)
        .map(|step| Measured {
            size: (600 + step, 800),
            reported: 20.0,
            guest: Some(GuestResize {
                queued: 1.0,
                bounds: 15.0,
                since_bounds: 300.0,
            }),
        })
        .collect();
    assert!(verdict(&fast).is_empty());
    // A resize whose frame never came fails, however fast the others were.
    let mut lost = fast.clone();
    lost[3].guest = None;
    assert_eq!(verdict(&lost).len(), 1);
    assert!(report(&lost, &[]).contains("| 603x800 | 20 | | | | none |"));
    assert_eq!(
        p95(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 100.0]),
        100.0
    );
    assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
}

/// The task of `package`, once it is listed with its frames placed in a window.
fn app_task(api: &mut HostApiClient, package: &str, deadline: Instant) -> Result<Task, String> {
    loop {
        let tasks = api.tasks().map_err(|why| format!("listing tasks: {why}"))?;
        if let Some(task) = tasks
            .tasks
            .iter()
            .find(|task| task.package_name == package)
            .filter(|task| {
                tasks
                    .frames
                    .iter()
                    .any(|(id, display)| *id == task.task_id && *display != 0)
            })
        {
            return Ok(task.clone());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{package} never got a task whose frames reach a window (tasks {:?}, frames {:?})",
                tasks
                    .tasks
                    .iter()
                    .map(|task| &task.package_name)
                    .collect::<Vec<_>>(),
                tasks.frames
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `task_id`'s size as the guest reports it.
fn reported_size(api: &mut HostApiClient, task_id: i32) -> Option<(u32, u32)> {
    let tasks = api.tasks().ok()?;
    let task = tasks
        .tasks
        .into_iter()
        .find(|task| task.task_id == task_id)?;
    Some((
        (task.bounds.right - task.bounds.left).max(0) as u32,
        (task.bounds.bottom - task.bounds.top).max(0) as u32,
    ))
}

#[test]
fn fr4_resizing_a_real_app_watched_by_an_accessibility_client_shows_each_size_in_time_on_the_real_image(
) {
    let (Some(crosvm), Some(image)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_WIDGET_TEST_IMAGE name no crosvm and image");
        return;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let started = Instant::now();
    let _watchdog = Watchdog::start("the resize check", BUDGET + OVERRUN);
    let dir = scratch();
    let socket = dir.join("host.sock");
    let config = dir.join("dsa-hostd.json");
    let settings = serde_json::json!({
        "image_dir": image,
        "kernel": kernel,
        "crosvm": crosvm,
        "socket": socket,
        "machine_dir": dir.join("machine"),
        "network": false,
        "memory_mb": 4096,
        "processors": 2,
        "data_disk_size_gb": 8,
    });
    std::fs::write(&config, settings.to_string()).unwrap();
    let service = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-linux-hostd"))
            .arg("--config")
            .arg(&config),
        dir.join("hostd.log"),
    );
    // The window manager shows the task in a window of the job's headless desktop and starts
    // the renderer, which renders on the software Vulkan driver the job installs.
    let _compositor = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-compositor")).env("DSA_SOCKET", &socket),
        dir.join("compositor.log"),
    );
    let _devices = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-devices"))
            .arg("--socket")
            .arg(&socket),
        dir.join("devices.log"),
    );
    let mut api = client(&socket, Duration::from_secs(30));
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let reader = client(&socket, Duration::from_secs(10))
            .watch_log(&LogFilter::default(), false)
            .expect("reading the guest log");
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            let mut reader = reader;
            loop {
                match reader.next(Duration::from_secs(5)) {
                    Ok(Some(batch)) => {
                        let mut log = lock(&log);
                        for entry in batch.entries {
                            log.push(entry.threadtime());
                        }
                    }
                    Ok(None) => {}
                    Err(_) => return,
                }
            }
        });
    }
    if let Err(why) =
        api.ensure_running(Duration::from_secs(600), Duration::from_secs(2), |state| {
            eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
        })
    {
        fail(
            format!("Android did not become ready: {why}"),
            &service,
            &log,
        );
    }
    eprintln!(
        "Android was ready {} s after the start",
        started.elapsed().as_secs()
    );

    if let Err(why) = api.launch_app(APP) {
        fail(format!("opening {APP}: {why}"), &service, &log);
    }
    let task = match app_task(&mut api, APP, Instant::now() + APP_WAIT) {
        Ok(task) => task,
        Err(why) => fail(why, &service, &log),
    };
    let Some(max) = task.max_size() else {
        fail(
            format!("the guest gave no largest size for task {}", task.task_id),
            &service,
            &log,
        );
    };
    // A host accessibility client watches the window from here on and takes every tree. Why
    // its stream ended, if it did, goes into the failure.
    let watched = Arc::new(Mutex::new(0usize));
    let ended: Arc<Mutex<Option<String>>> = Arc::default();
    {
        let stream = client(&socket, Duration::from_secs(10)).watch_accessibility(task.task_id);
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => fail(
                format!("watching task {}'s accessibility tree: {why}", task.task_id),
                &service,
                &log,
            ),
        };
        let watched = Arc::clone(&watched);
        let ended = Arc::clone(&ended);
        std::thread::spawn(move || loop {
            match stream.next(Duration::from_secs(5)) {
                Ok(Some(_)) => *watched.lock().unwrap() += 1,
                Ok(None) => {}
                Err(why) => {
                    *ended.lock().unwrap() = Some(why.to_string());
                    return;
                }
            }
        });
    }
    // The resizes are measured with the client holding the app's tree, as a screen reader
    // does, so they start only once the first tree came. The guest may take a while to bind
    // its accessibility service right after boot, but a tree that never comes is a broken
    // export, and the check says so with what the agent and the service said about it.
    let tree_wait = Instant::now();
    while *watched.lock().unwrap() == 0 {
        let why = ended.lock().unwrap().clone();
        if why.is_some() || tree_wait.elapsed() >= TREE_WAIT {
            let accessibility: Vec<String> = lock(&log)
                .iter()
                .filter(|line| line.contains("DsaAgent") && line.contains("accessibility"))
                .cloned()
                .collect();
            fail(
                format!(
                    "the accessibility client got no tree of task {} in {} s ({}), so the app could not be resized as a watched one; what the agent said about accessibility:\n{}",
                    task.task_id,
                    tree_wait.elapsed().as_secs(),
                    why.map_or_else(|| "its stream stayed open".to_string(), |why| format!("its stream ended: {why}")),
                    accessibility.join("\n"),
                ),
                &service,
                &log,
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!(
        "the accessibility client got task {}'s first tree {} ms after it asked",
        task.task_id,
        tree_wait.elapsed().as_millis()
    );
    // The app settles.
    std::thread::sleep(Duration::from_secs(2));

    let mut measured = Vec::new();
    for step in 0..2 * SIZES.len() {
        let (width, height) = SIZES[size_at(step)];
        let size = (
            ((max.0 as f64) * width) as u32,
            ((max.1 as f64) * height) as u32,
        );
        // What the guest gives the task for it: the window manager takes that as the answer.
        let given = guest_choice(size, task.min_size(), Some(max));
        if given != size {
            eprintln!(
                "the guest keeps task {} at {}x{} for the {}x{} asked, within its smallest size {:?}",
                task.task_id,
                given.0,
                given.1,
                size.0,
                size.1,
                task.min_size()
            );
        }
        // Only what the guest says from here on is about this resize: the same size was
        // asked for in the first round.
        let from = lock(&log).len();
        let asked = Instant::now();
        if let Err(why) = api.resize_task(task.task_id, size.0, size.1, true) {
            fail(
                format!("resizing task {}: {why}", task.task_id),
                &service,
                &log,
            );
        }
        // The window manager reconfigures the task once the guest reports it at the size.
        let mut reported = None;
        while asked.elapsed() < FRAME_WAIT {
            if reported_size(&mut api, task.task_id) == Some(given) {
                reported = Some(asked.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let Some(reported) = reported else {
            fail(
                format!(
                    "the guest never reported task {} at {}x{}",
                    task.task_id, given.0, given.1
                ),
                &service,
                &log,
            );
        };
        if let Err(why) = api.resize_task(task.task_id, size.0, size.1, false) {
            fail(
                format!("reconfiguring task {}: {why}", task.task_id),
                &service,
                &log,
            );
        }
        // The frame of the size, as the guest's trace tells it.
        let mut guest = None;
        while asked.elapsed() < FRAME_WAIT {
            let lines: Vec<TraceLine> = lock(&log)[from..]
                .iter()
                .map(String::as_str)
                .filter_map(trace_line)
                .collect();
            if let Some(found) = guest_resize(&lines, task.task_id, given) {
                guest = Some(found);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let resize = Measured {
            size,
            reported: reported.as_secs_f64() * 1000.0,
            guest,
        };
        eprintln!("{resize:?}");
        measured.push(resize);
        std::thread::sleep(STEP.saturating_sub(asked.elapsed()));
    }

    let slow_work: Vec<String> = lock(&log)
        .iter()
        .filter(|line| line.contains("on one piece of work"))
        .cloned()
        .collect();
    let summary = report(&measured, &slow_work);
    let _ = std::fs::write(dir.join("resize.md"), &summary);
    let _ = std::fs::write(dir.join("guest.log"), lock(&log).join("\n"));
    println!("{summary}");
    if *watched.lock().unwrap() == 0 {
        fail(
            "the accessibility client never got a tree, so the check did not resize the app as a watched one".into(),
            &service,
            &log,
        );
    }
    let problems = verdict(&measured);
    if !problems.is_empty() {
        fail(problems.join("\n"), &service, &log);
    }
    let _ = api.stop();
    drop(service);
    eprintln!(
        "the resize check took {} s; what it read is in {}",
        started.elapsed().as_secs(),
        dir.display()
    );
}

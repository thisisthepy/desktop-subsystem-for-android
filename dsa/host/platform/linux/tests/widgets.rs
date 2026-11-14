//! A desktop widget placed on the real Android image, end to end: the service boots the image
//! under crosvm, a client places the digital clock widget through the local API, and the
//! widget's pictures reach the client. Then it installs the widget probe
//! (`dsa/guest/widget-probe/`), whose widgets need a setup, and places each: the setup must
//! open as the probe's own focused task window on the desktop, not in a task the host has no
//! window for, and its answer must decide whether the widget is placed.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM` names the crosvm to run,
//! `DSA_WIDGET_TEST_IMAGE` the image folder built from the commit under test,
//! `DSA_WIDGET_TEST_KERNEL` the kernel to boot it with when the folder carries none, and
//! `DSA_WIDGET_PROBE` the widget probe's APK. It needs `/dev/kvm` and `/dev/vhost-vsock`, and
//! is skipped without them.
//!
//! The agent's unit tests run against fakes of Android's widget host, so they cannot catch a
//! widget that Android's own classes fail to draw. This test can: it fails when no picture
//! comes, when the picture is blank, when the clock does not tick on, and when the guest log
//! says a widget failed to draw or had no listener.
#![cfg(target_os = "linux")]

mod boot_check;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boot_check::{lock, Process as Service, Watchdog};

use dsa_host_core::apk_info::inflate::inflate;
use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::tasks::{Task, TaskEvent};
use dsa_host_core::widgets::{Widget, WidgetEvent, WidgetFrame};
use dsa_host_unix::socket::UnixLocalConnector;

/// The widget placed: the digital clock of AOSP's clock app, which needs no setup and redraws
/// every minute.
const CLOCK: &str = "com.android.deskclock/com.android.alarmclock.DigitalAppWidgetProvider";
/// The whole test, boot included, stays under thirteen minutes.
const BUDGET: Duration = Duration::from_secs(750);
/// The widget probe's package.
const PROBE: &str = "io.github.thisisthepy.dsa.widgetprobe";
/// What the probe's widgets take, from its install to the last widget placed.
const SETUPS: Duration = Duration::from_secs(180);
/// A setup's window shows this soon after its widget is asked for.
const SETUP_WINDOW: Duration = Duration::from_secs(60);
/// The first picture comes this soon after the widget is placed.
const FIRST_FRAME: Duration = Duration::from_secs(60);
/// A clock that shows minutes draws a new picture within this long.
const NEXT_FRAME: Duration = Duration::from_secs(80);
/// How long past its budget the test may run before it stops everything it started and
/// ends: well inside the job step's 12 minutes, so the step never kills it with its VM still
/// running.
const OVERRUN: Duration = Duration::from_secs(45);

fn scratch() -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("widget-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fails the test with what the service printed and the guest log's last lines.
fn fail(why: String, service: &Service, log: &Mutex<Vec<String>>) -> ! {
    // The lock is let go before the panic, so the thread that reads the guest log is not
    // left with a poisoned lock and does not panic on top of this failure.
    let tail = {
        let log = lock(log);
        let _ = std::fs::write(service.log.with_file_name("guest.log"), log.join("\n"));
        log[log.len().saturating_sub(200)..].join("\n")
    };
    let path = service.log.with_file_name("compositor.log");
    let compositor = std::fs::read_to_string(path).unwrap_or_default();
    panic!(
        "{why}\n--- the service's output ---\n{}\n--- the window manager's output ---\n{compositor}\n--- the guest log's last lines ---\n{tail}",
        service.log(),
    )
}

fn client(socket: &Path, wait: Duration) -> HostApiClient {
    let mut connector = UnixLocalConnector {
        path: socket.to_path_buf(),
    };
    let deadline = Instant::now() + wait;
    loop {
        match HostApiClient::connect(&mut connector, "widget-test", Duration::from_secs(30)) {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("the service did not answer on {}: {why}", socket.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// The pixels of a PNG with 8 bits per sample, as `(width, height, channels, rows)`.
fn decode_png(png: &[u8]) -> Result<(usize, usize, usize, Vec<u8>), String> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 8 || png[..8] != SIGNATURE {
        return Err("not a PNG".into());
    }
    let (mut at, mut header, mut data) = (8, None, Vec::new());
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind = &png[at + 4..at + 8];
        let body = png
            .get(at + 8..at + 8 + len)
            .ok_or("a PNG chunk runs past the end")?;
        match kind {
            b"IHDR" => header = Some(body.to_vec()),
            b"IDAT" => data.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    let header = header.ok_or("the PNG has no header")?;
    let width = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
    let height = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    let (depth, colour, interlace) = (header[8], header[9], header[12]);
    let channels = match colour {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        other => {
            return Err(format!(
                "the PNG has colour type {other}, which is not checked"
            ))
        }
    };
    if depth != 8 || interlace != 0 {
        return Err(format!(
            "the PNG has {depth} bits per sample and interlace {interlace}; 8 and none are checked"
        ));
    }
    if data.len() < 2 {
        return Err("the PNG has no image data".into());
    }
    let stride = width * channels;
    // A zlib stream: two bytes of header before the DEFLATE data.
    let raw = inflate(&data[2..], height * (stride + 1))?;
    if raw.len() < height * (stride + 1) {
        return Err("the PNG's image data is short".into());
    }
    let mut out = vec![0u8; height * stride];
    for y in 0..height {
        let filter = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= channels {
                out[y * stride + x - channels]
            } else {
                0
            };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if x >= channels && y > 0 {
                out[(y - 1) * stride + x - channels]
            } else {
                0
            };
            let predicted = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let pa = (p - a as i16).abs();
                    let pb = (p - b as i16).abs();
                    let pc = (p - c as i16).abs();
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                other => return Err(format!("the PNG uses filter {other}")),
            };
            out[y * stride + x] = line[x].wrapping_add(predicted);
        }
    }
    Ok((width, height, channels, out))
}

/// Why a widget's picture shows nothing, or None when it shows something: some pixel is not
/// fully transparent, and the pixels are not all one colour.
fn blank(frame: &WidgetFrame) -> Option<String> {
    let (width, height, channels, pixels) = match decode_png(&frame.png) {
        Ok(decoded) => decoded,
        Err(why) => return Some(why),
    };
    if width == 0 || height == 0 {
        return Some(format!("the picture is {width}x{height}"));
    }
    if (width as u32, height as u32) != (frame.width, frame.height) {
        return Some(format!(
            "the picture is {width}x{height}, but the frame says {}x{}",
            frame.width, frame.height
        ));
    }
    let px: Vec<&[u8]> = pixels.chunks(channels).collect();
    if (channels == 2 || channels == 4) && px.iter().all(|p| p[channels - 1] == 0) {
        return Some("every pixel is fully transparent".into());
    }
    if px.iter().all(|p| *p == px[0]) {
        return Some(format!("every pixel is {:?}", px[0]));
    }
    None
}

/// Why a widget's picture has no see-through pixel, or None when it has one. The digital
/// clock draws its text over the wallpaper, so what it leaves undrawn must stay transparent,
/// not come out black.
fn opaque_everywhere(frame: &WidgetFrame) -> Option<String> {
    let (_, _, channels, pixels) = match decode_png(&frame.png) {
        Ok(decoded) => decoded,
        Err(why) => return Some(why),
    };
    if channels != 2 && channels != 4 {
        return Some(format!(
            "the picture has {channels} channels and so no alpha"
        ));
    }
    let px: Vec<&[u8]> = pixels.chunks(channels).collect();
    if px.iter().any(|p| p[channels - 1] == 0) {
        return None;
    }
    let clearest = px.iter().map(|p| p[channels - 1]).min().unwrap_or(255);
    Some(format!(
        "no pixel is see-through; the most transparent has an alpha of {clearest}"
    ))
}

/// Whether a guest log line says a widget failed to draw (`drawing widget 3 failed`).
fn drawing_failed(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find("drawing widget ") {
        let after = &rest[at + "drawing widget ".len()..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(" failed") {
            return true;
        }
        rest = after;
    }
    false
}

#[test]
fn fr12_the_log_check_finds_a_failed_drawing_and_a_null_listener() {
    assert!(drawing_failed(
        "W DsaAgent: drawing widget 12 failed; trying again"
    ));
    assert!(drawing_failed("drawing widget 1 failed"));
    assert!(!drawing_failed(
        "drawing widget 1 with the GPU failed; drawing it in software"
    ));
    assert!(!drawing_failed("drawing widget failed"));
}

#[test]
fn fr12_the_picture_check_tells_a_drawn_widget_from_a_blank_one() {
    // A 2x1 RGBA picture, stored without compression, with filter 0.
    fn png(pixels: [[u8; 4]; 2]) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out.extend_from_slice(&[0, 0, 0, 0]);
        }
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut out, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        let mut row = vec![0u8];
        row.extend_from_slice(&pixels[0]);
        row.extend_from_slice(&pixels[1]);
        let len = row.len() as u16;
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(&row);
        z.extend_from_slice(&[0, 0, 0, 0]);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
    let frame = |png: Vec<u8>| WidgetFrame {
        widget_id: 1,
        width: 2,
        height: 1,
        png,
    };
    assert_eq!(
        blank(&frame(png([[255, 255, 255, 255], [0, 0, 0, 0]]))),
        None
    );
    assert!(blank(&frame(png([[9, 9, 9, 0], [0, 0, 0, 0]]))).is_some());
    assert!(blank(&frame(png([[9, 9, 9, 255], [9, 9, 9, 255]]))).is_some());
    assert!(blank(&frame(b"not a png".to_vec())).is_some());

    // A clock's text over the wallpaper has see-through pixels; one drawn on black has none.
    assert_eq!(
        opaque_everywhere(&frame(png([[255, 255, 255, 255], [0, 0, 0, 0]]))),
        None
    );
    assert!(opaque_everywhere(&frame(png([[255, 255, 255, 255], [0, 0, 0, 255]]))).is_some());
}

/// The probe's task window of a setup, as the task watch told it.
#[derive(Default)]
struct SetupWindow {
    /// The window, if one showed.
    task: Option<Task>,
    /// Whether it had the focus at some point.
    focused: bool,
    /// Whether it went.
    closed: bool,
    /// The tasks there were before the widget was asked for, which are not its setup's.
    before: Vec<i32>,
}

impl SetupWindow {
    fn saw(&mut self, event: TaskEvent) {
        match event {
            TaskEvent::Updated(task)
                if task.package_name == PROBE
                    && !self.before.contains(&task.task_id)
                    && task.bounds.width() > 0
                    && task.bounds.height() > 0 =>
            {
                self.focused |= task.focused;
                self.task = Some(task);
            }
            TaskEvent::Removed { task_id }
                if self.task.as_ref().is_some_and(|t| t.task_id == task_id) =>
            {
                self.closed = true;
            }
            _ => {}
        }
    }
}

/// Places a widget of `provider`, watching the tasks meanwhile for the window of its setup.
fn place_with_setup(socket: &Path, provider: &str) -> (Result<Widget, String>, SetupWindow) {
    let mut tasks = client(socket, Duration::from_secs(10))
        .watch_tasks()
        .expect("watching the tasks");
    let placing = {
        let socket = socket.to_path_buf();
        let provider = provider.to_string();
        std::thread::spawn(move || {
            client(&socket, Duration::from_secs(10))
                .add_widget(&provider, SETUP_WINDOW * 2)
                .map_err(|why| why.to_string())
        })
    };
    let mut window = SetupWindow {
        before: tasks.current().tasks.iter().map(|t| t.task_id).collect(),
        ..SetupWindow::default()
    };
    let until = Instant::now() + SETUP_WINDOW * 2 + Duration::from_secs(10);
    while !placing.is_finished() && Instant::now() < until {
        match tasks.next(Duration::from_millis(250)) {
            Ok(Some(event)) => window.saw(event),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    let placed = placing
        .join()
        .unwrap_or_else(|_| Err("placing the widget panicked".into()));
    // The window goes a moment after the setup answers.
    let until = Instant::now() + Duration::from_secs(20);
    while window.task.is_some() && !window.closed && Instant::now() < until {
        match tasks.next(Duration::from_millis(250)) {
            Ok(Some(event)) => window.saw(event),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    (placed, window)
}

/// Why the probe's widgets were not placed as their setups answer; Ok when they were.
fn check_setups(socket: &Path, probe: &Path) -> Result<(), String> {
    let apk = std::fs::read(probe).map_err(|e| format!("reading {}: {e}", probe.display()))?;
    let outcome = client(socket, Duration::from_secs(10))
        .install_apk("dsa-widget-probe.apk", apk.len() as u64, &mut &apk[..])
        .map_err(|e| format!("installing the widget probe: {e}"))?;
    if !outcome.installed() {
        return Err(format!(
            "Android did not install the widget probe: {outcome:?}"
        ));
    }
    // The gallery lists a new app's widgets a moment after it is installed.
    let until = Instant::now() + Duration::from_secs(30);
    let offered = loop {
        let offered: Vec<_> = client(socket, Duration::from_secs(10))
            .widget_providers()
            .map_err(|e| format!("listing the widgets: {e}"))?
            .into_iter()
            .filter(|p| p.package_name == PROBE)
            .collect();
        if offered.len() >= 4 || Instant::now() >= until {
            break offered;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let find = |class: &str| {
        offered
            .iter()
            .find(|p| p.provider.ends_with(&format!(".{class}")))
            .cloned()
            .ok_or_else(|| format!("the gallery does not list the probe's {class}: {offered:?}"))
    };
    let ok = find("OkWidget")?;
    let cancel = find("CancelWidget")?;
    let at_once = find("AtOnceWidget")?;
    let optional = find("OptionalWidget")?;
    for needs in [&ok, &cancel, &at_once] {
        if !needs.needs_configuration {
            return Err(format!(
                "{} has a setup, but the gallery says it needs none",
                needs.provider
            ));
        }
    }
    if optional.needs_configuration {
        return Err(format!(
            "{} says its setup is optional, but the gallery says it needs one",
            optional.provider
        ));
    }

    // Finished: placed, after its setup showed in the probe's own focused window.
    let (placed, window) = place_with_setup(socket, &ok.provider);
    let Some(task) = &window.task else {
        return Err(format!(
            "the setup of {} opened no task window on the desktop (placing it: {:?})",
            ok.provider,
            placed.as_ref().map(|w| w.widget_id)
        ));
    };
    eprintln!(
        "the setup of {} showed in task {} at {:?}, titled {:?}",
        ok.provider, task.task_id, task.bounds, task.label
    );
    if !window.focused {
        return Err(format!(
            "the setup window of {} never had the focus",
            ok.provider
        ));
    }
    if let Err(why) = &placed {
        return Err(format!(
            "{} was not placed after its setup finished: {why}",
            ok.provider
        ));
    }
    if !window.closed {
        return Err(format!(
            "the setup window of {} stayed after its setup finished",
            ok.provider
        ));
    }

    // Canceled: not placed, and said to be canceled.
    let (placed, window) = place_with_setup(socket, &cancel.provider);
    if window.task.is_none() {
        return Err(format!(
            "the setup of {} opened no task window on the desktop",
            cancel.provider
        ));
    }
    match &placed {
        Ok(widget) => {
            return Err(format!(
                "{} was placed (widget {}) although its setup was canceled",
                cancel.provider, widget.widget_id
            ))
        }
        Err(why) if !why.contains("canceled") => {
            return Err(format!(
                "{} was not placed, but not as canceled: {why}",
                cancel.provider
            ))
        }
        Err(_) => {}
    }

    // Finished at once: placed.
    let (placed, _) = place_with_setup(socket, &at_once.provider);
    if let Err(why) = &placed {
        return Err(format!(
            "{} was not placed though its setup finished at once: {why}",
            at_once.provider
        ));
    }

    // Optional: placed without a setup.
    let (placed, window) = place_with_setup(socket, &optional.provider);
    if let Err(why) = &placed {
        return Err(format!("{} was not placed: {why}", optional.provider));
    }
    if let Some(task) = &window.task {
        return Err(format!(
            "{} ran its setup in task {} though its app says the setup is optional",
            optional.provider, task.task_id
        ));
    }

    let (placed, _) = client(socket, Duration::from_secs(10))
        .widgets()
        .map_err(|e| format!("listing the placed widgets: {e}"))?;
    let mut probes: Vec<&str> = placed
        .iter()
        .filter(|w| w.provider.package_name == PROBE)
        .map(|w| w.provider.provider.as_str())
        .collect();
    probes.sort_unstable();
    let mut expected = vec![
        ok.provider.as_str(),
        at_once.provider.as_str(),
        optional.provider.as_str(),
    ];
    expected.sort_unstable();
    if probes != expected {
        return Err(format!(
            "the placed probe widgets are {probes:?}; expected {expected:?}"
        ));
    }
    Ok(())
}

#[test]
fn fr12_a_placed_clock_widget_draws_on_the_real_image_and_ticks() {
    let (Some(crosvm), Some(image)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_WIDGET_TEST_IMAGE name no crosvm and image");
        return;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let probe = std::env::var_os("DSA_WIDGET_PROBE")
        .map(PathBuf::from)
        .filter(|apk| apk.is_file())
        .expect(
            "DSA_WIDGET_PROBE names no widget probe APK (dsa/guest/widget-probe/build.sh), so no widget with a setup can be placed",
        );
    let started = Instant::now();
    let deadline = started + BUDGET;
    let _watchdog = Watchdog::start("the widget boot check", BUDGET + OVERRUN);
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
    // Android's renderer (dsa-gfx) is started by the window manager, which shows Android in
    // the desktop session the job runs (a headless Wayland compositor): without it Android
    // never finishes booting.
    let _compositor = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-compositor")).env("DSA_SOCKET", &socket),
        dir.join("compositor.log"),
    );
    // The user's devices for Android (sound, sensors, location), as the desktop session runs
    // them.
    let _devices = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-devices"))
            .arg("--socket")
            .arg(&socket),
        dir.join("devices.log"),
    );
    let mut first = client(&socket, Duration::from_secs(30));
    // The guest log, read from the start of the boot: Android sends it only while something
    // reads it, and a boot that fails says why in it.
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
    let boot = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(FIRST_FRAME + NEXT_FRAME + SETUPS);
    let ready = first.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    });
    if let Err(why) = ready {
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
    let mut watch = client(&socket, Duration::from_secs(10))
        .watch_widgets()
        .expect("watching the widgets");
    let placed = started.elapsed();
    let mut placer = client(&socket, Duration::from_secs(10));
    let widget = match placer.add_widget(CLOCK, Duration::from_secs(120)) {
        Ok(widget) => widget,
        Err(why) => fail(format!("placing {CLOCK} failed: {why}"), &service, &log),
    };
    let id = widget.widget_id;
    eprintln!("placed widget {id} {} s after the start", placed.as_secs());

    // The first picture, then one that differs from it as the minute turns.
    let mut pictures: Vec<WidgetFrame> = Vec::new();
    let mut until = Instant::now() + FIRST_FRAME;
    while pictures.len() < 2 {
        let now = Instant::now();
        if now >= until {
            let why = if pictures.is_empty() {
                format!(
                    "widget {id} sent no picture within {} s of being placed",
                    FIRST_FRAME.as_secs()
                )
            } else {
                format!(
                    "widget {id} sent no new picture within {} s of its first; the clock did not tick on",
                    NEXT_FRAME.as_secs()
                )
            };
            fail(why, &service, &log);
        }
        match watch.next(until - now) {
            Ok(Some(WidgetEvent::Frame(frame))) if frame.widget_id == id => {
                if let Some(why) = blank(&frame) {
                    fail(
                        format!(
                            "widget {id} sent a blank {}x{} picture: {why}",
                            frame.width, frame.height
                        ),
                        &service,
                        &log,
                    );
                }
                if let Some(why) = opaque_everywhere(&frame) {
                    let _ = std::fs::write(dir.join("opaque.png"), &frame.png);
                    fail(
                        format!(
                            "widget {id} sent a {}x{} picture with no see-through pixel, so the clock covers the wallpaper: {why}",
                            frame.width, frame.height
                        ),
                        &service,
                        &log,
                    );
                }
                if pictures.last().is_some_and(|last| last.png == frame.png) {
                    continue;
                }
                eprintln!(
                    "widget {id}: a {}x{} picture of {} bytes {} s after the start",
                    frame.width,
                    frame.height,
                    frame.png.len(),
                    started.elapsed().as_secs()
                );
                if pictures.is_empty() {
                    until = Instant::now() + NEXT_FRAME;
                    let _ = std::fs::write(dir.join("first.png"), &frame.png);
                }
                pictures.push(frame);
            }
            Ok(_) => {}
            Err(why) => fail(
                format!("the watch on the widgets ended: {why}"),
                &service,
                &log,
            ),
        }
    }

    // The widgets that need a setup.
    let setups = Instant::now();
    if let Err(why) = check_setups(&socket, &probe) {
        fail(why, &service, &log);
    }
    eprintln!(
        "the probe's widgets were set up in {} s",
        setups.elapsed().as_secs()
    );

    // Records take a moment to come from the guest.
    std::thread::sleep(Duration::from_secs(3));
    let lines = lock(&log).clone();
    let bad: Vec<&String> = lines
        .iter()
        .filter(|line| drawing_failed(line) || line.contains("null listener"))
        .collect();
    if !bad.is_empty() {
        let bad: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
        fail(
            format!("the guest log says the widget failed:\n{}", bad.join("\n")),
            &service,
            &log,
        );
    }
    let _ = first.stop();
    drop(service);
    let mut report = std::io::stderr();
    let _ = writeln!(
        report,
        "the widget test took {} s; the guest logged {} lines",
        started.elapsed().as_secs(),
        lines.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

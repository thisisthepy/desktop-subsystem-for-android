//! A desktop widget placed on the real Android image, end to end: the service boots the image
//! under crosvm, a client places the digital clock widget through the local API, and the
//! widget's pictures reach the client.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM` names the crosvm to run,
//! `DSA_WIDGET_TEST_IMAGE` the image folder built from the commit under test, and
//! `DSA_WIDGET_TEST_KERNEL` the kernel to boot it with when the folder carries none. It needs
//! `/dev/kvm` and `/dev/vhost-vsock`, and is skipped without them.
//!
//! The agent's unit tests run against fakes of Android's widget host, so they cannot catch a
//! widget that Android's own classes fail to draw. This test can: it fails when no picture
//! comes, when the picture is blank, when the clock does not tick on, and when the guest log
//! says a widget failed to draw or had no listener.
#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::apk_info::inflate::inflate;
use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::widgets::{WidgetEvent, WidgetFrame};
use dsa_host_unix::socket::UnixLocalConnector;

/// The widget placed: the digital clock of AOSP's clock app, which needs no setup and redraws
/// every minute.
const CLOCK: &str = "com.android.deskclock/com.android.alarmclock.DigitalAppWidgetProvider";
/// The whole test, boot included, stays under ten minutes.
const BUDGET: Duration = Duration::from_secs(570);
/// The first picture comes this soon after the widget is placed.
const FIRST_FRAME: Duration = Duration::from_secs(60);
/// A clock that shows minutes draws a new picture within this long.
const NEXT_FRAME: Duration = Duration::from_secs(80);

fn scratch() -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("widget-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The service, stopped (SIGTERM, then SIGKILL) when dropped, so a failing test leaves no VM.
struct Service {
    child: Child,
    log: PathBuf,
}

impl Service {
    fn start(dir: &Path, config: &Path) -> Service {
        let log = dir.join("hostd.log");
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_dsa-linux-hostd"))
            .arg("--config")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .expect("starting dsa-linux-hostd");
        Service { child, log }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: kill with a child's own process ID and a constant signal.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Fails the test with what the service printed and the guest log's last lines.
fn fail(why: String, service: &Service, log: &[String]) -> ! {
    let _ = std::fs::write(service.log.with_file_name("guest.log"), log.join("\n"));
    let tail = &log[log.len().saturating_sub(200)..];
    let path = service.log.with_file_name("compositor.log");
    let compositor = std::fs::read_to_string(path).unwrap_or_default();
    panic!(
        "{why}\n--- the service's output ---\n{}\n--- the window manager's output ---\n{compositor}\n--- the guest log's last lines ---\n{}",
        service.log(),
        tail.join("\n")
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
    let started = Instant::now();
    let deadline = started + BUDGET;
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
    let service = Service::start(&dir, &config);
    // Android's renderer is in the window manager, which shows Android in the desktop
    // session the job runs (a headless Wayland compositor): without it Android never finishes
    // booting.
    let compositor_log = std::fs::File::create(dir.join("compositor.log")).unwrap();
    let _compositor = Service {
        child: Command::new(env!("CARGO_BIN_EXE_dsa-compositor"))
            .env("DSA_SOCKET", &socket)
            .stdin(Stdio::null())
            .stdout(compositor_log.try_clone().unwrap())
            .stderr(compositor_log)
            .spawn()
            .expect("starting dsa-compositor"),
        log: dir.join("compositor.log"),
    };
    // The user's devices for Android (sound, sensors, location), as the desktop session runs
    // them.
    let devices_log = std::fs::File::create(dir.join("devices.log")).unwrap();
    let _devices = Service {
        child: Command::new(env!("CARGO_BIN_EXE_dsa-devices"))
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(devices_log.try_clone().unwrap())
            .stderr(devices_log)
            .spawn()
            .expect("starting dsa-devices"),
        log: dir.join("devices.log"),
    };
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
                        let mut log = log.lock().unwrap();
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
    let boot = deadline.saturating_duration_since(Instant::now()) - FIRST_FRAME - NEXT_FRAME;
    let ready = first.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    });
    if let Err(why) = ready {
        fail(
            format!("Android did not become ready: {why}"),
            &service,
            &log.lock().unwrap(),
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
        Err(why) => fail(
            format!("placing {CLOCK} failed: {why}"),
            &service,
            &log.lock().unwrap(),
        ),
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
            fail(why, &service, &log.lock().unwrap());
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
                        &log.lock().unwrap(),
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
                        &log.lock().unwrap(),
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
                &log.lock().unwrap(),
            ),
        }
    }

    // Records take a moment to come from the guest.
    std::thread::sleep(Duration::from_secs(3));
    let lines = log.lock().unwrap().clone();
    let bad: Vec<&String> = lines
        .iter()
        .filter(|line| drawing_failed(line) || line.contains("null listener"))
        .collect();
    if !bad.is_empty() {
        let bad: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
        fail(
            format!("the guest log says the widget failed:\n{}", bad.join("\n")),
            &service,
            &lines,
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

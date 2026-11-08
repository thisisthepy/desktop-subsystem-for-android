//! The design of Android's own screens on the real image, end to end: the service boots the
//! image under crosvm, the test opens Settings in task windows of a tall and a short height
//! through the local API, and reads what Android drew through adb (developer mode): the view
//! hierarchy (`uiautomator dump`) and the screen (`screencap`).
//!
//! It checks that Settings' home page opens with its large title expanded in a window 800 dp
//! tall and folds it when the list scrolls, that it opens folded in a window under 600 dp
//! tall, and that the rows of a Settings list are grouped in rounded cards: the first row of a
//! group has rounded corners, and a row joined to the one above it square ones.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM` names the crosvm to run,
//! `DSA_WIDGET_TEST_IMAGE` the image folder built from the commit under test,
//! `DSA_WIDGET_TEST_KERNEL` the kernel to boot it with when the folder carries none, and
//! `DSA_ADB` the adb to read the guest with (`adb` on the path when not set). It needs
//! `/dev/kvm` and `/dev/vhost-vsock`. The pictures and hierarchies it read stay in its folder
//! (`/tmp/screens-test-<pid>`) for the job to keep.
#![cfg(target_os = "linux")]

mod boot_check;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boot_check::{lock, run_bounded, Process as Service, Watchdog};

use dsa_host_core::apk_info::inflate::inflate;
use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_unix::socket::UnixLocalConnector;

const SETTINGS: &str = "com.android.settings";
const SETTINGS_HOME: &str = "com.android.settings/.homepage.SettingsHomepageActivity";
/// A Settings page with several groups of rows.
const DISPLAY_SETTINGS: &str = "android.settings.DISPLAY_SETTINGS";
/// The window heights checked, in dp: one well above the 600 dp the large title needs, and
/// one below it.
const TALL_DP: u32 = 800;
const SHORT_DP: u32 = 520;
const WIDTH_DP: u32 = 480;
/// The whole test, boot included, stays under twelve minutes.
const BUDGET: Duration = Duration::from_secs(690);
/// How long past its budget the test may run before it stops everything it started and
/// ends: inside the job step's 14 minutes, so the step never kills it with its VM still
/// running.
const OVERRUN: Duration = Duration::from_secs(60);
/// How long one adb command may take. A dump or a capture takes seconds.
const ADB_LIMIT: Duration = Duration::from_secs(120);
/// How long the screenrecord fallback may take: it records for 3 s.
const SCREENRECORD_LIMIT: Duration = Duration::from_secs(30);
/// How many frames the screenrecord fallback reads before it stops: the newest of them is
/// kept, and raw frames are large (6 MiB at 1920x1080).
const SCREENRECORD_FRAMES: usize = 4;

fn scratch() -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("screens-test-{}", std::process::id()));
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
    let compositor =
        std::fs::read_to_string(service.log.with_file_name("compositor.log")).unwrap_or_default();
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
        match HostApiClient::connect(&mut connector, "screens-test", Duration::from_secs(30)) {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("the service did not answer on {}: {why}", socket.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// A rectangle in display pixels, as uiautomator writes one: `[left,top][right,bottom]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn parse(text: &str) -> Option<Rect> {
        let numbers: Vec<i32> = text
            .split(['[', ']', ','])
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().ok())
            .collect::<Option<Vec<i32>>>()?;
        match numbers[..] {
            [left, top, right, bottom] => Some(Rect {
                left,
                top,
                right,
                bottom,
            }),
            _ => None,
        }
    }

    fn width(&self) -> i32 {
        self.right - self.left
    }

    fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// One node of a uiautomator dump, with the index of its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    parent: Option<usize>,
    class: String,
    text: String,
    id: String,
    bounds: Rect,
}

/// The value of attribute `name` in the attributes text of an XML element.
fn attribute(element: &str, name: &str) -> Option<String> {
    let start = element.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = element[start..].find('"')? + start;
    Some(
        element[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// The nodes of a uiautomator dump, in document order.
fn nodes(xml: &str) -> Vec<Node> {
    let mut found = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut at = 0;
    while let Some(next) = xml[at..].find('<') {
        let start = at + next;
        let Some(length) = xml[start..].find('>') else {
            break;
        };
        let tag = &xml[start..start + length + 1];
        at = start + length + 1;
        if tag.starts_with("</node") {
            open.pop();
        } else if tag.starts_with("<node") {
            let index = found.len();
            found.push(Node {
                parent: open.last().copied(),
                class: attribute(tag, "class").unwrap_or_default(),
                text: attribute(tag, "text").unwrap_or_default(),
                id: attribute(tag, "resource-id").unwrap_or_default(),
                bounds: attribute(tag, "bounds")
                    .and_then(|b| Rect::parse(&b))
                    .unwrap_or(Rect {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    }),
            });
            if !tag.ends_with("/>") {
                open.push(index);
            }
        }
    }
    found
}

/// The first node whose id is Settings' `name`.
fn by_id<'a>(nodes: &'a [Node], name: &str) -> Option<(usize, &'a Node)> {
    let id = format!("{SETTINGS}:id/{name}");
    nodes.iter().enumerate().find(|(_, node)| node.id == id)
}

/// The large title of Settings' home page: a text view with text above the search bar, at
/// least twice its height. Settings hides the search bar when its search app is off; then
/// the title is above the list, at least a sixth of its height. None when the home page shows
/// no such title (folded).
fn large_title(nodes: &[Node]) -> Result<Option<Rect>, String> {
    let (below, least) = match by_id(nodes, "search_action_bar") {
        Some((_, search)) => (search.bounds.top, 2 * search.bounds.height()),
        None => {
            let (_, list) = by_id(nodes, "main_content_scrollable_container").ok_or(
                "the dump has neither the home page's search bar nor its list \
                 (com.android.settings:id/search_action_bar, main_content_scrollable_container)",
            )?;
            (list.bounds.top, list.bounds.height() / 6)
        }
    };
    Ok(nodes
        .iter()
        .filter(|node| node.class.ends_with("TextView") && !node.text.is_empty())
        .map(|node| node.bounds)
        .find(|bounds| bounds.bottom <= below && bounds.height() > 0 && bounds.height() >= least))
}

/// Whether a picture is nothing but zeros: a capture that never received the screen, since
/// a drawn screen is opaque.
fn empty_picture(pixels: &[u8]) -> bool {
    pixels.iter().all(|&byte| byte == 0)
}

/// The newest whole frame of `screenrecord --output-format=raw-frames`, which writes each
/// frame as `width` x `height` RGB pixels and nothing between them, as opaque RGBA. `None`
/// when not even one whole frame came.
fn newest_raw_frame(frames: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
    let size = width * height * 3;
    if size == 0 || frames.len() < size {
        return None;
    }
    let start = (frames.len() / size - 1) * size;
    Some(
        frames[start..start + size]
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
    )
}

/// The pixel at (x, y) of an RGBA picture `width` wide.
fn pixel(pixels: &[u8], width: usize, x: i32, y: i32) -> [u8; 3] {
    let at = (y as usize * width + x as usize) * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2]]
}

fn differs(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter()
        .zip(b.iter())
        .any(|(a, b)| (*a as i16 - *b as i16).abs() > 12)
}

/// How a list row's card is drawn: whether its top corners are rounded (the page shows at
/// the corner) or square (the card goes on to the row above), or None when the row shows no
/// card on the page.
fn top_corner(pixels: &[u8], width: usize, row: Rect) -> Option<bool> {
    let middle = (row.top + row.bottom) / 2;
    let page = pixel(pixels, width, row.left + 1, middle);
    // The card's left edge: SettingsLib insets it from the row's side.
    let edge = (row.left + 1..row.left + row.width() / 3)
        .find(|&x| differs(pixel(pixels, width, x, middle), page))?;
    let card = pixel(pixels, width, edge + 4, middle);
    // A card is inset as far on the right: a row of text on the page (a group's heading) is
    // not.
    let mirror = row.right - 1 - (edge - row.left) - 4;
    if mirror <= edge || differs(pixel(pixels, width, mirror, middle), card) {
        return None;
    }
    let corner = pixel(pixels, width, edge + 1, row.top + 1);
    if !differs(corner, page) {
        Some(true)
    } else if !differs(corner, card) {
        Some(false)
    } else {
        None
    }
}

/// Whether the rows of the list in `nodes` are grouped in rounded cards, read from the
/// screen `pixels`: some row's card has rounded top corners and some row's square ones.
fn rounded_cards(nodes: &[Node], pixels: &[u8], width: usize) -> Result<(), String> {
    let (list, list_node) = by_id(nodes, "recycler_view")
        .ok_or("the dump has no list (com.android.settings:id/recycler_view)")?;
    let height = (pixels.len() / (width * 4)) as i32;
    let rows: Vec<Rect> = nodes
        .iter()
        .filter(|node| node.parent == Some(list))
        .map(|node| node.bounds)
        .filter(|bounds| bounds.width() * 10 >= list_node.bounds.width() * 9 && bounds.height() > 8)
        // Whole rows on the screen only.
        .filter(|bounds| {
            bounds.left >= 0
                && bounds.top >= 0
                && bounds.right <= width as i32
                && bounds.bottom <= height
        })
        .collect();
    let corners: Vec<Option<bool>> = rows
        .iter()
        .map(|&row| top_corner(pixels, width, row))
        .collect();
    let rounded = corners.iter().filter(|c| **c == Some(true)).count();
    let square = corners.iter().filter(|c| **c == Some(false)).count();
    if rounded == 0 || square == 0 {
        return Err(format!(
            "of {} rows, {rounded} start a rounded card and {square} join the card above; a list of \
             grouped cards has both (rows {rows:?}, corners {corners:?})",
            rows.len()
        ));
    }
    Ok(())
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
        2 => 3,
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
    // RGBA, whatever the PNG carried.
    if channels == 3 {
        out = out
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
    }
    Ok((width, height, 4, out))
}

/// adb, talking to the guest through the service's developer mode port.
struct Adb {
    program: PathBuf,
    serial: String,
}

impl Adb {
    /// Runs adb with `args` against the guest, for [ADB_LIMIT] at most: a command that
    /// hangs fails the check with what it was, instead of holding it until the job's timeout.
    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let out = run_bounded(
            Command::new(&self.program)
                .arg("-s")
                .arg(&self.serial)
                .args(args),
            ADB_LIMIT,
            usize::MAX,
        )?;
        match out.status {
            Some(status) if status.success() => Ok(out.stdout),
            Some(_) => Err(format!(
                "adb {} failed: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )),
            None => Err(format!(
                "adb {} did not finish within {} s and was stopped",
                args.join(" "),
                ADB_LIMIT.as_secs()
            )),
        }
    }

    fn shell(&self, command: &str) -> Result<String, String> {
        self.run(&["shell", command])
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// Connects, and waits until the guest's shell answers.
    fn connect(&self, wait: Duration) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        let _ = run_bounded(
            Command::new(&self.program).arg("start-server"),
            ADB_LIMIT,
            usize::MAX,
        );
        loop {
            let _ = run_bounded(
                Command::new(&self.program).arg("connect").arg(&self.serial),
                ADB_LIMIT,
                usize::MAX,
            );
            match self.shell("echo ready") {
                Ok(out) if out.trim() == "ready" => return Ok(()),
                Ok(out) if Instant::now() >= deadline => {
                    return Err(format!("the guest's shell answered {out:?}"))
                }
                Err(why) if Instant::now() >= deadline => return Err(why),
                _ => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    }

    /// The view hierarchy of the focused window, kept in `dir` as `name.xml`.
    fn dump(&self, dir: &Path, name: &str) -> Result<Vec<Node>, String> {
        self.shell("uiautomator dump /data/local/tmp/dsa-screens.xml")?;
        let xml = self.run(&["exec-out", "cat", "/data/local/tmp/dsa-screens.xml"])?;
        let _ = std::fs::write(dir.join(format!("{name}.xml")), &xml);
        Ok(nodes(&String::from_utf8_lossy(&xml)))
    }

    /// The screen, kept in `dir` as `name.png`, as `(width, RGBA pixels)`.
    ///
    /// `screencap` maps SurfaceFlinger's capture for the CPU, and in the guest a buffer the
    /// GPU drew can map as nothing but zeros. Then the screen is read again from a
    /// `screenrecord` frame, which the GPU reads back itself (`glReadPixels`), and that
    /// picture is kept as `name-record.png`. screenrecord runs for [SCREENRECORD_LIMIT] at
    /// most, and stops once it has sent [SCREENRECORD_FRAMES] frames: a screenrecord that sends
    /// nothing held the whole check until the job's timeout.
    fn screen(&self, dir: &Path, name: &str) -> Result<(usize, Vec<u8>), String> {
        let png = self.run(&["exec-out", "screencap", "-p"])?;
        let _ = std::fs::write(dir.join(format!("{name}.png")), &png);
        let (width, height, _, pixels) = decode_png(&png)?;
        if !empty_picture(&pixels) {
            return Ok((width, pixels));
        }
        eprintln!(
            "screencap gave an empty picture of {name} ({width}x{height}); reading a screenrecord frame instead"
        );
        let size = format!("{width}x{height}");
        let recorded = run_bounded(
            Command::new(&self.program)
                .arg("-s")
                .arg(&self.serial)
                .args([
                    "exec-out",
                    "screenrecord",
                    "--output-format=raw-frames",
                    "--size",
                    size.as_str(),
                    "--time-limit",
                    "3",
                    "-",
                ]),
            SCREENRECORD_LIMIT,
            width * height * 3 * SCREENRECORD_FRAMES,
        )?;
        let pixels = newest_raw_frame(&recorded.stdout, width, height)
            .ok_or_else(|| screenrecord_failure(name, &size, &recorded))?;
        let picture = dsa_host_linux::shell::png::Rgba {
            width: width as u32,
            height: height as u32,
            pixels,
        };
        let _ = std::fs::write(
            dir.join(format!("{name}-record.png")),
            dsa_host_linux::shell::png::encode(&picture, false),
        );
        if empty_picture(&picture.pixels) {
            return Err(format!(
                "both screencap and screenrecord gave an empty picture of {name}"
            ));
        }
        Ok((width, picture.pixels))
    }

    /// The overlays that apply to Settings and their state, kept in `dir` as `overlays.log`.
    fn settings_overlays(&self, dir: &Path) -> String {
        let listed = self
            .shell(&format!("cmd overlay list {SETTINGS}"))
            .unwrap_or_else(|why| why);
        let _ = std::fs::write(dir.join("overlays.log"), &listed);
        listed
    }
}

/// Why the screenrecord fallback gave no picture of `name`, which is `size` large.
fn screenrecord_failure(name: &str, size: &str, recorded: &boot_check::Bounded) -> String {
    let how = match recorded.status {
        _ if recorded.timed_out => format!(
            "did not finish within {} s and was stopped",
            SCREENRECORD_LIMIT.as_secs()
        ),
        Some(status) => format!("ended ({status})"),
        None => "was stopped".to_string(),
    };
    format!(
        "screencap gave an empty picture of {name}, and screenrecord {how} after sending {} bytes in {:.1} s, not one whole {size} frame{}",
        recorded.stdout.len(),
        recorded.took.as_secs_f64(),
        match String::from_utf8_lossy(&recorded.stderr).trim() {
            "" => String::new(),
            said => format!("; it said: {said}"),
        }
    )
}

/// The density a task's app draws at, in dpi: the one Android reports, or 160 (a scale of
/// 100%) when it reports none.
fn drawing_dpi(density_dpi: u32) -> u32 {
    if density_dpi == 0 {
        160
    } else {
        density_dpi
    }
}

/// `dp` in pixels at `dpi`.
fn px_at(dp: u32, dpi: u32) -> u32 {
    dp * dpi / 160
}

/// `px` pixels in dp at `dpi`.
fn dp_at(px: u32, dpi: u32) -> u32 {
    px * 160 / dpi
}

/// What to do next to have a page laid out in a task window of a size in dp.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Ask for the window in pixels.
    Resize { width: u32, height: u32 },
    /// The size asked for has not come yet.
    Wait,
    /// The window has the size: open the page afresh in it.
    Open,
    /// The page was opened in the window as it still is, `height_dp` tall.
    Ready { height_dp: u32 },
}

/// Brings a task window to `width_dp` x `height_dp` and has the page opened in it at that size.
///
/// A size in dp is pixels at the density the task's app draws at now, which can change after
/// the task appears: the window manager gives the task the density of the monitor its window
/// is on, and until then the task has the display's. So each look at the task converts with
/// the density it has then, asks for the window again when that gives another size, and opens
/// the page again when the window changed after the page was opened in it.
struct Sizing {
    width_dp: u32,
    height_dp: u32,
    /// The size last asked for, in pixels.
    asked: Option<(u32, u32)>,
    /// The density and the size in pixels the window had when the page was opened in it.
    opened: Option<(u32, u32, u32)>,
}

impl Sizing {
    fn new(width_dp: u32, height_dp: u32) -> Self {
        Sizing {
            width_dp,
            height_dp,
            asked: None,
            opened: None,
        }
    }

    /// The next step for a window `width` x `height` pixels whose app draws at `density_dpi`.
    fn step(&mut self, width: u32, height: u32, density_dpi: u32) -> Step {
        let dpi = drawing_dpi(density_dpi);
        let height_dp = dp_at(height, dpi);
        if height_dp.abs_diff(self.height_dp) > 2 {
            self.opened = None;
            let wanted = (px_at(self.width_dp, dpi), px_at(self.height_dp, dpi));
            if self.asked == Some(wanted) {
                return Step::Wait;
            }
            self.asked = Some(wanted);
            return Step::Resize {
                width: wanted.0,
                height: wanted.1,
            };
        }
        let now = (dpi, width, height);
        if self.opened == Some(now) {
            return Step::Ready { height_dp };
        }
        self.opened = Some(now);
        Step::Open
    }
}

/// Gives Settings' task a window `height_dp` tall, then opens its home page afresh in it, so
/// the page is laid out for that height: the home page keeps its views when its window is
/// resized. Returns the task's height in dp as Android has it once the page opened in it.
fn open_settings_at(
    api: &mut HostApiClient,
    adb: &Adb,
    height_dp: u32,
    wait: Duration,
) -> Result<u32, String> {
    let deadline = Instant::now() + wait;
    // The tasks Settings had before it is stopped. The host can still list one of them for
    // a moment after the force-stop removes it, so only a task that was not there before is
    // the one this launch opens.
    let before: Vec<i32> = api
        .tasks()
        .map_err(|why| format!("listing tasks: {why}"))?
        .tasks
        .into_iter()
        .filter(|task| task.package_name == SETTINGS)
        .map(|task| task.task_id)
        .collect();
    let _ = adb.shell(&format!("am force-stop {SETTINGS}"));
    api.launch_app(SETTINGS)
        .map_err(|why| format!("opening Settings: {why}"))?;
    let mut task_id = None;
    let mut sizing = Sizing::new(WIDTH_DP, height_dp);
    loop {
        let tasks = api.tasks().map_err(|why| format!("listing tasks: {why}"))?;
        let new = |task: &dsa_host_core::tasks::Task| {
            task.package_name == SETTINGS && !before.contains(&task.task_id)
        };
        // The task this launch opened, or the one Settings has after its page was opened
        // afresh, should that be another.
        let task = tasks
            .tasks
            .iter()
            .find(|task| Some(task.task_id) == task_id && new(task))
            .or_else(|| tasks.tasks.iter().find(|task| new(task)));
        let Some(task) = task else {
            if Instant::now() >= deadline {
                return Err(match task_id {
                    Some(_) => "Settings' task went away".into(),
                    None => "Settings' task never appeared".into(),
                });
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        task_id = Some(task.task_id);
        let (width, height) = (task.bounds.width(), task.bounds.height());
        let step = sizing.step(width, height, task.density_dpi);
        if Instant::now() >= deadline && !matches!(step, Step::Ready { .. }) {
            let dpi = drawing_dpi(task.density_dpi);
            return Err(format!(
                "Settings' task is {width}x{height} pixels at {dpi} dpi, {} dp tall, and did \
                 not settle at the {height_dp} dp asked for with its home page opened in it",
                dp_at(height, dpi)
            ));
        }
        match step {
            Step::Resize { width, height } => {
                api.resize_task(task.task_id, width, height, false)
                    .map_err(|why| format!("resizing Settings' task: {why}"))?;
                std::thread::sleep(Duration::from_millis(500));
            }
            Step::Wait => std::thread::sleep(Duration::from_millis(500)),
            Step::Open => {
                adb.shell(&format!(
                    "am start -W --activity-clear-task -n {SETTINGS_HOME}"
                ))?;
                // The page lays out and draws its list.
                std::thread::sleep(Duration::from_secs(5));
            }
            Step::Ready { height_dp } => return Ok(height_dp),
        }
    }
}

#[test]
fn fr34_the_dump_parser_reads_nodes_their_parents_and_bounds() {
    let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="com.android.settings:id/app_bar" class="android.widget.LinearLayout" bounds="[0,0][480,300]"><node index="0" text="Settings &amp; more" resource-id="" class="android.widget.TextView" bounds="[0,0][480,200]" /><node index="1" text="" resource-id="com.android.settings:id/search_action_bar" class="android.widget.LinearLayout" bounds="[16,208][464,264]"><node index="0" text="Search" resource-id="com.android.settings:id/search_bar_title" class="android.widget.TextView" bounds="[60,220][120,250]" /></node></node></hierarchy>"#;
    let found = nodes(xml);
    assert_eq!(found.len(), 4);
    assert_eq!(found[1].text, "Settings & more");
    assert_eq!(found[1].parent, Some(0));
    assert_eq!(found[3].parent, Some(2));
    assert_eq!(
        found[2].bounds,
        Rect {
            left: 16,
            top: 208,
            right: 464,
            bottom: 264
        }
    );
    // The title above the search bar, more than twice its height: expanded.
    assert_eq!(
        large_title(&found).unwrap(),
        Some(Rect {
            left: 0,
            top: 0,
            right: 480,
            bottom: 200
        })
    );
    // Without it, folded.
    let folded: Vec<Node> = found
        .iter()
        .filter(|n| n.text != "Settings & more")
        .cloned()
        .collect();
    assert_eq!(large_title(&folded).unwrap(), None);
}

#[test]
fn fr34_a_window_s_height_in_dp_is_read_at_the_density_its_app_draws_at() {
    // 692 pixels are under 600 dp at the display's 213 dpi, but not at the 160 dpi the
    // window manager gives a task on a monitor at a scale of 100%: there the large title
    // rightly opens expanded.
    assert_eq!(dp_at(692, drawing_dpi(213)), 519);
    assert_eq!(dp_at(692, drawing_dpi(160)), 692);
    assert_eq!(px_at(SHORT_DP, drawing_dpi(160)), SHORT_DP);
    // A task whose density is not known draws at a scale of 100%.
    assert_eq!(drawing_dpi(0), 160);
}

#[test]
fn fr34_a_window_sized_before_its_density_changed_is_sized_and_opened_again() {
    let mut sizing = Sizing::new(WIDTH_DP, SHORT_DP);
    // Listed at the display's 213 dpi: asked for 520 dp in those pixels.
    assert_eq!(
        sizing.step(639, 1065, 213),
        Step::Resize {
            width: 639,
            height: 692
        }
    );
    assert_eq!(sizing.step(639, 1065, 213), Step::Wait);
    assert_eq!(sizing.step(639, 692, 213), Step::Open);
    // The window manager then sets the monitor's 160 dpi: the window is 692 dp tall, and the
    // page opened in it is not the short window's.
    assert_eq!(
        sizing.step(639, 692, 160),
        Step::Resize {
            width: 480,
            height: 520
        }
    );
    assert_eq!(sizing.step(480, 520, 160), Step::Open);
    assert_eq!(sizing.step(480, 520, 160), Step::Ready { height_dp: 520 });
}

#[test]
fn fr34_a_page_counts_as_opened_at_a_size_only_while_its_window_keeps_it() {
    let mut sizing = Sizing::new(WIDTH_DP, TALL_DP);
    assert_eq!(sizing.step(480, 800, 160), Step::Open);
    // The window moved to a monitor of another scale after the page opened: the same pixels
    // are another size, so the page is opened again once the window has the size again.
    assert_eq!(
        sizing.step(480, 800, 240),
        Step::Resize {
            width: 720,
            height: 1200
        }
    );
    assert_eq!(sizing.step(720, 1200, 240), Step::Open);
    assert_eq!(sizing.step(720, 1200, 240), Step::Ready { height_dp: 800 });
}

#[test]
fn fr34_the_card_check_tells_rounded_cards_from_square_rows() {
    // A 100x60 screen in the page's grey, with a white card from x 10 to 90 over two rows
    // (y 0..30 and 30..60): the top row's top corners cut round, the second row's square.
    let (width, height) = (100usize, 60usize);
    let mut pixels = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let in_card = (10..90).contains(&x);
            let cut = y < 4 && !(14..86).contains(&x);
            let colour = if in_card && !cut { 255 } else { 230 };
            pixels[(y * width + x) * 4..(y * width + x) * 4 + 4]
                .copy_from_slice(&[colour, colour, colour, 255]);
        }
    }
    let top = Rect {
        left: 0,
        top: 0,
        right: 100,
        bottom: 30,
    };
    let joined = Rect {
        left: 0,
        top: 30,
        right: 100,
        bottom: 60,
    };
    assert_eq!(top_corner(&pixels, width, top), Some(true));
    assert_eq!(top_corner(&pixels, width, joined), Some(false));
    let list = |rows: &[Rect]| {
        let mut all = vec![Node {
            parent: None,
            class: "androidx.recyclerview.widget.RecyclerView".into(),
            text: String::new(),
            id: format!("{SETTINGS}:id/recycler_view"),
            bounds: Rect {
                left: 0,
                top: 0,
                right: 100,
                bottom: 60,
            },
        }];
        all.extend(rows.iter().map(|&bounds| Node {
            parent: Some(0),
            class: "android.widget.LinearLayout".into(),
            text: String::new(),
            id: String::new(),
            bounds,
        }));
        all
    };
    assert!(rounded_cards(&list(&[top, joined]), &pixels, width).is_ok());
    // Every row a card of its own, or no card at all, is not a list of grouped cards.
    assert!(rounded_cards(&list(&[top]), &pixels, width).is_err());
}

#[test]
fn fr34_an_empty_capture_is_read_again_from_the_newest_whole_screenrecord_frame() {
    // A capture that never received the screen is all zeros; a drawn screen is opaque.
    assert!(empty_picture(&[0; 16]));
    assert!(!empty_picture(&[0, 0, 0, 255]));
    // Two 2x1 RGB frames and part of a third: the second is the newest whole one.
    let frames = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];
    assert_eq!(
        newest_raw_frame(&frames, 2, 1),
        Some(vec![7, 8, 9, 255, 10, 11, 12, 255])
    );
    assert_eq!(newest_raw_frame(&frames[..5], 2, 1), None);
    assert_eq!(newest_raw_frame(&frames, 0, 1), None);
}

#[test]
fn fr34_a_screenrecord_that_sends_nothing_is_stopped_at_its_limit_with_a_reason() {
    // A command that never ends and never writes stands in for a screenrecord that hangs.
    let started = Instant::now();
    let recorded = run_bounded(
        Command::new("sh").args(["-c", "echo no frames >&2; exec sleep 600"]),
        Duration::from_secs(2),
        usize::MAX,
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(recorded.timed_out);
    assert!(recorded.status.is_none());
    let why = screenrecord_failure("display", "1920x1080", &recorded);
    assert!(
        why.contains("did not finish within 30 s and was stopped"),
        "{why}"
    );
    assert!(why.contains("after sending 0 bytes"), "{why}");
    assert!(why.contains("not one whole 1920x1080 frame"), "{why}");
    assert!(why.contains("it said: no frames"), "{why}");

    // One that keeps writing is stopped once it has written enough, well before its limit.
    let started = Instant::now();
    let recorded = run_bounded(
        Command::new("sh").args(["-c", "exec yes"]),
        Duration::from_secs(60),
        64 * 1024,
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(!recorded.timed_out);
    assert!(recorded.stdout.len() >= 64 * 1024);

    // One that ends gives all it wrote.
    let recorded = run_bounded(
        Command::new("sh").args(["-c", "printf abc"]),
        Duration::from_secs(20),
        usize::MAX,
    )
    .unwrap();
    assert!(recorded.status.is_some_and(|status| status.success()));
    assert_eq!(recorded.stdout, b"abc");
}

#[test]
fn fr34_settings_opens_with_a_large_title_that_folds_and_lists_its_rows_in_rounded_cards() {
    let (Some(crosvm), Some(image)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_WIDGET_TEST_IMAGE name no crosvm and image");
        return;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let adb_program = std::env::var_os("DSA_ADB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("adb"));
    let started = Instant::now();
    let deadline = started + BUDGET;
    let _watchdog = Watchdog::start("the system screens check", BUDGET + OVERRUN);
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
    // Android's renderer is started by the window manager, which shows Android's task windows
    // in the desktop session the job runs (a headless Wayland compositor).
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
    let boot = deadline.saturating_duration_since(Instant::now()) - Duration::from_secs(240);
    if let Err(why) = api.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    }) {
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

    // Developer mode, so adb reaches the guest through the service's loopback port.
    let mut wanted = match api.settings() {
        Ok(report) => report.settings,
        Err(why) => fail(format!("reading the settings: {why}"), &service, &log),
    };
    wanted.developer_mode = true;
    wanted.adb_auto_connect = false;
    let port = wanted.adb_port;
    if let Err(why) = api.set_settings(&wanted) {
        fail(format!("turning developer mode on: {why}"), &service, &log);
    }
    let adb = Adb {
        program: adb_program,
        serial: format!("127.0.0.1:{port}"),
    };
    if let Err(why) = adb.connect(Duration::from_secs(120)) {
        fail(
            format!("adb did not reach the guest: {why}"),
            &service,
            &log,
        );
    }

    // Each check that fails is noted and the others still run, so one failure does not hide
    // the next; the test fails at the end with all of them.
    let mut failures: Vec<String> = Vec::new();

    // A tall window: the home page opens with its large title, which folds as the list scrolls.
    let wait = Duration::from_secs(60);
    let height = match open_settings_at(&mut api, &adb, TALL_DP, wait) {
        Ok(height) => height,
        Err(why) => fail(why, &service, &log),
    };
    if height < 600 {
        fail(
            format!("Settings' window is {height} dp tall, not the {TALL_DP} dp asked for"),
            &service,
            &log,
        );
    }
    let tall = adb
        .dump(&dir, "home-tall")
        .unwrap_or_else(|why| fail(why, &service, &log));
    let _ = adb.screen(&dir, "home-tall");
    match large_title(&tall) {
        Ok(Some(title)) => {
            eprintln!("the large title is at {title:?} in a window {height} dp tall")
        }
        Ok(None) => failures.push(format!(
            "Settings opened in a window {height} dp tall without its large title"
        )),
        Err(why) => failures.push(why),
    }
    match by_id(&tall, "main_content_scrollable_container").map(|(_, node)| node.bounds) {
        Some(scroll) => {
            let x = (scroll.left + scroll.right) / 2;
            match adb.shell(&format!(
                "input swipe {x} {} {x} {} 600",
                scroll.bottom - 20,
                scroll.top + 20
            )) {
                Ok(_) => {
                    std::thread::sleep(Duration::from_secs(2));
                    let scrolled = adb
                        .dump(&dir, "home-tall-scrolled")
                        .unwrap_or_else(|why| fail(why, &service, &log));
                    let _ = adb.screen(&dir, "home-tall-scrolled");
                    match large_title(&scrolled) {
                        Ok(None) => {}
                        Ok(Some(still)) => failures.push(format!(
                            "the large title stayed at {still:?} when the list scrolled"
                        )),
                        Err(why) => failures.push(why),
                    }
                }
                Err(why) => failures.push(format!("scrolling the home page: {why}")),
            }
        }
        None => failures
            .push("the dump has no home page list (main_content_scrollable_container)".into()),
    }

    // A Settings list, in the same window: its rows are grouped in rounded cards.
    if let Err(why) = adb.shell(&format!("am start -W -a {DISPLAY_SETTINGS}")) {
        fail(why, &service, &log);
    }
    std::thread::sleep(Duration::from_secs(5));
    let overlays = adb.settings_overlays(&dir);
    eprintln!("the overlays on Settings:\n{}", overlays.trim_end());
    let page = adb
        .dump(&dir, "display")
        .unwrap_or_else(|why| fail(why, &service, &log));
    match adb.screen(&dir, "display") {
        Ok((width, pixels)) => {
            if let Err(why) = rounded_cards(&page, &pixels, width) {
                failures.push(format!(
                    "Settings' display page: {why}\nthe overlays on Settings:\n{}",
                    overlays.trim_end()
                ));
            }
        }
        Err(why) => failures.push(format!("reading Settings' display page: {why}")),
    }

    // A short window: the home page opens folded.
    let height = match open_settings_at(&mut api, &adb, SHORT_DP, wait) {
        Ok(height) => height,
        Err(why) => fail(why, &service, &log),
    };
    if height >= 600 {
        failures.push(format!(
            "Settings' window is {height} dp tall, not the {SHORT_DP} dp asked for"
        ));
    } else {
        let short = adb
            .dump(&dir, "home-short")
            .unwrap_or_else(|why| fail(why, &service, &log));
        let _ = adb.screen(&dir, "home-short");
        match large_title(&short) {
            Ok(None) => {}
            Ok(Some(title)) => failures.push(format!(
                "Settings opened in a window {height} dp tall with its large title at {title:?}"
            )),
            Err(why) => failures.push(why),
        }
    }

    if !failures.is_empty() {
        fail(failures.join("\n\n"), &service, &log);
    }

    let _ = api.stop();
    drop(service);
    eprintln!(
        "the system screen test took {} s; its pictures are in {}",
        started.elapsed().as_secs(),
        dir.display()
    );
}

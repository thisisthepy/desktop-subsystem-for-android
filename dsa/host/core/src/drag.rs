//! Files and text an app drags out of its window onto the host.
//!
//! An app in the guest starts a drag with `View.startDragAndDrop` and `DRAG_FLAG_GLOBAL`.
//! When the pointer leaves the app's host window with the button still held, the window host
//! asks the service ([`DragOut`]), which asks the guest's agent. The agent ends the drag in the
//! guest on a window of its own, takes read access to what it carries, and answers with the
//! items ([`DragOffer`]): each file's name, size and type, or the text. The window host then
//! starts a native drag on the host with those items. Only when a host app takes the drop does
//! it read each file, in pieces ([`DragContent`]), over the same control channel as everything
//! else; the guest streams it from the app's content provider. Ending the drag ([`DragEnd`])
//! closes what was read and gives back the guest's access, whether it was dropped or not.
//!
//! The host never learns a content URI or a path in the guest: an item is a name and bytes.
//! [`safe_name`] makes sure the name cannot reach outside the folder it is dropped in.

use std::io::{self, Read};

use dsa_protocol::control;
use dsa_protocol::host_api::types;

/// The most bytes one read of a dragged file asks for: 256 KiB, half the agent's share of a
/// one-way binder call, so an answer always fits.
pub const READ_BYTES: u32 = 256 * 1024;

/// The longest name a dragged file keeps, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// What one drag carries: a file or text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragItem {
    /// The file's name, already made safe ([`safe_name`]). Empty for text.
    pub name: String,
    /// The file's length in bytes, when the guest knows it.
    pub size: Option<u64>,
    /// The MIME type the app gave.
    pub mime_type: String,
    /// The text, for an item that is text rather than a file.
    pub text: Option<String>,
}

impl DragItem {
    /// Whether the item is a file, whose content is read from the guest.
    pub fn is_file(&self) -> bool {
        self.text.is_none()
    }

    /// The item as the agent sent it, its name made safe; `index` names an unnamed file.
    pub fn from_control(item: &control::DragItem, index: usize) -> DragItem {
        if item.is_text {
            return DragItem {
                name: String::new(),
                size: None,
                mime_type: item.mime_type.clone(),
                text: Some(item.text.clone()),
            };
        }
        DragItem {
            name: safe_name(&item.name, &item.mime_type, index),
            size: item.size_known.then_some(item.size),
            mime_type: item.mime_type.clone(),
            text: None,
        }
    }

    /// The item on the local API.
    pub fn to_wire(&self) -> types::DragItem {
        types::DragItem {
            name: self.name.clone(),
            size: self.size.unwrap_or(0),
            size_known: self.size.is_some(),
            mime_type: self.mime_type.clone(),
            is_text: self.text.is_some(),
            text: self.text.clone().unwrap_or_default(),
        }
    }

    /// The item from the local API, its name made safe again: the client does not trust the
    /// service any more than the service trusts the guest.
    pub fn from_wire(item: types::DragItem, index: usize) -> DragItem {
        if item.is_text {
            return DragItem {
                name: String::new(),
                size: None,
                mime_type: item.mime_type,
                text: Some(item.text),
            };
        }
        DragItem {
            name: safe_name(&item.name, &item.mime_type, index),
            size: item.size_known.then_some(item.size),
            mime_type: item.mime_type,
            text: None,
        }
    }
}

/// A drag the guest handed over: what it carries, under the ID the reads name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragOffer {
    pub drag_id: u32,
    pub items: Vec<DragItem>,
}

impl DragOffer {
    /// The files among the items, with their indices in the drag.
    pub fn files(&self) -> impl Iterator<Item = (u32, &DragItem)> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_file())
            .map(|(index, item)| (index as u32, item))
    }

    /// The text of the drag, when it carries any: every text item, one per line.
    pub fn text(&self) -> Option<String> {
        let texts: Vec<&str> = self
            .items
            .iter()
            .filter_map(|item| item.text.as_deref())
            .collect();
        (!texts.is_empty()).then(|| texts.join("\n"))
    }
}

/// Where the pointer left the window: the task, the place relative to it, and the tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragOut {
    pub task_id: i32,
    pub x: f32,
    pub y: f32,
    /// A [`control::PointerTool`] as its number.
    pub tool: i32,
    pub pointer_id: u32,
}

/// Watches a task window's pointer for the moment a press drags out of it: the first move
/// outside the window while the primary button (or a finger or pen) is down, once per press.
/// That is when the window host asks whether an app is dragging something out.
#[derive(Debug, Default)]
pub struct LeaveWatch {
    down: bool,
    asked: bool,
    inside: Option<(f64, f64)>,
}

impl LeaveWatch {
    /// The primary button (or a contact) went down or up.
    pub fn pressed(&mut self, down: bool) {
        self.down = down;
        if down {
            self.asked = false;
        }
    }

    /// The press ended without completing, as when the window lost the pointer.
    pub fn cancel(&mut self) {
        self.down = false;
    }

    /// The pointer moved to `position` in a client area of `client` pixels. The last place
    /// inside the window when the press has just left it, which is where the guest's drag
    /// leaves the task; `None` otherwise.
    pub fn moved(&mut self, position: (f64, f64), client: (u32, u32)) -> Option<(f64, f64)> {
        let (x, y) = position;
        let inside =
            (0.0..f64::from(client.0)).contains(&x) && (0.0..f64::from(client.1)).contains(&y);
        if inside {
            self.inside = Some(position);
            return None;
        }
        if !self.down || self.asked {
            return None;
        }
        self.asked = true;
        Some(self.inside.unwrap_or((
            x.clamp(0.0, f64::from(client.0.saturating_sub(1))),
            y.clamp(0.0, f64::from(client.1.saturating_sub(1))),
        )))
    }
}

/// A request for part of a dragged file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragRead {
    pub drag_id: u32,
    pub item: u32,
    pub offset: u64,
    pub length: u32,
}

/// Part of a dragged file: its bytes, and whether the file ends after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragChunk {
    pub data: Vec<u8>,
    pub end: bool,
}

/// A name for a dragged file that stays in the folder it is dropped in: the last part of
/// whatever the guest sent, without separators, `..`, control characters or the characters a
/// Windows file name cannot hold, and no longer than [`MAX_NAME_BYTES`]. A name with nothing
/// left becomes `file-<n>`, with an extension from `mime_type` when it is a common one.
pub fn safe_name(name: &str, mime_type: &str, index: usize) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    // Windows drops trailing dots and spaces, which would turn "a." into "a" and "..." into
    // nothing; leading dots and spaces are trimmed so ".." and "." leave nothing behind.
    clean = clean
        .trim_matches(|c: char| c == ' ' || c == '.')
        .to_string();
    if is_reserved_device(&clean) {
        clean.insert(0, '_');
    }
    if clean.is_empty() {
        clean = format!("file-{}{}", index + 1, extension_of(mime_type));
    }
    if clean.len() > MAX_NAME_BYTES {
        clean = shorten(&clean, MAX_NAME_BYTES);
    }
    clean
}

/// Whether `name` is one Windows keeps for a device, such as `CON` or `com1.txt`.
fn is_reserved_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

/// `name` cut to at most `limit` bytes, on a character boundary, keeping its extension.
fn shorten(name: &str, limit: usize) -> String {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if name.len() - dot <= 16 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    let mut cut = limit.saturating_sub(extension.len());
    while !stem.is_char_boundary(cut.min(stem.len())) {
        cut -= 1;
    }
    format!("{}{extension}", &stem[..cut.min(stem.len())])
}

/// The usual extension of a MIME type, with its dot, or nothing.
fn extension_of(mime_type: &str) -> &'static str {
    match mime_type.to_ascii_lowercase().as_str() {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "video/mp4" => ".mp4",
        "audio/mpeg" => ".mp3",
        "audio/ogg" => ".ogg",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "text/plain" => ".txt",
        "text/html" => ".html",
        _ => "",
    }
}

/// Reads one dragged file from the start, in pieces of [`READ_BYTES`], through `read`, which
/// asks the guest for one piece. A host drag target copies the file through this.
pub struct DragContent<F>
where
    F: FnMut(DragRead) -> Result<DragChunk, String>,
{
    drag_id: u32,
    item: u32,
    offset: u64,
    buffer: Vec<u8>,
    position: usize,
    ended: bool,
    read: F,
}

impl<F> DragContent<F>
where
    F: FnMut(DragRead) -> Result<DragChunk, String>,
{
    pub fn new(drag_id: u32, item: u32, read: F) -> Self {
        DragContent {
            drag_id,
            item,
            offset: 0,
            buffer: Vec::new(),
            position: 0,
            ended: false,
            read,
        }
    }
}

impl<F> Read for DragContent<F>
where
    F: FnMut(DragRead) -> Result<DragChunk, String>,
{
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.position == self.buffer.len() {
            if self.ended {
                return Ok(0);
            }
            let chunk = (self.read)(DragRead {
                drag_id: self.drag_id,
                item: self.item,
                offset: self.offset,
                length: READ_BYTES,
            })
            .map_err(io::Error::other)?;
            self.offset += chunk.data.len() as u64;
            self.ended = chunk.end;
            if chunk.data.is_empty() && !chunk.end {
                return Err(io::Error::other(
                    "Android sent an empty piece of a file that has not ended",
                ));
            }
            self.buffer = chunk.data;
            self.position = 0;
        }
        let n = out.len().min(self.buffer.len() - self.position);
        out[..n].copy_from_slice(&self.buffer[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr29_a_dragged_name_never_leaves_the_folder_it_is_dropped_in() {
        assert_eq!(safe_name("holiday.jpg", "image/jpeg", 0), "holiday.jpg");
        assert_eq!(safe_name("../../Windows/evil.exe", "", 0), "evil.exe");
        assert_eq!(safe_name("C:\\Users\\me\\x.txt", "", 0), "x.txt");
        assert_eq!(safe_name("..", "image/png", 2), "file-3.png");
        assert_eq!(safe_name("", "text/plain", 0), "file-1.txt");
        assert_eq!(safe_name("a:b*c?.txt", "", 0), "abc.txt");
        assert_eq!(safe_name("CON.txt", "", 0), "_CON.txt");
        assert_eq!(safe_name("line\nbreak", "", 0), "linebreak");
        let long = format!("{}.jpg", "\u{d55c}".repeat(200));
        let short = safe_name(&long, "", 0);
        assert!(short.len() <= MAX_NAME_BYTES && short.ends_with(".jpg"));
    }

    #[test]
    fn fr29_a_dragged_file_is_read_whole_in_pieces() {
        let content: Vec<u8> = (0..(READ_BYTES as usize * 2 + 7))
            .map(|i| i as u8)
            .collect();
        let source = content.clone();
        let mut reads = 0;
        let mut reader = DragContent::new(4, 1, |read: DragRead| {
            reads += 1;
            assert_eq!((read.drag_id, read.item), (4, 1));
            let start = read.offset as usize;
            let end = (start + read.length as usize).min(source.len());
            Ok(DragChunk {
                data: source[start..end].to_vec(),
                end: end == source.len(),
            })
        });
        let mut copied = Vec::new();
        reader.read_to_end(&mut copied).unwrap();
        assert_eq!(copied, content);
        drop(reader);
        assert_eq!(reads, 3);
    }

    #[test]
    fn fr29_a_press_that_leaves_the_window_asks_once_from_the_last_place_inside() {
        let mut watch = LeaveWatch::default();
        assert_eq!(
            watch.moved((-5.0, 10.0), (200, 100)),
            None,
            "no press, no drag"
        );
        watch.pressed(true);
        assert_eq!(watch.moved((50.0, 40.0), (200, 100)), None);
        assert_eq!(watch.moved((199.0, 40.0), (200, 100)), None);
        assert_eq!(watch.moved((230.0, 41.0), (200, 100)), Some((199.0, 40.0)));
        assert_eq!(
            watch.moved((260.0, 42.0), (200, 100)),
            None,
            "once per press"
        );
        watch.pressed(false);
        watch.pressed(true);
        assert_eq!(watch.moved((10.0, -1.0), (200, 100)), Some((199.0, 40.0)));
        watch.cancel();
        assert_eq!(watch.moved((10.0, -9.0), (200, 100)), None);
    }

    #[test]
    fn fr29_a_failed_read_is_an_error_not_a_short_file() {
        let mut reader = DragContent::new(1, 0, |_| Err("the app's file went away".into()));
        let mut copied = Vec::new();
        assert!(reader.read_to_end(&mut copied).is_err());
    }

    #[test]
    fn fr29_a_text_drag_carries_its_text_and_no_file() {
        let offer = DragOffer {
            drag_id: 1,
            items: vec![DragItem::from_control(
                &control::DragItem {
                    is_text: true,
                    text: "hello".into(),
                    mime_type: "text/plain".into(),
                    ..Default::default()
                },
                0,
            )],
        };
        assert_eq!(offer.text().as_deref(), Some("hello"));
        assert_eq!(offer.files().count(), 0);
    }
}

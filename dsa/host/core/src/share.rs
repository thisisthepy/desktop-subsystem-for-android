//! "Share on PC": what apps in the user's Android share with the host's own share UI.
//!
//! An app shares as it would on a phone, through Android's share sheet. The agent's "Share on
//! PC" target takes the text, links and files (`ACTION_SEND`, `ACTION_SEND_MULTIPLE`), holds
//! read access to the files, and sends the host a `ShareToHost` with each item's name, size
//! and type, or its text. The session checks it ([`ShareRequest::from_control`]) and hands it
//! to one watcher in the user's session ([`serve`]). The watcher copies the files from the
//! guest into a folder of the user's ([`copy_files`]), over the same reads as a drag out of a
//! window, ends the share in the guest so it gives the access back, and shows the host's share
//! UI ([`ShareUi`]) with the same content: Windows' share window with Nearby sharing, the
//! macOS share menu, or the desktop portal's chooser on Linux. What became of it goes back to
//! the guest as a `ShareResult`.
//!
//! The host never learns a content URI or a path in the guest: an item is a name and bytes,
//! and [`crate::drag::safe_name`] keeps each name inside the folder it is copied to. Nothing a
//! guest sends ends the session or the watcher.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use dsa_protocol::control;
use dsa_protocol::host_api::types;
use dsa_transport::Connector;

use crate::drag::{self, DragChunk, DragItem, DragRead};
use crate::host_api::HostApiClient;

/// The most items one share carries.
pub const MAX_SHARE_ITEMS: usize = 100;

/// The most bytes the files of one share copy to the host, together.
pub const MAX_SHARE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The longest title or app name the host keeps, in characters.
pub const MAX_TITLE_CHARS: usize = 256;

/// The longest text the host takes from one share, in characters.
pub const MAX_TEXT_CHARS: usize = 1024 * 1024;

/// How long the copied files of a share are kept. The app the user shared with may read them
/// some time after the share UI closes, as a mail app does when it sends.
pub const KEEP_FILES: Duration = Duration::from_secs(24 * 60 * 60);

/// One share from an app, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRequest {
    /// The ID its files are read and its share ended with.
    pub share_id: u32,
    pub title: String,
    pub package_name: String,
    pub app_label: String,
    pub items: Vec<DragItem>,
}

/// What became of a share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareOutcome {
    Shared,
    Canceled,
    Failed(String),
}

/// What the host's share UI is given: the text, the links and the copied files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareContent {
    /// The subject or title the app gave, or the app's name; never empty.
    pub title: String,
    /// The text that is not a link, every text item one per line; `None` when there is none.
    pub text: Option<String>,
    /// The web links among the text items, in order.
    pub links: Vec<String>,
    /// The copied files, in order.
    pub files: Vec<PathBuf>,
}

fn clean(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

impl ShareRequest {
    fn checked(
        share_id: u32,
        title: &str,
        package_name: &str,
        app_label: &str,
        items: Vec<DragItem>,
    ) -> Result<ShareRequest, String> {
        if items.is_empty() {
            return Err("the share carries nothing".into());
        }
        if items.len() > MAX_SHARE_ITEMS {
            return Err(format!(
                "the share carries {} items, more than the {MAX_SHARE_ITEMS} the host takes",
                items.len()
            ));
        }
        let mut text_chars = 0usize;
        for item in &items {
            if let Some(text) = &item.text {
                text_chars += text.chars().count();
            }
        }
        if text_chars > MAX_TEXT_CHARS {
            return Err("the shared text is longer than the host takes".into());
        }
        Ok(ShareRequest {
            share_id,
            title: clean(title, MAX_TITLE_CHARS),
            package_name: clean(package_name, MAX_TITLE_CHARS),
            app_label: clean(app_label, MAX_TITLE_CHARS),
            items,
        })
    }

    /// The share the agent sent, checked, each file's name made safe.
    pub fn from_control(share: &control::ShareToHost) -> Result<ShareRequest, String> {
        if share.share_id == 0 {
            return Err("the share has no ID to read its files with".into());
        }
        if share.items.len() > MAX_SHARE_ITEMS {
            return Err(format!(
                "the share carries {} items, more than the {MAX_SHARE_ITEMS} the host takes",
                share.items.len()
            ));
        }
        let items = share
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| DragItem::from_control(item, index))
            .collect();
        ShareRequest::checked(
            share.share_id,
            &share.title,
            &share.package_name,
            &share.app_label,
            items,
        )
    }

    pub fn to_host_api(&self) -> types::ShareOffer {
        types::ShareOffer {
            share_id: self.share_id,
            title: self.title.clone(),
            package_name: self.package_name.clone(),
            app_label: self.app_label.clone(),
            items: self.items.iter().map(DragItem::to_wire).collect(),
        }
    }

    /// The share from the local API, checked again.
    pub fn from_host_api(offer: types::ShareOffer) -> Result<ShareRequest, String> {
        if offer.items.len() > MAX_SHARE_ITEMS {
            return Err("a share of too many items".into());
        }
        let items = offer
            .items
            .into_iter()
            .enumerate()
            .map(|(index, item)| DragItem::from_wire(item, index))
            .collect();
        ShareRequest::checked(
            offer.share_id,
            &offer.title,
            &offer.package_name,
            &offer.app_label,
            items,
        )
    }

    /// The name to show for the share: the title the app gave, else the app's, else a plain
    /// one.
    pub fn display_title(&self) -> String {
        [&self.title, &self.app_label, &self.package_name]
            .into_iter()
            .find(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| "Shared from Android".into())
    }

    /// The files among the items, with their indices in the share.
    pub fn files(&self) -> impl Iterator<Item = (u32, &DragItem)> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_file())
            .map(|(index, item)| (index as u32, item))
    }

    /// What the share UI is given, with the files copied to `files`.
    pub fn content(&self, files: Vec<PathBuf>) -> ShareContent {
        let mut texts = Vec::new();
        let mut links = Vec::new();
        for text in self.items.iter().filter_map(|item| item.text.as_deref()) {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if is_link(trimmed) {
                links.push(trimmed.to_string());
            } else {
                texts.push(trimmed.to_string());
            }
        }
        ShareContent {
            title: self.display_title(),
            text: (!texts.is_empty()).then(|| texts.join("\n")),
            links,
            files,
        }
    }
}

/// Whether `text` is one web link and nothing else.
pub fn is_link(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    match rest {
        Some(rest) => {
            !rest.is_empty()
                && !rest.starts_with('/')
                && !text.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    }
}

impl ShareOutcome {
    pub fn to_control(&self, share_id: u32) -> control::ShareResult {
        let (outcome, message) = match self {
            ShareOutcome::Shared => (control::ShareOutcome::Shared, String::new()),
            ShareOutcome::Canceled => (control::ShareOutcome::Canceled, String::new()),
            ShareOutcome::Failed(why) => (control::ShareOutcome::Failed, clean(why, 512)),
        };
        control::ShareResult {
            share_id,
            outcome: outcome as i32,
            message,
        }
    }

    pub fn to_host_api(&self, share_id: u32) -> types::ReportShare {
        let (outcome, message) = match self {
            ShareOutcome::Shared => (types::ShareOutcome::Shared, String::new()),
            ShareOutcome::Canceled => (types::ShareOutcome::Canceled, String::new()),
            ShareOutcome::Failed(why) => (types::ShareOutcome::Failed, clean(why, 512)),
        };
        types::ReportShare {
            share_id,
            outcome: outcome as i32,
            message,
        }
    }

    pub fn from_host_api(report: &types::ReportShare) -> Result<ShareOutcome, String> {
        match types::ShareOutcome::try_from(report.outcome) {
            Ok(types::ShareOutcome::Shared) => Ok(ShareOutcome::Shared),
            Ok(types::ShareOutcome::Canceled) => Ok(ShareOutcome::Canceled),
            Ok(types::ShareOutcome::Failed) => Ok(ShareOutcome::Failed(report.message.clone())),
            _ => Err("a share is shared, canceled or failed".into()),
        }
    }
}

/// The host's share UI, as the platform shows it.
pub trait ShareUi {
    /// Shows the share UI with `content` and waits until the user has picked where it goes or
    /// closed it.
    fn share(&mut self, content: &ShareContent) -> ShareOutcome;
}

/// Copies the files of `request` into a new folder under `spool`, reading each with `read`
/// in order, and returns their paths in the share's order. A file that cannot be read, or
/// files larger together than [`MAX_SHARE_BYTES`], fail the copy and leave nothing behind.
pub fn copy_files(
    request: &ShareRequest,
    spool: &Path,
    read: &mut dyn FnMut(DragRead) -> Result<DragChunk, String>,
) -> Result<Vec<PathBuf>, String> {
    if request.files().next().is_none() {
        return Ok(Vec::new());
    }
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let folder = spool.join(format!("share-{stamp}-{}", request.share_id));
    fs::create_dir_all(&folder).map_err(|e| format!("making {}: {e}", folder.display()))?;
    let copied = copy_into(request, &folder, read);
    if copied.is_err() {
        let _ = fs::remove_dir_all(&folder);
    }
    copied
}

fn copy_into(
    request: &ShareRequest,
    folder: &Path,
    read: &mut dyn FnMut(DragRead) -> Result<DragChunk, String>,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let mut total = 0u64;
    for (index, item) in request.files() {
        let path = unique_path(folder, &item.name);
        let mut file =
            fs::File::create(&path).map_err(|e| format!("making {}: {e}", path.display()))?;
        let mut offset = 0u64;
        loop {
            let chunk = read(DragRead {
                drag_id: request.share_id,
                item: index,
                offset,
                length: drag::READ_BYTES,
            })
            .map_err(|why| format!("{} could not be read from Android: {why}", item.name))?;
            if chunk.data.len() > drag::READ_BYTES as usize {
                return Err(format!("Android sent more of {} than was asked", item.name));
            }
            total += chunk.data.len() as u64;
            if total > MAX_SHARE_BYTES {
                return Err("the shared files are larger than the host takes".into());
            }
            file.write_all(&chunk.data)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
            offset += chunk.data.len() as u64;
            if chunk.end || item.size.is_some_and(|size| offset >= size) {
                break;
            }
            if chunk.data.is_empty() {
                return Err(format!("Android stopped sending {}", item.name));
            }
        }
        paths.push(path);
    }
    Ok(paths)
}

/// `name` in `folder`, with a number before its extension when a file of that name is
/// already there (two shared files may have the same name).
fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Removes the folders of shares older than `keep` from `spool`.
pub fn clean_spool(spool: &Path, keep: Duration) {
    let Ok(entries) = fs::read_dir(spool) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > keep);
        let ours = entry.file_name().to_string_lossy().starts_with("share-");
        if old && ours {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Copies the files of `request` with `read`, ends the share in Android with `end`, and shows
/// `ui`. Returns what became of it.
pub fn share_one(
    request: &ShareRequest,
    spool: &Path,
    ui: &mut dyn ShareUi,
    read: &mut dyn FnMut(DragRead) -> Result<DragChunk, String>,
    end: &mut dyn FnMut(u32, bool),
) -> ShareOutcome {
    let copied = copy_files(request, spool, read);
    // Android gives back its access to the files now: they are on the host, or cannot be.
    end(request.share_id, copied.is_ok());
    let files = match copied {
        Ok(files) => files,
        Err(why) => return ShareOutcome::Failed(why),
    };
    let content = request.content(files);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ui.share(&content)))
        .unwrap_or_else(|_| ShareOutcome::Failed("the share window failed".into()))
}

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);
/// How long the watch waits for a share before it looks at `stop` again.
const POLL: Duration = Duration::from_millis(200);
/// How long the watcher waits before it connects again.
const RETRY: Duration = Duration::from_secs(2);

/// Shows what the caller's Android shares in `ui` until `stop` is set, one share at a time,
/// with the files copied under `spool`. A lost connection is made again.
pub fn serve(
    connector: &mut dyn Connector,
    build: &str,
    ui: &mut dyn ShareUi,
    spool: &Path,
    stop: &AtomicBool,
    log: &dyn Fn(&str),
) {
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(connector, build, ANSWER)
            .and_then(|client| client.watch_shares());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(why) => {
                if !warned {
                    log(&format!("share: cannot take Android's shares yet: {why}"));
                    warned = true;
                }
                wait(stop, RETRY);
                continue;
            }
        };
        warned = false;
        while !stop.load(Ordering::Relaxed) {
            let request = match stream.next(POLL) {
                Ok(None) => continue,
                Ok(Some(request)) => request,
                Err(why) => {
                    log(&format!("share: the service connection ended: {why}"));
                    break;
                }
            };
            clean_spool(spool, KEEP_FILES);
            let files = request.files().count();
            log(&format!(
                "share: {} shares {} item(s), {files} of them files",
                request.display_title(),
                request.items.len()
            ));
            let mut client = match HostApiClient::connect(connector, build, ANSWER) {
                Ok(client) => client,
                Err(why) => {
                    log(&format!("share: cannot read the shared files: {why}"));
                    continue;
                }
            };
            let outcome = {
                let client = std::cell::RefCell::new(&mut client);
                let mut read = |read: DragRead| {
                    client
                        .borrow_mut()
                        .read_drag(read)
                        .map_err(|e| e.to_string())
                };
                let mut end = |share_id: u32, dropped: bool| {
                    if let Err(why) = client.borrow_mut().end_drag(share_id, dropped) {
                        log(&format!("share: could not end the share in Android: {why}"));
                    }
                };
                share_one(&request, spool, ui, &mut read, &mut end)
            };
            match &outcome {
                ShareOutcome::Shared => log("share: shared"),
                ShareOutcome::Canceled => log("share: the user closed the share window"),
                ShareOutcome::Failed(why) => log(&format!("share: failed: {why}")),
            }
            let reported = HostApiClient::connect(connector, build, ANSWER)
                .and_then(|mut client| client.report_share(request.share_id, &outcome));
            if let Err(why) = reported {
                log(&format!("share: the outcome did not reach Android: {why}"));
            }
        }
        wait(stop, RETRY);
    }
}

/// Sleeps `time`, or less once `stop` is set.
fn wait(stop: &AtomicBool, time: Duration) {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < time && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        waited += step;
    }
}

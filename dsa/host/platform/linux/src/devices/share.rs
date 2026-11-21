//! "Share on PC": what apps in the user's Android share, handed to the desktop.
//!
//! The follower watches the shares over the local API ([`dsa_host_core::share::serve`]),
//! copies the shared files into the user's cache folder, and hands them to the desktop through
//! the desktop portal's OpenURI interface (`org.freedesktop.portal.OpenURI`), which every
//! desktop that runs Flatpak apps has. The desktop has no share window of its own, so the
//! portal's app chooser stands in for one: a link opens in the app the user picks, one file or
//! the shared text (written to a file) opens in the app the user picks, and several files open
//! in the file manager, where the desktop's own send and share actions are. Without the portal
//! the same goes to `xdg-open`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use dsa_host_core::share::{self, ShareContent, ShareOutcome, ShareUi};
use dsa_transport::Connector;
use zbus::blocking::Connection;
use zbus::zvariant::{Fd, Value};

use super::dbus::portal_request;
use super::Log;

/// The build the follower names itself by.
const BUILD: &str = "dsa-devices share";

const OPEN_URI: &str = "org.freedesktop.portal.OpenURI";

/// The folder shared files are copied to: `dsa/shares` in the user's cache folder, which the
/// apps the user shares them with can read.
pub fn default_spool_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(cache.join("dsa").join("shares"))
}

/// Shows what the user's Android shares until `stop`.
pub fn serve(mut connector: Box<dyn Connector>, stop: Arc<AtomicBool>, log: Log) {
    let Some(spool) = default_spool_dir() else {
        log("share: no cache folder is known, so nothing can be shared with the PC");
        return;
    };
    let mut ui = DesktopShare {
        spool: spool.clone(),
        log: Arc::clone(&log),
    };
    let line = move |line: &str| log(line);
    share::serve(&mut *connector, BUILD, &mut ui, &spool, &stop, &line);
}

/// What the desktop is handed for one share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// A web link, opened in the app the user picks.
    Link(String),
    /// One file, opened in the app the user picks.
    File(PathBuf),
    /// A folder of several files, opened in the file manager.
    Folder(PathBuf),
}

/// What to hand the desktop for `content`, with the text written to a file in `spool` when
/// there is text to share. A share of one link and nothing else stays a link.
pub fn handoff(content: &ShareContent, spool: &Path) -> Result<Handoff, String> {
    let mut files = content.files.clone();
    if content.files.is_empty() && content.text.is_none() && content.links.len() == 1 {
        return Ok(Handoff::Link(content.links[0].clone()));
    }
    let mut text = content.text.clone().unwrap_or_default();
    for link in &content.links {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(link);
    }
    if !text.is_empty() {
        let folder = match files.first().and_then(|f| f.parent()) {
            Some(folder) => folder.to_path_buf(),
            None => {
                static NEXT: AtomicU32 = AtomicU32::new(1);
                let folder = spool.join(format!(
                    "share-text-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir_all(&folder)
                    .map_err(|e| format!("making {}: {e}", folder.display()))?;
                folder
            }
        };
        let name =
            dsa_host_core::drag::safe_name(&format!("{}.txt", content.title), "text/plain", 0);
        let path = folder.join(name);
        fs::write(&path, text.as_bytes())
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
        files.push(path);
    }
    match files.len() {
        0 => Err("the share carries nothing".into()),
        1 => Ok(Handoff::File(files.remove(0))),
        _ => {
            let folder = files[0]
                .parent()
                .map(Path::to_path_buf)
                .ok_or("the shared files are in no folder")?;
            Ok(Handoff::Folder(folder))
        }
    }
}

/// The desktop's chooser through the portal, else `xdg-open`.
pub struct DesktopShare {
    pub spool: PathBuf,
    pub log: Log,
}

impl ShareUi for DesktopShare {
    fn share(&mut self, content: &ShareContent) -> ShareOutcome {
        let handoff = match handoff(content, &self.spool) {
            Ok(handoff) => handoff,
            Err(why) => return ShareOutcome::Failed(why),
        };
        match portal_open(&handoff) {
            Ok(outcome) => outcome,
            Err(why) => {
                (self.log)(&format!(
                    "share: the desktop portal did not answer ({why}); using xdg-open"
                ));
                xdg_open(&handoff)
            }
        }
    }
}

static TOKENS: AtomicU32 = AtomicU32::new(1);

/// Hands `handoff` to the OpenURI portal, asking the user which app takes it.
fn portal_open(handoff: &Handoff) -> Result<ShareOutcome, String> {
    let connection = Connection::session().map_err(|e| e.to_string())?;
    let token = format!("dsa_share_{}", TOKENS.fetch_add(1, Ordering::Relaxed));
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    let (code, _) = match handoff {
        Handoff::Link(url) => {
            options.insert("ask", Value::from(true));
            portal_request(&connection, OPEN_URI, "OpenURI", &token, |portal| {
                portal.call("OpenURI", &("", url.as_str(), options))
            })?
        }
        Handoff::File(path) => {
            let file = File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
            options.insert("ask", Value::from(true));
            portal_request(&connection, OPEN_URI, "OpenFile", &token, |portal| {
                portal.call("OpenFile", &("", Fd::from(file.as_fd()), options))
            })?
        }
        Handoff::Folder(path) => {
            let folder =
                File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
            portal_request(&connection, OPEN_URI, "OpenDirectory", &token, |portal| {
                portal.call("OpenDirectory", &("", Fd::from(folder.as_fd()), options))
            })?
        }
    };
    Ok(match code {
        0 => ShareOutcome::Shared,
        1 => ShareOutcome::Canceled,
        _ => ShareOutcome::Failed("the desktop could not open it".into()),
    })
}

/// Opens `handoff` with `xdg-open`, in the program the desktop names for it.
fn xdg_open(handoff: &Handoff) -> ShareOutcome {
    let target = match handoff {
        Handoff::Link(url) => match dsa_host_core::links::web_url(url) {
            Ok(url) => url.to_string(),
            Err(why) => return ShareOutcome::Failed(why),
        },
        Handoff::File(path) | Handoff::Folder(path) => path.display().to_string(),
    };
    match Command::new("xdg-open")
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => ShareOutcome::Shared,
        Ok(status) => ShareOutcome::Failed(format!("xdg-open could not open it ({status})")),
        Err(e) => ShareOutcome::Failed(format!("xdg-open cannot run: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsa-linux-share-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fr36_a_link_alone_is_handed_on_as_a_link() {
        let content = ShareContent {
            title: "Trip".into(),
            text: None,
            links: vec!["https://example.com".into()],
            files: Vec::new(),
        };
        assert_eq!(
            handoff(&content, Path::new("/nowhere")),
            Ok(Handoff::Link("https://example.com".into()))
        );
    }

    #[test]
    fn fr36_shared_text_becomes_a_file_beside_the_shared_files() {
        let spool = scratch("text");
        let photo = spool.join("photo.jpg");
        fs::write(&photo, b"jpeg").unwrap();
        let content = ShareContent {
            title: "Trip".into(),
            text: Some("Look at this".into()),
            links: vec!["https://example.com".into()],
            files: vec![photo],
        };
        assert_eq!(
            handoff(&content, &spool),
            Ok(Handoff::Folder(spool.clone()))
        );
        assert_eq!(
            fs::read_to_string(spool.join("Trip.txt")).unwrap(),
            "Look at this\nhttps://example.com"
        );
        let _ = fs::remove_dir_all(&spool);
    }
}

//! Opening or sharing a file from Explorer in an app in the user's Android.
//!
//! The installer adds two entries to the context menu of every file. Each runs the installed
//! window host with [`OPEN_FILE_ARGUMENT`] or [`SHARE_FILE_ARGUMENT`] and the file's path. The
//! window host then reads the file, makes sure the window manager and Android run (as when an
//! app is opened from the Start menu), and sends the file to Android, which asks the user which
//! app opens or takes it. That app then shows in a window of its own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dsa_host_core::files::{FileAction, FileTransfer};
use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;

use super::argument;

/// The argument that names a file to open in an app in Android.
pub const OPEN_FILE_ARGUMENT: &str = "--open-file";

/// The argument that names a file to share with an app in Android.
pub const SHARE_FILE_ARGUMENT: &str = "--share-file";

/// The build the window host names itself by when it sends a file.
const BUILD: &str = "dsa-app-window file";

/// The file `args` asks to open or share, and which, if they ask for one.
pub fn file_request(args: &[String]) -> Option<(PathBuf, FileAction)> {
    argument(args, OPEN_FILE_ARGUMENT)
        .map(|path| (PathBuf::from(path), FileAction::View))
        .or_else(|| {
            argument(args, SHARE_FILE_ARGUMENT).map(|path| (PathBuf::from(path), FileAction::Send))
        })
}

/// Reads the file at `path`, refusing it before Android is started when it cannot be sent.
pub fn read(path: &Path, action: FileAction) -> Result<FileTransfer, String> {
    FileTransfer::read(path, action)
}

/// Sends `file` to the caller's Android through the service at `api`. Returns once Android
/// has asked the user which app takes it.
pub fn send(api: &mut dyn Connector, file: &FileTransfer) -> Result<(), String> {
    HostApiClient::connect(api, BUILD, Duration::from_secs(5))
        .map_err(|e| format!("the DSA service cannot be reached: {e}"))?
        .open_file(file)
        .map_err(|e| e.to_string())
}

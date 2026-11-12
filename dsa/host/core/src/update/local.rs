//! Installing an Android image from a folder on this computer, as a person testing a build
//! does with `dsa android install-image <folder>`.
//!
//! The folder holds what the service would otherwise download: a signed release list
//! (`images.json` and `images.json.sig`) and the files of the image it names, under the names
//! the list gives them. The image goes through exactly the service's own update path: the
//! list's signature is checked against the keys the service trusts, the rules choose the image
//! (a supported version, never a lower major, never one that failed here), every file is
//! checked against the size and SHA-256 the signed list gives, and the image becomes current
//! on trial, with the one before it kept until it has booted. Only where the bytes come from
//! differs: the folder instead of the list's URLs. A folder whose list is not signed by a
//! trusted key installs nothing, so this is no way around the signature.
//!
//! [`check_setup_in_folder`] does the same for a setup file of DSA itself: the signed program
//! list beside it, and the file's size and SHA-256, as the service checks an update it
//! downloaded before it runs it.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

use super::manifest::{self, is_plain_name};
use super::policy::{check_for, InstalledImage};
use super::program::{self, ProgramError, ProgramManifest};
use super::service::{check_reporting, Download};
use super::store::{Fetch, FileFetch, ImageStore};
use super::{to_hex, UpdateError};

/// The release list's name in the folder; its signature is beside it with `.sig` added.
pub const LIST: &str = "images.json";

/// Reads a release list and the files it names from one folder: each URL stands for the
/// file of its last path component there. A name that is not a plain file name is refused,
/// so a list cannot reach outside the folder.
pub struct FolderFetch {
    dir: PathBuf,
}

impl FolderFetch {
    pub fn new(dir: impl Into<PathBuf>) -> FolderFetch {
        FolderFetch { dir: dir.into() }
    }

    fn path_of(&self, url: &str) -> Result<PathBuf, String> {
        let name = url.rsplit(['/', '\\']).next().unwrap_or(url);
        if !is_plain_name(name) {
            return Err(format!("{url:?} does not end in a plain file name"));
        }
        Ok(self.dir.join(name))
    }
}

impl Fetch for FolderFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let path = self.path_of(url)?;
        FileFetch.fetch(&path.to_string_lossy(), out)
    }
}

/// Installs the image the signed list in `dir` offers into `store`, trusting `keys` (the
/// service's own). Returns the image that is now current on trial; it boots at the next
/// start of Android, or at once where the service restarts a running Android into a new image.
/// `copying` hears how far the files have got. The error says why nothing was installed: a
/// bad signature, a file that does not match the list, or the rule that refused the image.
pub fn install_from_folder(
    store: &ImageStore,
    dir: &Path,
    keys: &[VerifyingKey],
    copying: &mut dyn FnMut(&Download),
) -> Result<InstalledImage, UpdateError> {
    let before = store.state()?;
    if before.on_trial {
        return Err(UpdateError::Refused(format!(
            "{} has not finished its first boot yet; start Android once so it is kept or \
             undone, then install again",
            before.current.version
        )));
    }
    let fetch = FolderFetch::new(dir);
    let list_url = dir.join(LIST).to_string_lossy().into_owned();
    check_reporting(store, &fetch, &list_url, keys, copying)?;
    let after = store.state()?;
    if after.on_trial && after.current != before.current {
        return Ok(after.current);
    }
    // Nothing was staged: say which rule kept each image out.
    let list = fs::read(dir.join(LIST))
        .map_err(|e| UpdateError::Download(format!("reading {}: {e}", dir.display())))?;
    let signature = fs::read_to_string(dir.join(format!("{LIST}.sig")))
        .map_err(|e| UpdateError::BadSignature(format!("reading the signature: {e}")))?;
    let manifest = manifest::verify(&list, &signature, keys)?;
    let reasons: Vec<String> = manifest
        .images
        .iter()
        .map(|image| {
            check_for(
                &after.current,
                image,
                &manifest,
                &after.failed,
                after.older_allowed,
            )
            .err()
            .unwrap_or_else(|| {
                format!(
                    "{} is not of the Android version this computer follows (dsa android use \
                     chooses it)",
                    image.version
                )
            })
        })
        .collect();
    Err(UpdateError::Refused(if reasons.is_empty() {
        format!("the list in {} names no image", dir.display())
    } else {
        reasons.join("; ")
    }))
}

/// The program list's name beside a setup file in a folder; its signature is beside it with
/// `.sig` added.
pub const PROGRAM_LIST: &str = "program.json";

/// Checks the setup file in `dir` as the service checks a downloaded update of DSA before it
/// runs it: the program list beside it (`program.json` and `program.json.sig`) is signed by
/// one of `keys`, and the file its URL names (by its last path component, in `dir`) has the
/// size and SHA-256 the list gives. Returns the list and the setup file's path. Whether the
/// version is newer is not checked: a person installing a build by hand may go back.
pub fn check_setup_in_folder(
    dir: &Path,
    keys: &[VerifyingKey],
) -> Result<(ProgramManifest, PathBuf), ProgramError> {
    let list_path = dir.join(PROGRAM_LIST);
    let bytes = fs::read(&list_path)
        .map_err(|e| ProgramError::BadList(format!("reading {}: {e}", list_path.display())))?;
    let signature = fs::read_to_string(dir.join(format!("{PROGRAM_LIST}.sig")))
        .map_err(|e| ProgramError::BadSignature(format!("reading the signature: {e}")))?;
    let list = program::verify(&bytes, &signature, keys)?;
    let name = list
        .setup
        .url
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    if !is_plain_name(name) {
        return Err(ProgramError::BadList(format!(
            "the setup file's address {:?} does not end in a plain file name",
            list.setup.url
        )));
    }
    let path = dir.join(name);
    let mut file = fs::File::open(&path)
        .map_err(|e| ProgramError::Download(format!("opening {}: {e}", path.display())))?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| ProgramError::Download(format!("reading {}: {e}", path.display())))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        size += read as u64;
    }
    if size != list.setup.size {
        return Err(ProgramError::Download(format!(
            "{name} is {size} bytes, and the program list says {}",
            list.setup.size
        )));
    }
    let actual = to_hex(&hash.finalize());
    if !actual.eq_ignore_ascii_case(list.setup.sha256.trim()) {
        return Err(ProgramError::Download(format!(
            "the SHA-256 of {name} is {actual}, and the program list says {}",
            list.setup.sha256
        )));
    }
    Ok((list, path))
}

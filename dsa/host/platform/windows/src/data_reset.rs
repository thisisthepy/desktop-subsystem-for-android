//! Resetting a user's Android data on Windows: their data disk is replaced by a fresh one.
//!
//! The disk is where the service keeps it for that user (their profile's `DSA` folder, or the
//! configuration's `data_dir`), as [`HostConfig::user_vm`] finds it through the paths module.
//! The fresh disk is made the way a new user's is: a copy of the current Android image's disk
//! template, with the image version recorded beside it, or an empty disk for an installation
//! that boots a fixed system image. The image store and the settings folder are not touched.

use std::path::Path;
use std::sync::Arc;

use dsa_host_core::data_reset::{TemplateDataDisk, UserDataReset};
use dsa_host_core::update::store::ImageStore;

use crate::host_config::HostConfig;
use crate::user_disk::{bring_disk_to, prepare_user_data_disk};

/// The extension of the file beside a user's disk that records the image version it holds.
const IMAGE_VERSION: &str = "image-version";

/// The data reset of the service: `images` is the update store the next start boots from, if
/// the service uses one. A disk in the user's own folder is reset with that folder held and
/// each file checked first (see `crate::safe_fs`).
pub fn windows_data_reset(config: HostConfig, images: Option<Arc<ImageStore>>) -> GuardedReset {
    GuardedReset {
        config: config.clone(),
        reset: template_reset(config, images),
    }
}

/// [`TemplateDataDisk`], run while the folders of a disk in the user's own folder are held.
pub struct GuardedReset {
    config: HostConfig,
    reset: TemplateDataDisk,
}

impl UserDataReset for GuardedReset {
    fn replace_disk(&self, user: &str) -> Result<(), String> {
        let vm = self.config.user_vm(user).map_err(|e| e.to_string())?;
        let disk = vm.config.data_disk;
        let _guard = match disk.parent().filter(|_| vm.disk_in_profile) {
            Some(root) => {
                let aside = |path: &Path| {
                    let mut name = path.as_os_str().to_os_string();
                    name.push(".before-reset");
                    std::path::PathBuf::from(name)
                };
                let version = disk.with_extension(IMAGE_VERSION);
                let mut files = vec![disk.clone(), version.clone(), aside(&disk), aside(&version)];
                files.push(disk.with_extension("vhdx.new"));
                let files: Vec<&Path> = files.iter().map(std::path::PathBuf::as_path).collect();
                crate::safe_fs::guard_files(root, &files)?
            }
            None => Vec::new(),
        };
        self.reset.replace_disk(user)
    }
}

fn template_reset(config: HostConfig, images: Option<Arc<ImageStore>>) -> TemplateDataDisk {
    let finder = config.clone();
    TemplateDataDisk::new(
        Box::new(move |sid: &str| {
            finder
                .user_vm(sid)
                .map(|user| user.config.data_disk)
                .map_err(|e| e.to_string())
        }),
        Box::new(move |disk: &Path| make_fresh_disk(&config, images.as_deref(), disk)),
    )
    .with_beside(IMAGE_VERSION)
}

fn make_fresh_disk(
    config: &HostConfig,
    images: Option<&ImageStore>,
    disk: &Path,
) -> Result<(), String> {
    let Some(store) = images else {
        return prepare_user_data_disk(disk, config.data_disk_size_gb);
    };
    let current = store.state().map_err(|e| e.to_string())?.current;
    let files = config.image_files(Some(&store.image_dir(&current.version)));
    match &files.disk_template {
        Some(template) => bring_disk_to(disk, template, &current.version),
        None => prepare_user_data_disk(disk, config.data_disk_size_gb),
    }
}

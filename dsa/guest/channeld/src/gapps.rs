//! The guest's half of the Google apps a user installs: their system image on the user's disk.
//!
//! The host downloads the build of Google's apps the user asked for, makes a system image of
//! it, and sends the image (`GappsInstall` and its `GappsChunk`s). The daemon keeps it as
//! [`ImageStore`] keeps an image, so a transfer that breaks off leaves the guest as it was.
//! `GappsRemove` deletes it, and `GappsQuery` asks how things stand. Every one of them is
//! answered with `GappsState`, which also says which Android the guest is (its API level and
//! ABIs), so the host fetches the build for this Android and no other.
//!
//! The image takes effect at the next boot: init lays it over `/product` and `/system_ext`
//! then (`dsa-gapps.rc`), when the host says one is installed. Whether the running boot has it
//! comes from the mount table. The platform supplies those facts, so this runs and is tested
//! off Android.

use std::path::PathBuf;

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::GappsState;

pub use crate::image_store::IMAGE;
use crate::image_store::{ImageStore, Progress};

/// Where the image goes on the user's disk.
pub const DIR: &str = "/data/vendor/dsa/gapps";
/// What the image is called in what the host shows the user.
const IMAGE_NAME: &str = "the Google apps image";
/// The largest image taken: LiteGapps' Core builds take a few hundred MiB.
pub const MAX_IMAGE_BYTES: u64 = 2 << 30;
/// Where init mounts the image, as the mount table shows it once it is laid over the system.
pub const MOUNT_POINT: &str = "/mnt/dsa-gapps";

/// What the running boot is, which the platform reads from the system.
pub struct GuestFacts {
    /// Whether this boot has the image laid over the system.
    pub active: Box<dyn Fn() -> bool + Send + Sync>,
    /// `ro.build.version.sdk`.
    pub sdk: Box<dyn Fn() -> u32 + Send + Sync>,
    /// `ro.product.cpu.abi`.
    pub abi: Box<dyn Fn() -> String + Send + Sync>,
    /// `ro.product.cpu.abilist`.
    pub abilist: Box<dyn Fn() -> String + Send + Sync>,
}

/// Whether the mount table `mounts` (as `/proc/self/mounts` reads) has something mounted at
/// [`MOUNT_POINT`].
pub fn mounted(mounts: &str) -> bool {
    mounts
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(MOUNT_POINT))
}

impl GuestFacts {
    /// The facts of the running system: the mount table, and the properties `property` reads.
    pub fn of_system(property: impl Fn(&str) -> String + Send + Sync + 'static) -> GuestFacts {
        let property = std::sync::Arc::new(property);
        let (sdk, abi, abilist) = (property.clone(), property.clone(), property);
        GuestFacts {
            active: Box::new(|| {
                std::fs::read_to_string("/proc/self/mounts")
                    .map(|mounts| mounted(&mounts))
                    .unwrap_or(false)
            }),
            sdk: Box::new(move || (*sdk)("ro.build.version.sdk").trim().parse().unwrap_or(0)),
            abi: Box::new(move || (*abi)("ro.product.cpu.abi")),
            abilist: Box::new(move || (*abilist)("ro.product.cpu.abilist")),
        }
    }
}

/// The Google apps' image on the user's disk and the transfer under way.
pub struct Gapps {
    store: ImageStore,
    facts: GuestFacts,
}

impl Gapps {
    /// Keeps the image in `dir`, which init makes at boot.
    pub fn new(dir: impl Into<PathBuf>, facts: GuestFacts) -> Gapps {
        Gapps {
            store: ImageStore::new(dir, IMAGE_NAME, MAX_IMAGE_BYTES),
            facts,
        }
    }

    /// Handles a message from the host meant for the Google apps, and returns the answer for
    /// the host. Any other message is not this module's: `None`.
    pub fn handle(&self, body: &Body) -> Option<GappsState> {
        let progress = match body {
            Body::GappsInstall(install) => {
                self.store
                    .begin(install.transfer_id, install.size, &install.sha256)
            }
            Body::GappsChunk(chunk) => {
                self.store
                    .write(chunk.transfer_id, chunk.offset, &chunk.data)
            }
            Body::GappsRemove(_) => self.store.remove(),
            Body::GappsQuery(_) => Progress::default(),
            _ => return None,
        };
        Some(self.state(progress))
    }

    fn state(&self, progress: Progress) -> GappsState {
        GappsState {
            transfer_id: progress.transfer_id,
            received: progress.received,
            done: progress.done,
            error: progress.error,
            installed: self.store.installed(),
            active: (self.facts.active)(),
            sdk: (self.facts.sdk)(),
            abi: (self.facts.abi)(),
            abilist: (self.facts.abilist)(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr32_the_image_is_active_once_it_is_mounted() {
        let mounts = "/dev/block/dm-0 / erofs ro 0 0\n\
                      /dev/block/loop7 /mnt/dsa-gapps erofs ro,nodev,nosuid 0 0\n\
                      overlay /product overlay ro,lowerdir=/mnt/dsa-gapps/product:/product 0 0\n";
        assert!(mounted(mounts));
        assert!(!mounted("/dev/block/dm-0 / erofs ro 0 0\n"));
        assert!(!mounted("/dev/loop1 /mnt/dsa-gapps-other erofs ro 0 0\n"));
    }
}

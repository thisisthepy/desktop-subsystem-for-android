//! The guest's half of ARM app translation: the translation package's system image on the
//! user's disk.
//!
//! The host sends the image (`ArmTranslationInstall` and its `ArmTranslationChunk`s). The
//! daemon keeps it as [`ImageStore`] keeps an image: written beside the one already there, and
//! put in that one's place only once all of it has arrived and its length and SHA-256 match
//! what the host announced, so a transfer that breaks off leaves the guest as it was.
//! `ArmTranslationRemove` deletes it, and `ArmTranslationQuery` asks how things stand. Every
//! one of them is answered with `ArmTranslationState`.
//!
//! The image takes effect at the next boot: init mounts it over the system then
//! (`dsa-arm-translation.rc`), when the host says one is installed. What the running boot has
//! comes from the system: whether the bridge library is in place and which ABIs the package
//! manager installs apps for. The platform supplies those, so this runs and is tested off
//! Android.

use std::path::{Path, PathBuf};

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::ArmTranslationState;

pub use crate::image_store::IMAGE;
use crate::image_store::{ImageStore, Progress};

/// Where the package goes on the user's disk.
pub const DIR: &str = "/data/vendor/dsa/arm-translation";
/// What the image is called in what the host shows the user.
const IMAGE_NAME: &str = "the ARM translation image";
/// The largest image taken: a translator, its ARM64 libraries and their proxies take a few
/// hundred MiB at most.
pub const MAX_IMAGE_BYTES: u64 = 2 << 30;

/// The native bridge library the image's properties name, once a package is laid over the
/// system.
pub const BRIDGE_LIBRARY: &str = "/system/lib64/libdsa_native_bridge.so";

/// What the running boot has, which the platform reads from the system.
pub struct BootFacts {
    /// Whether the native bridge library the image's properties name is in place.
    pub active: Box<dyn Fn() -> bool + Send + Sync>,
    /// `ro.product.cpu.abilist`.
    pub abilist: Box<dyn Fn() -> String + Send + Sync>,
}

impl BootFacts {
    /// The bridge is in place when [`BRIDGE_LIBRARY`] exists; `abilist` reads the property.
    pub fn of_system(abilist: impl Fn() -> String + Send + Sync + 'static) -> BootFacts {
        BootFacts {
            active: Box::new(|| Path::new(BRIDGE_LIBRARY).exists()),
            abilist: Box::new(abilist),
        }
    }
}

/// The package on the user's disk and the transfer under way.
pub struct ArmTranslation {
    store: ImageStore,
    facts: BootFacts,
}

impl ArmTranslation {
    /// Keeps the package in `dir`, which init makes at boot.
    pub fn new(dir: impl Into<PathBuf>, facts: BootFacts) -> ArmTranslation {
        ArmTranslation {
            store: ImageStore::new(dir, IMAGE_NAME, MAX_IMAGE_BYTES),
            facts,
        }
    }

    /// Handles a message from the host meant for ARM translation, and returns the answer for
    /// the host. Any other message is not this module's: `None`.
    pub fn handle(&self, body: &Body) -> Option<ArmTranslationState> {
        let progress = match body {
            Body::ArmTranslationInstall(install) => {
                self.store
                    .begin(install.transfer_id, install.size, &install.sha256)
            }
            Body::ArmTranslationChunk(chunk) => {
                self.store
                    .write(chunk.transfer_id, chunk.offset, &chunk.data)
            }
            Body::ArmTranslationRemove(_) => self.store.remove(),
            Body::ArmTranslationQuery(_) => Progress::default(),
            _ => return None,
        };
        Some(self.state(progress))
    }

    fn state(&self, progress: Progress) -> ArmTranslationState {
        ArmTranslationState {
            transfer_id: progress.transfer_id,
            received: progress.received,
            done: progress.done,
            error: progress.error,
            installed: self.store.installed(),
            active: (self.facts.active)(),
            abilist: (self.facts.abilist)(),
        }
    }
}

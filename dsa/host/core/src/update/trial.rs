//! The first boot of a new image, and going back when it fails.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::policy::InstalledImage;
use super::store::ImageStore;
use super::UpdateError;

/// Boots the guest from an image folder.
pub trait Boot {
    /// Starts the guest from `image_dir` and returns once it has finished booting, or an
    /// error with the cause when it has not within `timeout`.
    fn boot(
        &mut self,
        image: &InstalledImage,
        image_dir: &Path,
        timeout: Duration,
    ) -> Result<(), String>;
    /// Stops the guest, whatever state it is in.
    fn halt(&mut self);
    /// Whether the failure [`boot`](Self::boot) just reported says anything about the image.
    fn image_to_blame(&self) -> bool {
        true
    }
}

/// How a start ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The current image booted. `confirmed` is set when it was a new image on trial.
    Booted {
        image: InstalledImage,
        confirmed: bool,
    },
    /// The new image did not boot; the previous one did, and is current again.
    RolledBack {
        failed: InstalledImage,
        reason: String,
        image: InstalledImage,
    },
    /// The new image did not boot and there is no image to go back to: it was the first
    /// one since an installation without a bundled image.
    GaveUp {
        failed: InstalledImage,
        reason: String,
    },
    /// The image on trial did not get ready, for a cause that says nothing about it. The guest
    /// keeps running, and the image stays on trial: neither kept nor given up.
    Undecided {
        image: InstalledImage,
        reason: String,
    },
}

/// Starts the current image. A new image on trial that does not boot within `timeout` is
/// stopped and given up, and the previous image is started instead.
pub fn start(
    store: &ImageStore,
    guest: &mut dyn Boot,
    timeout: Duration,
) -> Result<Outcome, UpdateError> {
    let state = store.state()?;
    let dir: PathBuf = store.image_dir(&state.current.version);
    match guest.boot(&state.current, &dir, timeout) {
        Ok(()) => {
            store.confirm()?;
            Ok(Outcome::Booted {
                image: state.current,
                confirmed: state.on_trial,
            })
        }
        Err(reason) if state.on_trial && !guest.image_to_blame() => Ok(Outcome::Undecided {
            image: state.current,
            reason,
        }),
        Err(reason) if state.on_trial => {
            guest.halt();
            let image = store.roll_back()?;
            if !store.has_image(&image.version) {
                return Ok(Outcome::GaveUp {
                    failed: state.current,
                    reason,
                });
            }
            guest
                .boot(&image, &store.image_dir(&image.version), timeout)
                .map_err(|why| {
                    UpdateError::Store(format!(
                        "{} failed to boot ({reason}), and going back to {} failed too: {why}",
                        state.current.version, image.version
                    ))
                })?;
            Ok(Outcome::RolledBack {
                failed: state.current,
                reason,
                image,
            })
        }
        Err(reason) => Err(UpdateError::Store(format!(
            "{} did not boot: {reason}",
            state.current.version
        ))),
    }
}

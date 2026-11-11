//! The host's direct-mapped mode: the VM's read-only disks (the image's system disk and the
//! user's add-on disk) attached as persistent memory instead of block devices, so the guest
//! maps their files from the host's copy rather than keeping a page cache of its own.
//!
//! On a block device the guest reads each page of the read-only partitions into its own page
//! cache, on top of the host's file cache of the same image: the same bytes twice, and the
//! guest's copy is memory the VM holds. On persistent memory (Hyper-V's virtual persistent
//! memory, or virtio-pmem under KVM) the guest kernel maps the device into its physical address
//! space, and erofs mounted with `dax=always` reads and maps each file straight from there
//! (DAX): no guest page cache, and the pages are the host's file mapping of the image, which
//! the host can drop and read again like any clean file page.
//!
//! The mode is a host setting, off by default. A boot in it differs from a normal one in three
//! places, which this module gives every host:
//! - the image's mount table for the mode (`fstab.<suffix>`, which `image.json` names under
//!   `"direct_mapped"`): the logical partitions mount with `dax=always` and without dm-verity,
//!   which cannot pass DAX through. The image is checked on the host instead: the disk is the
//!   published, checksummed one, attached read only;
//! - the boot devices: Android's first stage init makes `/dev/block/by-name` links only for
//!   disks below a boot device, and the guest kernel puts each persistent memory bus below a
//!   platform device of its own, which the host adds to the list ([`kernel_cmdline`]);
//! - the add-on images, which the guest then mounts from their own partitions on the add-on
//!   disk ([`crate::addon_disk::direct_mapped_bootconfig`]).
//!
//! An image from before the mode has no such table, so the host boots it normally
//! ([`ImageSupport::from_image_dir`] finds none).

use std::path::Path;

use serde_json::Value;

/// The kernel command line's boot devices, which the host adds the persistent memory's
/// platform device to.
pub const BOOT_DEVICES_KEY: &str = "androidboot.boot_devices";
/// The kernel command line's mount table suffix (`fstab.<suffix>`).
pub const FSTAB_SUFFIX_KEY: &str = "androidboot.fstab_suffix";

/// What an image says about the direct-mapped mode in `image.json` (`"direct_mapped"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSupport {
    /// The mount table the mode boots with, `fstab.<fstab_suffix>`.
    pub fstab_suffix: String,
    /// Whether the read-only partitions are uncompressed, so that every file is mapped. A
    /// compressed file is read into the guest's cache as before.
    pub uncompressed_erofs: bool,
}

/// Whether `suffix` can name a mount table on the kernel command line.
fn is_plain_suffix(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl ImageSupport {
    /// The image's support for the mode, from its `image.json`, `manifest`; `None` for an image
    /// without it.
    pub fn from_manifest(manifest: &Value) -> Option<ImageSupport> {
        let entry = manifest.get("direct_mapped")?;
        let fstab_suffix = entry.get("fstab_suffix")?.as_str()?;
        if !is_plain_suffix(fstab_suffix) {
            return None;
        }
        Some(ImageSupport {
            fstab_suffix: fstab_suffix.to_string(),
            uncompressed_erofs: entry
                .get("uncompressed_erofs")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// The support of the image in the folder `dir`, from its `image.json`.
    pub fn from_image_dir(dir: &Path) -> Option<ImageSupport> {
        let text = std::fs::read_to_string(dir.join("image.json")).ok()?;
        let manifest: Value = serde_json::from_str(&text).ok()?;
        ImageSupport::from_manifest(&manifest)
    }
}

/// The kernel command line `cmdline` for a direct-mapped boot: `platform_device` added to its
/// boot devices, and the image's mount table for the mode named in place of its own. Android
/// keeps the first value the command line gives for each, as a read-only property, so each is
/// replaced where it is rather than given again.
pub fn kernel_cmdline(cmdline: &str, platform_device: &str, support: &ImageSupport) -> String {
    let devices_prefix = format!("{BOOT_DEVICES_KEY}=");
    let suffix_prefix = format!("{FSTAB_SUFFIX_KEY}=");
    let mut has_devices = false;
    let mut has_suffix = false;
    let mut words: Vec<String> = cmdline
        .split_whitespace()
        .map(|word| {
            if let Some(devices) = word.strip_prefix(&devices_prefix) {
                has_devices = true;
                let mut list: Vec<&str> = devices.split(',').filter(|d| !d.is_empty()).collect();
                if !list.contains(&platform_device) {
                    list.push(platform_device);
                }
                format!("{devices_prefix}{}", list.join(","))
            } else if word.starts_with(&suffix_prefix) {
                has_suffix = true;
                format!("{suffix_prefix}{}", support.fstab_suffix)
            } else {
                word.to_string()
            }
        })
        .collect();
    if !has_devices {
        words.push(format!("{devices_prefix}{platform_device}"));
    }
    if !has_suffix {
        words.push(format!("{suffix_prefix}{}", support.fstab_suffix));
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support() -> ImageSupport {
        ImageSupport {
            fstab_suffix: "dsa_dax".into(),
            uncompressed_erofs: true,
        }
    }

    #[test]
    fn nfr1_an_image_says_which_mount_table_its_direct_mapped_mode_boots_with() {
        let manifest = serde_json::json!({
            "version": "17.0.0-1-1",
            "direct_mapped": {"fstab_suffix": "dsa_dax", "uncompressed_erofs": true},
        });
        assert_eq!(ImageSupport::from_manifest(&manifest), Some(support()));
        let compressed = serde_json::json!({"direct_mapped": {"fstab_suffix": "dsa_dax"}});
        assert_eq!(
            ImageSupport::from_manifest(&compressed).map(|s| s.uncompressed_erofs),
            Some(false)
        );
        // An image from before the mode, or one whose table cannot be named, boots normally.
        assert_eq!(
            ImageSupport::from_manifest(&serde_json::json!({"version": "1"})),
            None
        );
        let odd = serde_json::json!({"direct_mapped": {"fstab_suffix": "dsa dax=1"}});
        assert_eq!(ImageSupport::from_manifest(&odd), None);
    }

    #[test]
    fn nfr1_a_direct_mapped_boot_names_its_table_and_the_persistent_memory_as_a_boot_device() {
        let image = "console=ttyS0 androidboot.boot_devices=vmbus androidboot.fstab_suffix=dsa \
                     androidboot.slot_suffix=_a";
        assert_eq!(
            kernel_cmdline(image, "nfit", &support()),
            "console=ttyS0 androidboot.boot_devices=vmbus,nfit androidboot.fstab_suffix=dsa_dax \
             androidboot.slot_suffix=_a"
        );
        // Given once only, whatever the line had.
        let twice = kernel_cmdline(
            &kernel_cmdline(image, "nfit", &support()),
            "nfit",
            &support(),
        );
        assert_eq!(twice.matches("nfit").count(), 1);
        assert_eq!(twice.matches("androidboot.fstab_suffix=").count(), 1);
        assert_eq!(
            kernel_cmdline("init=/init", "virtio-pmem", &support()),
            "init=/init androidboot.boot_devices=virtio-pmem androidboot.fstab_suffix=dsa_dax"
        );
    }
}

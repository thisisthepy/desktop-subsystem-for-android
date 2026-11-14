//! The host's direct-mapped mode: the VM's read-only disks (the system disk and the user's
//! add-on disk) attached as persistent memory instead of block devices, so the guest maps
//! their files from the host's copy rather than keeping a page cache of its own.
//!
//! On a block device the guest reads each page of the read-only partitions into its own page
//! cache, on top of the host's file cache of the same image: the same bytes twice, and the
//! guest's copy is memory the VM holds. On persistent memory (Hyper-V's virtual persistent
//! memory, or virtio-pmem under KVM) the guest kernel maps the device into its physical address
//! space, and erofs mounted with `dax=always` reads and maps each file straight from there
//! (DAX): no guest page cache, and the pages are the host's file mapping of the image, which
//! the host can drop and read again like any clean file page. The host drops them only when
//! short of memory, though, so each page the guest read once stays in the VM's working set
//! until the host takes it out itself ([`MappedPagesTrim`]).
//!
//! The mode is a host setting, off by default. A boot in it differs from a normal one in four
//! places, which this module gives every host:
//! - the system disk: not the image's own, whose read-only partitions are compressed and so
//!   cannot be mapped, but the uncompressed copy the host makes of it
//!   ([`crate::direct_mapped_disk`]) and checks against the hash `image.json` names. Its super
//!   partition is smaller than the image's, so the kernel command line's partition list gives
//!   the copy's size for it ([`kernel_cmdline`]);
//! - the image's mount table for the mode (`fstab.<suffix>`, which `image.json` names under
//!   `"direct_mapped"`): the logical partitions mount with `dax=always` and without dm-verity,
//!   which cannot pass DAX through, all but those `image.json` names as verified
//!   (`verified_partitions`). The copy keeps those as the image has them, and they keep
//!   dm-verity, so the guest's first stage still checks the verified boot data (vbmeta and the
//!   boot images) as a normal boot does. The mapped partitions are checked on the host instead:
//!   the copy matches the image's hash and is attached read only;
//! - the boot devices: Android's first stage init makes `/dev/block/by-name` links only for
//!   disks below a boot device, and the guest kernel puts each persistent memory bus below a
//!   platform device of its own, which the host adds to the list ([`kernel_cmdline`]);
//! - the add-on images, which the guest then mounts from their own partitions on the add-on
//!   disk ([`crate::addon_disk::direct_mapped_bootconfig`]).
//!
//! An image from before the mode, or one whose copy this host does not make, has no usable
//! description, so the host boots it normally ([`ImageSupport::from_image_dir`] finds none).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::direct_mapped_disk::SystemDisk;

/// The kernel command line's boot devices, which the host adds the persistent memory's
/// platform device to.
pub const BOOT_DEVICES_KEY: &str = "androidboot.boot_devices";
/// The kernel command line's mount table suffix (`fstab.<suffix>`).
pub const FSTAB_SUFFIX_KEY: &str = "androidboot.fstab_suffix";
/// The kernel command line's partition list (`blkdevparts=<disk>:<size>@<offset>(<name>),...`).
pub const PARTITIONS_KEY: &str = "blkdevparts";
/// The system disk's super partition, by the name the partition list gives it.
pub const SUPER_PARTITION: &str = "super";

/// What an image says about the direct-mapped mode in `image.json` (`"direct_mapped"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSupport {
    /// The mount table the mode boots with, `fstab.<fstab_suffix>`.
    pub fstab_suffix: String,
    /// The copy of the system disk the mode maps, as the image build made it.
    pub system_disk: SystemDisk,
    /// The logical partitions the copy keeps as they are, which the mode's mount table still
    /// verifies with dm-verity (names without their slot suffix).
    pub verified_partitions: Vec<String>,
}

/// Whether `suffix` can name a mount table on the kernel command line, or a partition.
fn is_plain_suffix(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl ImageSupport {
    /// The image's support for the mode, from its `image.json`, `manifest`; `None` for an image
    /// without it, or with a copy of its system disk this host does not make. The mode maps
    /// only that uncompressed copy: the image's own compressed partitions would give up
    /// dm-verity for files the guest caches anyway.
    pub fn from_manifest(manifest: &Value) -> Option<ImageSupport> {
        let entry = manifest.get("direct_mapped")?;
        let fstab_suffix = entry.get("fstab_suffix")?.as_str()?;
        if !is_plain_suffix(fstab_suffix) {
            return None;
        }
        let system_disk: SystemDisk =
            serde_json::from_value(entry.get("system_disk")?.clone()).ok()?;
        if !system_disk.is_usable() {
            return None;
        }
        let verified_partitions = match entry.get("verified_partitions") {
            None => Vec::new(),
            Some(list) => list
                .as_array()?
                .iter()
                .map(|name| {
                    name.as_str()
                        .filter(|n| is_plain_suffix(n))
                        .map(str::to_string)
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(ImageSupport {
            fstab_suffix: fstab_suffix.to_string(),
            system_disk,
            verified_partitions,
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
/// boot devices, the image's mount table for the mode named in place of its own, and the super
/// partition's size in the partition list made the copy's. Android keeps the first value the
/// command line gives for each, as a read-only property, so each is replaced where it is
/// rather than given again.
pub fn kernel_cmdline(cmdline: &str, platform_device: &str, support: &ImageSupport) -> String {
    let devices_prefix = format!("{BOOT_DEVICES_KEY}=");
    let suffix_prefix = format!("{FSTAB_SUFFIX_KEY}=");
    let partitions_prefix = format!("{PARTITIONS_KEY}=");
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
            } else if let Some(list) = word.strip_prefix(&partitions_prefix) {
                format!(
                    "{partitions_prefix}{}",
                    with_super_size(list, support.system_disk.super_bytes)
                )
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

/// A partition list (`<disk>:<size>@<offset>(<name>[:<uuid>]),...`, disks separated by `;`)
/// with `bytes` as the super partition's size.
fn with_super_size(list: &str, bytes: u64) -> String {
    list.split(';')
        .map(|disk| {
            let Some((name, parts)) = disk.split_once(':') else {
                return disk.to_string();
            };
            let parts: Vec<String> = parts
                .split(',')
                .map(|part| {
                    let label = part
                        .split_once('(')
                        .map(|(_, rest)| rest.trim_end_matches(')'))
                        .unwrap_or("");
                    let partition = label.split(':').next().unwrap_or("");
                    match part.split_once('@') {
                        Some((_, rest)) if partition == SUPER_PARTITION => {
                            format!("{bytes}@{rest}")
                        }
                        _ => part.to_string(),
                    }
                })
                .collect();
            format!("{name}:{}", parts.join(","))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// How often the host looks at how many pages of the mapped disks the VM's process holds.
/// The look is one process counter.
pub const MAPPED_PAGES_CHECK: Duration = Duration::from_secs(10);

/// The pages of the mapped disks the VM's process may hold before the host takes them out of
/// its working set, in KiB. What Android uses all the time (the code it runs while it dozes)
/// comes back at once and stays below this, so an idle Android is not given back to every
/// minute for nothing.
pub const MAPPED_PAGES_FLOOR_KB: u64 = 32 * 1024;

/// The least time between two give-backs of the same VM. After one, the pages Android is
/// using come back from the host's standby list as it touches them (a soft fault, no disk
/// read); a minute keeps that to once a minute for the pages an open app keeps using, a few
/// milliseconds of the guest's time, while the pages a boot or an app's start read once and
/// left go back within a minute.
pub const MAPPED_PAGES_GAP: Duration = Duration::from_secs(60);

/// How often the log says what the give-backs did, at most: as often as the memory log.
pub const MAPPED_PAGES_LOG_INTERVAL: Duration = crate::memory::LOG_INTERVAL;

/// Decides when the host takes the pages of the disks a VM maps directly out of the VM's
/// working set, and what the log says of it.
///
/// A page of a mapped disk the guest reads joins the working set of the VM's process on the
/// host and stays there until the host is short of memory: the guest cannot give it back,
/// because its own reclaim and free page reporting cover its RAM only. The pages are clean
/// pages of the disk files, so taking them out costs nothing: they stay in the host's file
/// cache, and a page the guest touches again comes back from there. One VM's boot gets one
/// of these; a new boot (a new VM ID) starts a new one.
#[derive(Debug, Clone)]
pub struct MappedPagesTrim {
    last: Option<Instant>,
    logged_at: Option<Instant>,
    /// Since the last log line.
    given_back_kb: u64,
    trims: u32,
}

impl MappedPagesTrim {
    /// For a VM that has not had a give-back yet.
    pub fn new() -> MappedPagesTrim {
        MappedPagesTrim {
            last: None,
            logged_at: None,
            given_back_kb: 0,
            trims: 0,
        }
    }

    /// Whether to give back now, with the VM's process holding `mapped_kb` of the disks'
    /// pages: once there are [`MAPPED_PAGES_FLOOR_KB`] or more, and [`MAPPED_PAGES_GAP`] after
    /// the give-back before.
    pub fn due(&self, now: Instant, mapped_kb: u64) -> bool {
        mapped_kb >= MAPPED_PAGES_FLOOR_KB
            && self
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= MAPPED_PAGES_GAP)
    }

    /// Takes in a give-back at `now` that failed: the next try waits [`MAPPED_PAGES_GAP`], as
    /// after one that worked, and the log says nothing of it here.
    pub fn failed(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// Takes in a give-back at `now` that took the VM's process from `before_kb` to `after_kb`
    /// of the disks' pages, and returns the log line for it when one is due: the first
    /// give-back of a boot, then one line for every [`MAPPED_PAGES_LOG_INTERVAL`] that had
    /// give-backs.
    pub fn gave_back(&mut self, now: Instant, before_kb: u64, after_kb: u64) -> Option<String> {
        self.last = Some(now);
        self.given_back_kb += before_kb.saturating_sub(after_kb);
        self.trims += 1;
        let due = self
            .logged_at
            .is_none_or(|at| now.saturating_duration_since(at) >= MAPPED_PAGES_LOG_INTERVAL);
        if !due {
            return None;
        }
        let line = format!(
            "direct-mapped: gave back {} MiB of the mapped disks' pages from the VM's working set \
             in {} {} (they stay in the host's file cache); it holds {} MiB of them now",
            crate::memory::mib(self.given_back_kb),
            self.trims,
            if self.trims == 1 { "pass" } else { "passes" },
            crate::memory::mib(after_kb)
        );
        self.logged_at = Some(now);
        self.given_back_kb = 0;
        self.trims = 0;
        Some(line)
    }
}

impl Default for MappedPagesTrim {
    fn default() -> Self {
        MappedPagesTrim::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr1_the_mapped_disks_pages_are_given_back_once_there_are_enough() {
        let start = Instant::now();
        let trim = MappedPagesTrim::new();
        assert!(!trim.due(start, 0));
        assert!(!trim.due(start, MAPPED_PAGES_FLOOR_KB - 1));
        assert!(trim.due(start, MAPPED_PAGES_FLOOR_KB));
        // A boot's reads: given back at the first look.
        assert!(trim.due(start, 480 << 10));
    }

    #[test]
    fn nfr1_the_mapped_disks_pages_are_given_back_at_most_once_a_minute() {
        let start = Instant::now();
        let mut trim = MappedPagesTrim::new();
        trim.gave_back(start, 480 << 10, 1 << 10);
        assert!(!trim.due(start + MAPPED_PAGES_CHECK, 200 << 10));
        assert!(!trim.due(start + MAPPED_PAGES_GAP - Duration::from_secs(1), 200 << 10));
        assert!(trim.due(start + MAPPED_PAGES_GAP, 200 << 10));
        // Below the floor it waits, however long it has been.
        assert!(!trim.due(start + 10 * MAPPED_PAGES_GAP, 17 << 10));
    }

    #[test]
    fn nfr1_the_log_says_what_the_give_backs_did_without_a_line_for_each() {
        let start = Instant::now();
        let mut trim = MappedPagesTrim::new();
        let first = trim.gave_back(start, 478 << 10, 1 << 10).unwrap();
        assert_eq!(
            first,
            "direct-mapped: gave back 477 MiB of the mapped disks' pages from the VM's working \
             set in 1 pass (they stay in the host's file cache); it holds 1 MiB of them now"
        );
        assert_eq!(
            trim.gave_back(start + MAPPED_PAGES_GAP, 40 << 10, 2 << 10),
            None
        );
        assert_eq!(
            trim.gave_back(start + 2 * MAPPED_PAGES_GAP, 50 << 10, 3 << 10),
            None
        );
        let later = trim
            .gave_back(start + MAPPED_PAGES_LOG_INTERVAL, 36 << 10, 4 << 10)
            .unwrap();
        assert_eq!(
            later,
            "direct-mapped: gave back 117 MiB of the mapped disks' pages from the VM's working \
             set in 3 passes (they stay in the host's file cache); it holds 4 MiB of them now"
        );
    }

    fn disk() -> SystemDisk {
        SystemDisk {
            format: crate::direct_mapped_disk::FORMAT,
            bytes: 2304 << 20,
            sha256: "ab".repeat(32),
            super_bytes: 2048 << 20,
        }
    }

    fn support() -> ImageSupport {
        ImageSupport {
            fstab_suffix: "dsa_dax".into(),
            system_disk: disk(),
            verified_partitions: vec!["odm".into()],
        }
    }

    #[test]
    fn nfr1_an_image_says_which_mount_table_and_disk_its_direct_mapped_mode_boots_with() {
        let manifest = serde_json::json!({
            "version": "17.0.0-1-1",
            "direct_mapped": {
                "fstab_suffix": "dsa_dax",
                "verified_partitions": ["odm"],
                "system_disk": serde_json::to_value(disk()).unwrap(),
            },
        });
        assert_eq!(ImageSupport::from_manifest(&manifest), Some(support()));
        // An image from before the mode, one whose table cannot be named, and one without a
        // copy of its system disk this host makes all boot normally: the image's own disk is
        // compressed, and mapping it would give up dm-verity for nothing.
        assert_eq!(
            ImageSupport::from_manifest(&serde_json::json!({"version": "1"})),
            None
        );
        let odd = serde_json::json!({"direct_mapped": {"fstab_suffix": "dsa dax=1",
            "system_disk": serde_json::to_value(disk()).unwrap()}});
        assert_eq!(ImageSupport::from_manifest(&odd), None);
        let compressed = serde_json::json!({"direct_mapped": {"fstab_suffix": "dsa_dax",
            "uncompressed_erofs": false}});
        assert_eq!(ImageSupport::from_manifest(&compressed), None);
        let mut newer = disk();
        newer.format += 1;
        let newer = serde_json::json!({"direct_mapped": {"fstab_suffix": "dsa_dax",
            "system_disk": serde_json::to_value(newer).unwrap()}});
        assert_eq!(ImageSupport::from_manifest(&newer), None);
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

    #[test]
    fn nfr1_a_direct_mapped_boot_gives_the_copys_super_partition_size() {
        let image = "blkdevparts=d5a0d15c-5157-4e6d-8d5a-0000000005d5:1048576@1048576(vbmeta_a),\
                     8589934592@205520896(super:6b1ad5ad-0d5a-4dd3-9a6b-5a0a1d5a5a17) quiet";
        assert_eq!(
            kernel_cmdline(image, "nfit", &support()),
            format!(
                "blkdevparts=d5a0d15c-5157-4e6d-8d5a-0000000005d5:1048576@1048576(vbmeta_a),\
                 {}@205520896(super:6b1ad5ad-0d5a-4dd3-9a6b-5a0a1d5a5a17) quiet \
                 androidboot.boot_devices=nfit androidboot.fstab_suffix=dsa_dax",
                2048u64 << 20
            )
        );
        // Twice gives the same.
        let once = kernel_cmdline(image, "nfit", &support());
        assert_eq!(kernel_cmdline(&once, "nfit", &support()), once);
    }
}

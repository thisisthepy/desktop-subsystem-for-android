//! The direct-mapped mode's measurement: the same image booted with its read-only disks on
//! virtio-pmem (mapped with DAX) and on virtio-blk (cached by the guest), with Google Play and
//! one app window each time, and what the guest's page cache and the VM's memory came to in
//! each.
//!
//! The boot check (`overlays.rs`) takes the readings; this module reads what the guest says
//! about its mounts, what crosvm holds by kind, and writes the comparison for the job's page.
#![allow(dead_code)]

use super::{median, mib, Reading};

/// One erofs mount of the guest's mount table: its mount point, its source, and whether the
/// kernel maps its files (DAX).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErofsMount {
    pub point: String,
    pub source: String,
    pub dax: bool,
}

/// The erofs mounts of `mountinfo` (`/proc/self/mountinfo`). erofs lists `dax=always` among a
/// mount's options only while DAX is in effect: it drops the option, with a line in the kernel
/// log, from a device that cannot be mapped.
pub fn erofs_mounts(mountinfo: &str) -> Vec<ErofsMount> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mine, theirs) = line.split_once(" - ")?;
            let point = mine.split_whitespace().nth(4)?;
            let mut fields = theirs.split_whitespace();
            if fields.next()? != "erofs" {
                return None;
            }
            let source = fields.next().unwrap_or("");
            let options = fields.next().unwrap_or("");
            Some(ErofsMount {
                point: point.to_string(),
                source: source.to_string(),
                dax: options.split(',').any(|option| option == "dax=always"),
            })
        })
        .collect()
}

/// The read-only partitions a direct-mapped boot maps.
pub const MAPPED_POINTS: &[&str] = &["/system", "/vendor", "/product", "/system_ext"];

/// What is wrong with a direct-mapped boot's mounts: each of [`MAPPED_POINTS`], and the Google
/// apps image at `gapps_point`, must be erofs with DAX in effect.
pub fn mapping_problems(mounts: &[ErofsMount], gapps_point: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for point in MAPPED_POINTS.iter().copied().chain([gapps_point]) {
        // /system is mounted at / on a system-as-root boot.
        let found = mounts
            .iter()
            .rfind(|m| m.point == point || (point == "/system" && m.point == "/"));
        match found {
            None => problems.push(format!("{point} is not an erofs mount")),
            Some(m) if !m.dax => problems.push(format!(
                "{point} ({}) is not mapped: its mount has no dax=always, so the guest caches it",
                m.source
            )),
            Some(_) => {}
        }
    }
    problems
}

/// What a process holds by kind, from its `/proc/<pid>/status`, in KiB: anonymous memory, file
/// pages (the persistent memory's mapping of the image among them) and shared memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resident {
    pub anon_kb: u64,
    pub file_kb: u64,
    pub shmem_kb: u64,
}

pub fn resident(status: &str) -> Resident {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse().ok())
            .unwrap_or(0)
    };
    Resident {
        anon_kb: field("RssAnon:"),
        file_kb: field("RssFile:"),
        shmem_kb: field("RssShmem:"),
    }
}

/// One boot's numbers: the median reading, what crosvm held by kind, and the guest's erofs
/// mounts.
pub struct Boot<'a> {
    pub readings: &'a [Reading],
    pub crosvm: Resident,
    pub mounts: &'a [ErofsMount],
}

/// The comparison of the boot with the disks cached (`cached`) and the one with them mapped
/// (`mapped`), as Markdown for the job's page and JSON for the artifact. `None` when either
/// boot has no reading.
pub fn report(
    cached: &Boot<'_>,
    mapped: &Boot<'_>,
    setting: &super::Setting<'_>,
) -> Option<super::Report> {
    let c = median(cached.readings)?;
    let m = median(mapped.readings)?;
    let mut md = String::new();
    md.push_str("## Direct-mapped read-only disks (DAX) against the cached ones\n\n");
    md.push_str(&format!(
        "The same image, with {}, one window of {} open and untouched for a minute; the median of \
         {} and {} readings. Cached: the system and add-on disks on virtio-blk, read into the \
         guest's page cache. Mapped: the same disks on virtio-pmem, their erofs files mapped with \
         DAX. The VM has {} MiB and {} processors. Runner: {}.\n\n",
        setting.google_play,
        setting.app,
        cached.readings.len(),
        mapped.readings.len(),
        setting.memory_mb,
        setting.processors,
        setting.environment
    ));
    md.push_str("| MiB | Cached | Mapped | Change |\n|---|---|---|---|\n");
    let rows: [(&str, u64, u64); 10] = [
        (
            "guest page cache and buffers",
            c.guest_cache_kb,
            m.guest_cache_kb,
        ),
        ("guest in use", c.guest_used_kb, m.guest_used_kb),
        (
            "guest kernel and everything else",
            c.guest_kernel_kb,
            m.guest_kernel_kb,
        ),
        (
            "guest app memory (anonymous)",
            c.guest_anon_kb,
            m.guest_anon_kb,
        ),
        ("VM resident (crosvm)", c.vm_kb, m.vm_kb),
        ("VM private", c.vm_private_kb, m.vm_private_kb),
        (
            "crosvm anonymous (the guest's own memory)",
            cached.crosvm.anon_kb,
            mapped.crosvm.anon_kb,
        ),
        (
            "crosvm file pages (the mapped images among them)",
            cached.crosvm.file_kb,
            mapped.crosvm.file_kb,
        ),
        (
            "crosvm shared memory",
            cached.crosvm.shmem_kb,
            mapped.crosvm.shmem_kb,
        ),
        ("host total", c.host_total_kb, m.host_total_kb),
    ];
    for (name, before, after) in rows {
        let change = mib(after) as i64 - mib(before) as i64;
        md.push_str(&format!(
            "| {name} | {} | {} | {change:+} |\n",
            mib(before),
            mib(after)
        ));
    }
    md.push_str(
        "\nFile pages the VM maps from the image are clean and belong to the host's page cache: \
         the host can drop them and read them again, unlike the guest's own memory. The guest \
         keeps page structs for the mapped disks, about 1.6% of their size, in its kernel \
         memory.\n\n",
    );
    md.push_str("| Mapped boot's erofs mount | Source | DAX |\n|---|---|---|\n");
    for mount in mapped.mounts {
        md.push_str(&format!(
            "| {} | {} | {} |\n",
            mount.point,
            mount.source,
            if mount.dax { "yes" } else { "no" }
        ));
    }
    let row_json = |r: &Reading, crosvm: &Resident| {
        serde_json::json!({
            "guest_cache_kb": r.guest_cache_kb,
            "guest_used_kb": r.guest_used_kb,
            "guest_kernel_kb": r.guest_kernel_kb,
            "guest_anon_kb": r.guest_anon_kb,
            "vm_kb": r.vm_kb,
            "vm_private_kb": r.vm_private_kb,
            "host_total_kb": r.host_total_kb,
            "crosvm_anon_kb": crosvm.anon_kb,
            "crosvm_file_kb": crosvm.file_kb,
            "crosvm_shmem_kb": crosvm.shmem_kb,
        })
    };
    let json = serde_json::json!({
        "setting": {
            "google_play": setting.google_play,
            "app": setting.app,
            "memory_mb": setting.memory_mb,
            "processors": setting.processors,
            "environment": setting.environment,
        },
        "cached": row_json(&c, &cached.crosvm),
        "mapped": row_json(&m, &mapped.crosvm),
        "guest_cache_saved_kb": c.guest_cache_kb as i64 - m.guest_cache_kb as i64,
        "vm_saved_kb": c.vm_kb as i64 - m.vm_kb as i64,
        "mapped_mounts": mapped.mounts.iter().map(|mount| serde_json::json!({
            "point": mount.point,
            "source": mount.source,
            "dax": mount.dax,
        })).collect::<Vec<_>>(),
    });
    Some(super::Report { markdown: md, json })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
26 1 254:4 / / ro,relatime shared:1 - erofs /dev/block/dm-4 ro,user_xattr,acl,cache_strategy=readaround,dax=always
40 26 254:5 / /vendor ro,relatime shared:5 - erofs /dev/block/dm-5 ro,user_xattr,acl,cache_strategy=readaround,dax=always
41 26 254:6 / /product ro,relatime shared:6 - erofs /dev/block/dm-6 ro,user_xattr,acl,cache_strategy=readaround
42 26 254:7 / /system_ext ro,relatime shared:7 - erofs /dev/block/dm-7 ro,user_xattr,acl,dax=always
50 26 259:3 / /mnt/dsa-gapps ro,nosuid,nodev shared:9 - erofs /dev/block/by-name/dsa_gapps ro,dax=always
60 26 0:22 / /data rw - ext4 /dev/block/dm-9 rw
";

    #[test]
    fn nfr1_a_mount_the_kernel_could_not_map_is_found() {
        let mounts = erofs_mounts(MOUNTINFO);
        assert_eq!(mounts.len(), 5);
        assert!(mounts[0].dax && mounts[0].point == "/");
        let problems = mapping_problems(&mounts, "/mnt/dsa-gapps");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("/product (/dev/block/dm-6) is not mapped"));
        let without_gapps: Vec<ErofsMount> = mounts
            .into_iter()
            .filter(|m| m.point != "/mnt/dsa-gapps")
            .collect();
        assert!(mapping_problems(&without_gapps, "/mnt/dsa-gapps")
            .contains(&"/mnt/dsa-gapps is not an erofs mount".to_string()));
    }

    #[test]
    fn nfr1_what_crosvm_holds_is_read_by_kind() {
        let status = "Name:\tcrosvm\nVmRSS:\t  900000 kB\nRssAnon:\t  700000 kB\nRssFile:\t  \
                      180000 kB\nRssShmem:\t   20000 kB\n";
        assert_eq!(
            resident(status),
            Resident {
                anon_kb: 700_000,
                file_kb: 180_000,
                shmem_kb: 20_000
            }
        );
        assert_eq!(resident(""), Resident::default());
    }

    #[test]
    fn nfr1_the_comparison_shows_what_mapping_the_disks_saved() {
        let cached = Reading {
            guest_cache_kb: 400 << 10,
            vm_kb: 1200 << 10,
            ..Reading::default()
        };
        let mapped = Reading {
            guest_cache_kb: 150 << 10,
            vm_kb: 1000 << 10,
            ..Reading::default()
        };
        let mounts = erofs_mounts(MOUNTINFO);
        let setting = super::super::Setting {
            google_play: "LiteGapps",
            app: "Settings",
            memory_mb: 3072,
            processors: 2,
            environment: "test",
        };
        let report = report(
            &Boot {
                readings: &[cached],
                crosvm: Resident::default(),
                mounts: &[],
            },
            &Boot {
                readings: &[mapped],
                crosvm: Resident::default(),
                mounts: &mounts,
            },
            &setting,
        )
        .unwrap();
        assert!(report
            .markdown
            .contains("| guest page cache and buffers | 400 | 150 | -250 |"));
        assert!(report
            .markdown
            .contains("| VM resident (crosvm) | 1200 | 1000 | -200 |"));
        assert!(report
            .markdown
            .contains("| /product | /dev/block/dm-6 | no |"));
        assert_eq!(report.json["guest_cache_saved_kb"], 250 << 10);
        assert!(super::report(
            &Boot {
                readings: &[],
                crosvm: Resident::default(),
                mounts: &[]
            },
            &Boot {
                readings: &[mapped],
                crosvm: Resident::default(),
                mounts: &[]
            },
            &setting
        )
        .is_none());
    }
}

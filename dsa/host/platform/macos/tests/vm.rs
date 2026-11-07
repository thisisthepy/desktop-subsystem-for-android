//! What the macOS host boots: the kernel command line, the image's files and the VM's
//! description, before the Virtualization framework sees them. Paths here are Unix paths.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use dsa_host_macos::cmdline::{boot_devices, for_virtualization_framework, CONSOLE};
use dsa_host_macos::vm_config::{
    image_files, is_mac_address, kernel_fits, mac_address_for, Disk, GuestArch, MacVmConfig,
};

const HYPER_V_CMDLINE: &str = "console=ttyS0 printk.devkmsg=on panic=-1 8250.nr_uarts=1 \
    init=/init blkdevparts=6b1e:1@2(super) androidboot.boot_devices=vmbus \
    androidboot.fstab_suffix=dsa androidboot.hardware=cutf_cvm";

#[test]
fn fr2_the_console_is_the_virtio_console() {
    let cmdline = for_virtualization_framework(HYPER_V_CMDLINE, "", false);
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    assert_eq!(words[0], CONSOLE);
    assert_eq!(
        words.iter().filter(|w| w.starts_with("console=")).count(),
        1
    );
    assert!(!words
        .iter()
        .any(|w| w.starts_with("8250.") || w.starts_with("earlycon")));
    assert_eq!(words.iter().filter(|w| w.starts_with("panic=")).count(), 1);
}

#[test]
fn nfr2_freed_memory_is_reported_a_page_at_a_time() {
    // Once, from the host, whatever the image or the user's settings say.
    let cmdline = for_virtualization_framework(
        HYPER_V_CMDLINE,
        "page_reporting.page_reporting_order=9",
        false,
    );
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    let orders: Vec<&&str> = words
        .iter()
        .filter(|w| w.starts_with("page_reporting.page_reporting_order="))
        .collect();
    assert_eq!(orders, [&"page_reporting.page_reporting_order=0"]);
}

#[test]
fn fr2_android_finds_its_partitions_on_every_pci_slot() {
    let cmdline = for_virtualization_framework(HYPER_V_CMDLINE, "", false);
    assert!(!cmdline.contains("boot_devices=vmbus"));
    let devices = boot_devices();
    assert!(cmdline.split_whitespace().any(|w| w == devices));
    let list = devices.strip_prefix("androidboot.boot_devices=").unwrap();
    let slots: Vec<&str> = list.split(',').collect();
    assert_eq!(slots.len(), 31);
    assert!(slots.contains(&"pci0000:00/0000:00:05.0"));
    assert!(slots.contains(&"pci0000:00/0000:00:1f.0"));
}

#[test]
fn fr2_the_rest_of_the_image_command_line_is_kept_in_order() {
    let cmdline = for_virtualization_framework(HYPER_V_CMDLINE, "androidboot.extra=1", true);
    let at = |word: &str| {
        cmdline
            .find(word)
            .unwrap_or_else(|| panic!("{word} is missing"))
    };
    assert!(
        at("init=/init") < at("blkdevparts=")
            && at("blkdevparts=") < at("androidboot.hardware=cutf_cvm")
    );
    assert!(at("androidboot.hardware=cutf_cvm") < at("androidboot.extra=1"));
    // The pstore disk's settings appear once, from the host.
    assert_eq!(cmdline.matches("pstore_blk.blkdev=").count(), 1);
    let without = for_virtualization_framework("pstore_blk.blkdev=x init=/init", "", false);
    assert!(!without.contains("pstore_blk."));
}

#[test]
fn pr1_the_kernel_reads_the_bootconfig_that_carries_the_boot_secret() {
    // Once, whether the image's command line already says it or not.
    for image in [HYPER_V_CMDLINE, "init=/init bootconfig"] {
        let cmdline = for_virtualization_framework(image, "", false);
        let flags = cmdline.split_whitespace().filter(|w| *w == "bootconfig");
        assert_eq!(flags.count(), 1, "{cmdline}");
    }
}

#[test]
fn fr2_only_an_uncompressed_image_boots_on_apple_silicon() {
    let mut image = vec![0u8; 4096];
    image[56..60].copy_from_slice(b"ARM\x64");
    assert!(kernel_fits(&image, GuestArch::Arm64));
    assert!(!kernel_fits(&image, GuestArch::X86_64));
    let gzip = [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0];
    assert!(!kernel_fits(&gzip, GuestArch::Arm64));
    let mut bz = vec![0u8; 4096];
    bz[0x202..0x206].copy_from_slice(b"HdrS");
    assert!(kernel_fits(&bz, GuestArch::X86_64));
    assert_eq!(GuestArch::Arm64.kernel_name(), "Image");
    assert_eq!(GuestArch::X86_64.kernel_name(), "bzImage");
}

fn image_dir(name: &str, manifest: &str, files: &[&str]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("images")
        .join(format!("{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("image.json"), manifest).unwrap();
    for file in files {
        std::fs::write(dir.join(file), b"").unwrap();
    }
    dir
}

#[test]
fn fr14_an_image_of_two_disks_shares_its_system_disk_and_templates_the_users() {
    let dir = image_dir(
        "two",
        r#"{"version": "16.0.0-1", "arch": "arm64", "kernel_cmdline": "init=/init",
            "files": {"system.vhdx": {}, "data.vhdx": {}}}"#,
        &[],
    );
    let files = image_files(&dir, GuestArch::Arm64).unwrap();
    assert_eq!(files.kernel, dir.join("Image"));
    assert_eq!(files.initrd, dir.join("initrd.img"));
    assert_eq!(files.system_disk, Some(dir.join("system.vhdx")));
    assert_eq!(files.disk_template, dir.join("data.vhdx"));
    assert_eq!(files.kernel_cmdline, "init=/init");
    // An image for the other architecture is refused with the reason.
    assert!(image_files(&dir, GuestArch::X86_64)
        .unwrap_err()
        .contains("arm64"));
}

#[test]
fn fr14_raw_disks_in_the_image_are_used_as_they_are() {
    let dir = image_dir(
        "raw",
        r#"{"version": "16.0.0-1", "arch": "x86_64", "files": {"disk.vhdx": {}}}"#,
        &["disk.img"],
    );
    let files = image_files(&dir, GuestArch::X86_64).unwrap();
    assert_eq!(files.kernel, dir.join("bzImage"));
    assert_eq!(files.system_disk, None);
    assert_eq!(files.disk_template, dir.join("disk.img"));
}

fn config() -> MacVmConfig {
    MacVmConfig {
        kernel: PathBuf::from("/images/Image"),
        initrd: PathBuf::from("/tmp/boot.initrd"),
        cmdline: "console=hvc0".into(),
        memory_max_mb: 8192,
        memory_start_mb: 3072,
        processors: 4,
        disks: vec![Disk {
            path: PathBuf::from("/data/android.img"),
            read_only: false,
        }],
        console_log: None,
        network: true,
        mac_address: Some(mac_address_for("uid:501")),
        listen_ports: vec![5000],
    }
}

#[test]
fn nfr2_the_vm_starts_small_and_may_grow() {
    assert!(config().validate().is_ok());
    let mut bigger_start = config();
    bigger_start.memory_start_mb = 9000;
    assert!(bigger_start.validate().is_err());
    let mut tiny = config();
    tiny.memory_max_mb = 256;
    assert!(tiny.validate().is_err());
}

#[test]
fn fr2_a_vm_without_disks_or_with_relative_paths_is_refused() {
    let mut no_disk = config();
    no_disk.disks.clear();
    assert!(no_disk.validate().is_err());
    let mut relative = config();
    relative.kernel = Path::new("Image").to_path_buf();
    assert!(relative.validate().unwrap_err().contains("absolute"));
}

#[test]
fn fr28_each_user_keeps_one_locally_administered_mac_address() {
    let mac = mac_address_for("uid:501");
    assert!(is_mac_address(&mac));
    assert_eq!(mac, mac_address_for("uid:501"));
    assert_ne!(mac, mac_address_for("uid:502"));
    let first = u8::from_str_radix(&mac[0..2], 16).unwrap();
    assert_eq!(first & 0x03, 0x02, "locally administered and unicast");
    assert!(!is_mac_address("00:11:22:33:44"));
}

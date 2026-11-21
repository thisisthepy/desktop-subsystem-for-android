//! What a boot needs, decided once for every host: the image, the user's disk and the VM's
//! memory, with a backend of plain files standing for a host's disks.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use dsa_host_core::boot::{make_fresh_disk, BootPlanner, MemoryInputs};
use dsa_host_core::disk::{disk_image_version, version_file, DiskBackend};
use dsa_host_core::host_facts::HostFacts;
use dsa_host_core::image_files::ImageRulesOwned;
use dsa_host_core::memory::GrowthCeiling;
use dsa_host_core::settings::FileSettings;
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::store::ImageStore;

#[derive(Default)]
struct Files {
    keeps: bool,
    prepared: RefCell<Vec<(PathBuf, PathBuf)>>,
    convert: bool,
}

impl DiskBackend for Files {
    type Device = File;

    fn open(&self, disk: &Path, write: bool) -> Result<File, String> {
        OpenOptions::new()
            .read(true)
            .write(write)
            .open(disk)
            .map_err(|e| e.to_string())
    }

    fn create(&self, disk: &Path, template: Option<&Path>, size_gb: u32) -> Result<(), String> {
        std::fs::create_dir_all(disk.parent().unwrap()).map_err(|e| e.to_string())?;
        match template {
            Some(template) => std::fs::copy(template, disk).map(|_| ()),
            None => std::fs::write(disk, format!("empty {size_gb}")),
        }
        .map_err(|e| e.to_string())
    }

    fn write_beside(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }

    fn has_partitions(&self, _path: &Path) -> Result<bool, String> {
        Ok(false)
    }

    fn keeps_as_is(&self, _disk: &Path) -> bool {
        self.keeps
    }

    fn prepare_image_disk(&self, image_disk: &Path, cache: &Path) -> Result<PathBuf, String> {
        if !self.convert {
            return Ok(image_disk.to_path_buf());
        }
        let raw = cache.join(image_disk.file_name().unwrap());
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        std::fs::copy(image_disk, &raw).map_err(|e| e.to_string())?;
        self.prepared
            .borrow_mut()
            .push((image_disk.to_path_buf(), raw.clone()));
        Ok(raw)
    }
}

struct Facts(Option<u64>);

impl HostFacts for Facts {
    fn physical_memory_kb(&self) -> Option<u64> {
        self.0
    }
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "boot-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// An image folder of one disk, whose disk holds `content`.
fn image(dir: &Path, version: &str, content: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("image.json"),
        format!(r#"{{"version": "{version}", "kernel_cmdline": "a=b"}}"#),
    )
    .unwrap();
    std::fs::write(dir.join("bzImage"), "kernel").unwrap();
    std::fs::write(dir.join("initrd.img"), "initrd").unwrap();
    std::fs::write(dir.join("disk.img"), content).unwrap();
}

fn rules() -> ImageRulesOwned {
    ImageRulesOwned {
        kernel_names: vec!["bzImage".into()],
        disk_extensions: vec!["img".into()],
        arch: None,
        fallback_kernel: None,
        extra_cmdline: String::new(),
    }
}

fn memory(root: &Path, fixed_mb: Option<u32>, total_kb: Option<u64>) -> MemoryInputs {
    MemoryInputs {
        fixed_mb,
        settings: Arc::new(FileSettings::new(root.join("settings"))),
        user: "u".into(),
        ceiling: GrowthCeiling::HostLessReserve,
        facts: Arc::new(Facts(total_kb)),
        hypervisor_max_mb: None,
    }
}

fn planner(
    root: &Path,
    store: Option<Arc<ImageStore>>,
    configured_dir: Option<PathBuf>,
    backend: Files,
) -> BootPlanner<Files> {
    BootPlanner {
        store,
        configured_dir,
        rules: rules(),
        check: None,
        data_disk: root.join("user").join("data.img"),
        data_disk_size_gb: 3,
        image_cache: None,
        backend,
        memory: memory(root, None, Some(16 * 1024 * 1024)),
    }
}

fn store(root: &Path, version: &str, major: u32) -> Arc<ImageStore> {
    let installed = InstalledImage {
        version: version.into(),
        android_major: major,
        build: 1,
    };
    Arc::new(ImageStore::open(&root.join("store"), &installed).unwrap())
}

#[test]
fn fr28_boot_plan_resets_data_newer_than_the_image() {
    let root = scratch("erase");
    let store = store(&root, "15.0.1", 15);
    image(&store.image_dir("15.0.1"), "15.0.1", "android 15");
    let disk = root.join("user").join("data.img");
    std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
    std::fs::write(&disk, "data of android 16").unwrap();
    std::fs::write(version_file(&disk), "16.0.1").unwrap();

    let plan = planner(&root, Some(store), None, Files::default())
        .plan()
        .unwrap();

    assert_eq!(std::fs::read_to_string(&disk).unwrap(), "android 15");
    assert_eq!(disk_image_version(&disk).as_deref(), Some("15.0.1"));
    assert_eq!(plan.image_version.as_deref(), Some("15.0.1"));
    assert_eq!(plan.data_disk, disk);
}

#[test]
fn fr28_boot_plan_without_store_uses_the_configured_image() {
    let root = scratch("configured");
    let dir = root.join("image");
    image(&dir, "1.2.3", "android");
    let plan = planner(&root, None, Some(dir.clone()), Files::default())
        .plan()
        .unwrap();
    assert_eq!(plan.files.kernel, dir.join("bzImage"));
    assert_eq!(plan.files.kernel_cmdline, "a=b");
    assert_eq!(plan.image_version.as_deref(), Some("1.2.3"));
    assert_eq!(plan.cache_key(), "1.2.3");
    assert_eq!(std::fs::read_to_string(&plan.data_disk).unwrap(), "android");
}

#[test]
fn fr28_boot_plan_keeps_a_disk_of_the_same_image_and_names_local_without_a_version() {
    let root = scratch("local");
    let dir = root.join("image");
    image(&dir, "1.0", "android");
    std::fs::write(dir.join("image.json"), r#"{"kernel_cmdline": ""}"#).unwrap();
    let boot = planner(&root, None, Some(dir), Files::default());
    let plan = boot.plan().unwrap();
    assert_eq!(plan.image_version, None);
    assert_eq!(plan.cache_key(), "local");
    assert_eq!(
        disk_image_version(&plan.data_disk).as_deref(),
        Some("local")
    );
}

#[test]
fn fr28_boot_plan_makes_an_empty_disk_for_an_image_without_a_template() {
    let root = scratch("blank");
    let dir = root.join("image");
    image(&dir, "1.0", "android");
    std::fs::remove_file(dir.join("disk.img")).unwrap();
    std::fs::write(dir.join("system.img"), "system").unwrap();
    std::fs::remove_file(dir.join("image.json")).unwrap();
    let plan = planner(&root, None, Some(dir.clone()), Files::default())
        .plan()
        .unwrap();
    assert_eq!(plan.files.system_disk, Some(dir.join("system.img")));
    assert_eq!(std::fs::read_to_string(&plan.data_disk).unwrap(), "empty 3");
}

#[test]
fn fr28_boot_plan_refuses_an_image_the_hosts_check_refuses() {
    let root = scratch("check");
    let dir = root.join("image");
    image(&dir, "1.0", "android");
    let mut boot = planner(&root, None, Some(dir), Files::default());
    boot.check = Some(Box::new(|_| Err("not a kernel this host boots".into())));
    assert_eq!(boot.plan().unwrap_err(), "not a kernel this host boots");
    assert!(
        !boot.data_disk.exists(),
        "nothing is made for a refused image"
    );
}

#[test]
fn d9_boot_plan_uses_the_disks_the_host_prepares_and_prunes_old_copies() {
    let root = scratch("cache");
    let dir = root.join("image");
    image(&dir, "2.0", "android 2");
    std::fs::write(dir.join("system.img"), "system").unwrap();
    std::fs::remove_file(dir.join("disk.img")).unwrap();
    std::fs::write(dir.join("data.img"), "data template").unwrap();
    let cache = root.join("cache");
    std::fs::create_dir_all(cache.join("1.0")).unwrap();
    let mut boot = planner(
        &root,
        None,
        Some(dir.clone()),
        Files {
            convert: true,
            ..Files::default()
        },
    );
    boot.image_cache = Some(cache.clone());
    let plan = boot.plan().unwrap();
    assert_eq!(
        plan.files.system_disk,
        Some(cache.join("2.0").join("system.img"))
    );
    assert!(!cache.join("1.0").exists());
    assert!(cache.join("2.0").exists());
    assert_eq!(boot.backend.prepared.borrow().len(), 2);
}

#[test]
fn d9_a_disk_the_host_keeps_as_is_only_gets_its_version_recorded() {
    let root = scratch("keep");
    let dir = root.join("image");
    image(&dir, "2.0", "new android");
    let boot = planner(
        &root,
        None,
        Some(dir),
        Files {
            keeps: true,
            ..Files::default()
        },
    );
    std::fs::create_dir_all(boot.data_disk.parent().unwrap()).unwrap();
    std::fs::write(&boot.data_disk, "qcow2 of the user").unwrap();
    boot.plan().unwrap();
    assert_eq!(
        std::fs::read_to_string(&boot.data_disk).unwrap(),
        "qcow2 of the user"
    );
    assert_eq!(disk_image_version(&boot.data_disk).as_deref(), Some("2.0"));
}

#[test]
fn nfr2_boot_plan_carries_the_memory_plan() {
    let root = scratch("memory");
    let dir = root.join("image");
    image(&dir, "1.0", "android");
    let mut boot = planner(&root, None, Some(dir.clone()), Files::default());
    let plan = boot.plan().unwrap();
    assert_eq!(
        plan.memory.start_mb,
        dsa_host_core::host_config::DEFAULT_MEMORY_MB
    );
    assert!(plan.memory.max_mb.unwrap() > u64::from(plan.memory.start_mb));

    // The hypervisor's own ceiling caps the growth.
    boot.memory.hypervisor_max_mb = Some(4096);
    assert_eq!(boot.plan().unwrap().memory.max_mb, Some(4096));
    boot.memory.hypervisor_max_mb = Some(1024);
    assert_eq!(boot.plan().unwrap().memory.max_mb, None);

    // A fixed size never grows; a computer whose memory is unknown keeps the start size.
    boot.memory = memory(&root, Some(3000), Some(16 * 1024 * 1024));
    let fixed = boot.plan().unwrap().memory;
    assert_eq!((fixed.start_mb, fixed.max_mb), (3000, None));
    boot.memory = memory(&root, None, None);
    assert_eq!(boot.plan().unwrap().memory.max_mb, None);
}

#[test]
fn fr28_a_fresh_disk_is_made_from_the_current_image() {
    let root = scratch("fresh");
    let store = store(&root, "15.0.1", 15);
    image(&store.image_dir("15.0.1"), "15.0.1", "android 15");
    let disk = root.join("user").join("data.img");
    make_fresh_disk(
        &Files::default(),
        &rules(),
        (Some(&store), None),
        None,
        &disk,
        3,
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&disk).unwrap(), "android 15");
    assert_eq!(disk_image_version(&disk).as_deref(), Some("15.0.1"));
}

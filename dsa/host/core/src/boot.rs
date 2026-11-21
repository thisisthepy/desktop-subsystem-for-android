//! What a host decides before it starts a user's Android: which image, whether the user's
//! disk holds that image's Android, and how much memory the VM gets.
//!
//! The order is the same on every host: the current image, its files, the host's check of
//! them, its disks made ready, a disk that holds newer data than the image erased, the
//! user's disk brought to the image, old image copies removed, and the memory sized. What a
//! host does for itself comes in as data ([`ImageRulesOwned`], [`MemoryInputs`]) or through
//! its [`DiskBackend`]; what it does with the result (a VM configuration for its hypervisor)
//! stays its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::disk::{self, DiskBackend};
use crate::host_facts::HostFacts;
use crate::image_files::{ImageFiles, ImageRulesOwned};
use crate::memory::{memory_plan, GrowthCeiling, MemoryPlan};
use crate::settings::SettingsStore;
use crate::update::current::{current_image, prune_image_cache, CurrentImage};
use crate::update::store::ImageStore;

/// What sizes a VM's memory at each start.
pub struct MemoryInputs {
    /// The memory the configuration fixes, which never grows.
    pub fixed_mb: Option<u32>,
    pub settings: Arc<dyn SettingsStore>,
    /// The user whose limit applies.
    pub user: String,
    /// How far a VM with no limit may grow on this host.
    pub ceiling: GrowthCeiling,
    pub facts: Arc<dyn HostFacts>,
    /// The most the host's hypervisor lets a VM grow to, when it has a limit of its own.
    pub hypervisor_max_mb: Option<u64>,
}

impl MemoryInputs {
    /// The limit the user set, in MiB; 0 for automatic, and when the configuration fixes the
    /// memory.
    pub fn limit_mb(&self) -> u32 {
        match self.fixed_mb {
            Some(_) => 0,
            None => self
                .settings
                .get(&self.user)
                .map(|settings| settings.memory_limit_mb)
                .unwrap_or(0),
        }
    }

    /// The memory a VM gets now, which is logged unless the configuration fixes it.
    pub fn plan(&self) -> MemoryPlan {
        let limit = self.limit_mb();
        let total_mb = self.facts.physical_memory_kb().map(|kb| kb / 1024);
        let mut plan = memory_plan(self.fixed_mb, limit, total_mb, self.ceiling);
        if let Some(cap) = self.hypervisor_max_mb {
            let start = u64::from(plan.start_mb);
            plan.max_mb = plan
                .max_mb
                .map(|max| max.min(cap))
                .filter(|max| *max > start);
        }
        if self.fixed_mb.is_none() {
            eprintln!("{}", plan.describe(&self.user, limit));
        }
        plan
    }
}

/// What one boot of a user's Android needs, whatever the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPlan {
    /// The image's files. The disks are the ones the host attaches (see
    /// [`DiskBackend::prepare_image_disk`]).
    pub files: ImageFiles,
    /// The version of the image, when it names one.
    pub image_version: Option<String>,
    /// The user's disk, which holds this image's Android.
    pub data_disk: PathBuf,
    pub memory: MemoryPlan,
}

impl BootPlan {
    /// The name a cache folder or a recorded disk version takes: the image's version, or
    /// `local` for an image that names none.
    pub fn cache_key(&self) -> &str {
        self.image_version.as_deref().unwrap_or("local")
    }
}

/// Checks an image's files before anything is made from them.
pub type CheckImage = Box<dyn Fn(&ImageFiles) -> Result<(), String> + Send + Sync>;

/// Plans each boot of one user's Android.
pub struct BootPlanner<B: DiskBackend> {
    /// The update store, whose current image the next boot uses.
    pub store: Option<Arc<ImageStore>>,
    /// The image folder the configuration names, for a host without a store.
    pub configured_dir: Option<PathBuf>,
    pub rules: ImageRulesOwned,
    /// A check of the image's files by the host (a kernel format its hypervisor boots).
    pub check: Option<CheckImage>,
    pub data_disk: PathBuf,
    pub data_disk_size_gb: u32,
    /// The folder that holds one folder of converted disks per image version, for a host
    /// that cannot attach the image's disks as they are. Folders of other versions are
    /// removed. `None` for a host that attaches them as they are.
    pub image_cache: Option<PathBuf>,
    pub backend: B,
    pub memory: MemoryInputs,
}

impl<B: DiskBackend> BootPlanner<B> {
    /// Plans the next boot, making the user's disk ready for it. Call it only while the
    /// user's Android is stopped.
    pub fn plan(&self) -> Result<BootPlan, String> {
        let current = current_image(self.store.as_deref(), self.configured_dir.as_deref())?;
        let mut files = self.rules.read(&current.dir)?;
        if let Some(check) = &self.check {
            check(&files)?;
        }
        let key = current.cache_key().to_string();
        let cache = self.cache_for(&current, &key);
        if let Some(template) = files.disk_template.take() {
            files.disk_template = Some(self.backend.prepare_template_disk(&template, &cache)?);
        }
        if let Some(system) = files.system_disk.take() {
            files.system_disk = Some(self.backend.prepare_image_disk(&system, &cache)?);
        }
        // A disk holding a newer Android's data than this image, after the user chose an
        // older version and agreed to erase it, starts afresh.
        if let Some(major) = current.android_major {
            crate::data_reset::erase_if_newer(
                &self.data_disk,
                &disk::version_file(&self.data_disk),
                major,
            )?;
        }
        disk::prepare_user_disk(
            &self.backend,
            &self.data_disk,
            files.disk_template.as_deref(),
            &key,
            self.data_disk_size_gb,
        )?;
        if let Some(root) = &self.image_cache {
            prune_image_cache(root, &key);
        }
        Ok(BootPlan {
            files,
            image_version: current.version,
            data_disk: self.data_disk.clone(),
            memory: self.memory.plan(),
        })
    }

    fn cache_for(&self, current: &CurrentImage, key: &str) -> PathBuf {
        cache_folder(self.image_cache.as_deref(), current, key)
    }
}

fn cache_folder(root: Option<&Path>, current: &CurrentImage, key: &str) -> PathBuf {
    match root {
        Some(root) => root.join(key),
        None => current.dir.clone(),
    }
}

/// Makes the user's `disk` new from the current image, as a data reset does once the old disk
/// is out of the way: the image's disk template when it has one, else an empty disk of
/// `size_gb`. The arguments are those of [`BootPlanner`].
pub fn make_fresh_disk<B: DiskBackend>(
    backend: &B,
    rules: &ImageRulesOwned,
    source: (Option<&ImageStore>, Option<&Path>),
    image_cache: Option<&Path>,
    disk: &Path,
    size_gb: u32,
) -> Result<(), String> {
    let current = current_image(source.0, source.1)?;
    let files = rules.read(&current.dir)?;
    let key = current.cache_key().to_string();
    let cache = cache_folder(image_cache, &current, &key);
    let template = files
        .disk_template
        .as_deref()
        .map(|template| backend.prepare_template_disk(template, &cache))
        .transpose()?;
    disk::prepare_user_disk(backend, disk, template.as_deref(), &key, size_gb)
}

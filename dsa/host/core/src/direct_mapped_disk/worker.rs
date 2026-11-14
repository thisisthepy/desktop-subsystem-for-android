//! The service's maker of direct-mapped system disks: one background thread for the whole
//! service, which every user's boot asks for the current image's copy.
//!
//! When the service starts, and again whenever it is told an image was installed or replaced
//! ([`DirectMappedImages::kick`]), the thread looks at the current image. A copy made by an
//! earlier run is checked byte by byte against the image's hash first, since the file could
//! have changed while the service was not running; one that is missing, unfinished or wrong
//! is made (resuming an unfinished one) and checked. Copies of other images are removed. It
//! runs at the lowest priority the host offers, so the user's Android and the desktop go first.
//!
//! A boot uses a copy only once this run of the service has checked it
//! ([`DirectMappedImages::boot_disk`]). Until then it starts with the image's own disk, as a
//! boot outside the mode does, and Android is never kept waiting for the copy: at most for the
//! end of a check already under way.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{Failure, Progress, Ready, Store};
use crate::direct_mapped::ImageSupport;

/// What the thread asks for the image the next boot starts from: its version and folder.
pub type CurrentImage = Box<dyn Fn() -> Option<(String, PathBuf)> + Send + Sync>;
/// Where the thread writes what it did.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// How often the thread looks again without being told.
const LOOK_AGAIN: Duration = Duration::from_secs(600);
/// How long a boot waits for a check already under way.
const WAIT_FOR_CHECK: Duration = Duration::from_secs(20);
/// How often progress is written to the log.
const LOG_EVERY: Duration = Duration::from_secs(15);

#[derive(Default)]
struct State {
    kicked: bool,
    stop: bool,
    /// Versions whose copy this run checked.
    checked: HashSet<String>,
    /// The version whose copy is being checked now.
    checking: Option<String>,
    /// Versions whose copy cannot be made here (another format, a disk this host cannot copy),
    /// with the reason, so they are not tried again until the service restarts.
    failed: HashSet<String>,
}

/// The maker of direct-mapped system disks; see the module.
pub struct DirectMappedImages {
    store: Store,
    state: Mutex<State>,
    changed: Condvar,
    log: Log,
}

/// The version of the image in the folder `dir`: its `image.json`'s, else the folder's name.
pub fn image_version(dir: &Path) -> Option<String> {
    std::fs::read(dir.join("image.json"))
        .ok()
        .and_then(|text| serde_json::from_slice::<serde_json::Value>(&text).ok())
        .and_then(|m| m["version"].as_str().map(str::to_string))
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// The disk the image in `dir` ships for its system: `system.vhdx`, or a raw `system.img`.
pub fn image_system_disk(dir: &Path) -> Option<PathBuf> {
    ["system.vhdx", "system.img"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1e9)
}

impl DirectMappedImages {
    /// Starts the thread, which keeps the copies in `store` for the image `current` names.
    /// `lower_priority` runs first on the thread, to take it to the host's lowest priority.
    pub fn start(
        store: Store,
        current: CurrentImage,
        lower_priority: Box<dyn FnOnce() -> Result<(), String> + Send>,
        log: Log,
    ) -> Arc<DirectMappedImages> {
        let images = Arc::new(DirectMappedImages {
            store,
            state: Mutex::new(State {
                kicked: true,
                ..State::default()
            }),
            changed: Condvar::new(),
            log,
        });
        let worker = Arc::clone(&images);
        let spawned = std::thread::Builder::new()
            .name("dsa-direct-mapped".into())
            .spawn(move || {
                if let Err(why) = lower_priority() {
                    (worker.log)(&format!(
                        "direct-mapped: the disk maker runs at normal priority: {why}"
                    ));
                }
                worker.run(&current);
            });
        if let Err(why) = spawned {
            (images.log)(&format!(
                "direct-mapped: the disk maker could not start ({why}); Android boots with \
                 the image's own disks"
            ));
        }
        images
    }

    /// The store the copies are kept in.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Tells the thread to look at the current image now: one was installed or replaced, or a
    /// boot found no copy.
    pub fn kick(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.kicked = true;
        self.changed.notify_all();
    }

    /// Ends the thread after what it is doing; an unfinished copy is resumed next time.
    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stop = true;
        self.changed.notify_all();
    }

    fn stopping(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stop
    }

    /// The checked copy of the image `version` in `dir` and the image's support for the mode,
    /// for a boot; or why this boot starts with the image's own disk, in which case the
    /// thread is told to make the copy.
    pub fn boot_disk(&self, version: &str, dir: &Path) -> Result<(Ready, ImageSupport), String> {
        let support = ImageSupport::from_image_dir(dir).ok_or_else(|| {
            "the image has no direct-mapped system disk this host makes (an image from before \
             the mode, or one made for another format)"
                .to_string()
        })?;
        let deadline = Instant::now() + WAIT_FOR_CHECK;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.checking.as_deref() == Some(version) && Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if state.failed.contains(version) {
            return Err(format!(
                "the uncompressed copy of {version}'s system disk cannot be made here (the \
                 service log says why)"
            ));
        }
        let checked = state.checked.contains(version);
        drop(state);
        match self.store.ready(version, &support.system_disk) {
            Some(ready) if checked => Ok((ready, support)),
            _ => {
                self.kick();
                Err(
                    "the uncompressed copy of the system disk is not made and checked yet; it is \
                     being made in the background"
                        .into(),
                )
            }
        }
    }

    fn run(&self, current: &CurrentImage) {
        loop {
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let until = Instant::now() + LOOK_AGAIN;
                while !state.kicked && !state.stop && Instant::now() < until {
                    let left = until.saturating_duration_since(Instant::now());
                    state = self
                        .changed
                        .wait_timeout(state, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                if state.stop {
                    return;
                }
                state.kicked = false;
            }
            if let Some((version, dir)) = current() {
                self.keep(&version, &dir);
            }
        }
    }

    /// Makes sure a checked copy of the image `version` in `dir` is there.
    fn keep(&self, version: &str, dir: &Path) {
        let log = &self.log;
        let Some(support) = ImageSupport::from_image_dir(dir) else {
            // Nothing to make; copies of earlier images go.
            for removed in self.store.remove_others(version) {
                log(&format!("direct-mapped: removed {}", removed.display()));
            }
            return;
        };
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.checked.contains(version) || state.failed.contains(version) {
                return;
            }
        }
        for removed in self.store.remove_others(version) {
            log(&format!(
                "direct-mapped: removed {}, the copy of an image that was replaced",
                removed.display()
            ));
        }
        let expected = &support.system_disk;
        let stop = || self.stopping();
        // A copy an earlier run made is checked again before any boot uses it.
        if self.store.path(version).exists() {
            self.set_checking(Some(version));
            let started = Instant::now();
            let result =
                self.store
                    .verify(version, expected, &mut self.progress_log(version), &stop);
            self.set_checking(None);
            match result {
                Ok(true) => {
                    log(&format!(
                        "direct-mapped: the system disk copy of {version} matches the image's \
                         hash (checked in {:.1} s)",
                        started.elapsed().as_secs_f64()
                    ));
                    self.mark_checked(version);
                    return;
                }
                Ok(false) => log(&format!(
                    "direct-mapped: the system disk copy of {version} does not match the image's \
                     hash; it was removed and is made again"
                )),
                Err(Failure::Stopped) => return,
                Err(why) => log(&format!(
                    "direct-mapped: checking the system disk copy of {version}: {why}"
                )),
            }
        }
        let Some(source) = image_system_disk(dir) else {
            log(&format!(
                "direct-mapped: the image {version} has no system disk to copy"
            ));
            return;
        };
        // The estimate the log starts with: reading the image's disk and writing the copy
        // at a laptop SSD's few hundred MB/s, then reading the copy again to check it.
        let total = expected.bytes;
        log(&format!(
            "direct-mapped: making the uncompressed system disk of {version} from {}: {} to \
             write into {} (about {} more on this disk than the image), then checked against \
             the image's hash; it runs in the background at low priority, and Android boots \
             with the image's own disk until it is done",
            source.display(),
            gigabytes(total),
            self.store.folder().display(),
            gigabytes(total.saturating_sub(std::fs::metadata(&source).map_or(0, |m| m.len()))),
        ));
        let started = Instant::now();
        let result = self.store.produce(
            version,
            &source,
            expected,
            &support.verified_partitions,
            &mut self.progress_log(version),
            &stop,
        );
        match result {
            Ok(ready) => {
                log(&format!(
                    "direct-mapped: made {} ({}) in {:.0} s; it matches the image's hash, and \
                     the next start of Android maps it",
                    ready.path.display(),
                    gigabytes(ready.disk_bytes),
                    started.elapsed().as_secs_f64()
                ));
                self.mark_checked(version);
            }
            Err(Failure::Stopped) => log(&format!(
                "direct-mapped: stopped making the system disk of {version} after {:.0} s; the \
                 next start of the service resumes it",
                started.elapsed().as_secs_f64()
            )),
            Err(Failure::Mismatch(why)) => {
                log(&format!(
                    "direct-mapped: the system disk made for {version} does not match the image's \
                     hash, twice ({why}); Android boots with the image's own disk"
                ));
                self.mark_failed(version);
            }
            Err(Failure::Failed(why)) => {
                log(&format!(
                    "direct-mapped: the system disk of {version} cannot be made: {why}; Android \
                     boots with the image's own disk"
                ));
                self.mark_failed(version);
            }
        }
    }

    fn progress_log(&self, version: &str) -> impl FnMut(&Progress) + '_ {
        let version = version.to_string();
        let started = Instant::now();
        let mut last = Instant::now();
        move |progress: &Progress| {
            let done = progress.done >= progress.total;
            if !done && last.elapsed() < LOG_EVERY {
                return;
            }
            last = Instant::now();
            let percent = (progress.done as u128 * 100)
                .checked_div(progress.total as u128)
                .unwrap_or(100);
            let elapsed = started.elapsed().as_secs_f64();
            let rate = progress.done as f64 / elapsed.max(0.001);
            let left = (progress.total.saturating_sub(progress.done)) as f64 / rate.max(1.0);
            (self.log)(&format!(
                "direct-mapped: {} the system disk of {version}: {percent}% ({} of {}), \
                 {elapsed:.0} s so far, about {left:.0} s to go",
                progress.phase,
                gigabytes(progress.done),
                gigabytes(progress.total),
            ));
        }
    }

    fn set_checking(&self, version: Option<&str>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.checking = version.map(str::to_string);
        self.changed.notify_all();
    }

    fn mark_checked(&self, version: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.checked.insert(version.to_string());
        self.changed.notify_all();
    }

    fn mark_failed(&self, version: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.failed.insert(version.to_string());
        self.changed.notify_all();
    }

    /// Waits until the copy of `version` is checked, the thread gave up on it, or `timeout`
    /// passed; for tests and for the measurement. Returns whether it is checked.
    pub fn wait_checked(&self, version: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.checked.contains(version) {
                return true;
            }
            if state.failed.contains(version) || Instant::now() >= deadline {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

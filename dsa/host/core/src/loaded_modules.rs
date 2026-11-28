//! Which libraries a process loaded at each stage of its start, for its log.
//!
//! A library's writable data counts in the process's private memory whether or not the process
//! needs the library. The graphics renderer was seen holding another vendor's GPU driver
//! (NVIDIA's on a computer that renders on Intel) and Intel's Direct3D 9 and video acceleration
//! drivers, 21 MiB of module data between them, and its log did not say what loaded them. The
//! renderer now lists, after each stage of its start and later as it serves, the libraries that
//! stage loaded, so the stage that brings in a library nobody asked for shows in the log.
//!
//! The platform lists the loaded libraries' file names; this keeps the ones seen and says
//! which are new.

use std::collections::BTreeSet;

/// The libraries seen loaded so far.
#[derive(Debug, Default, Clone)]
pub struct ModuleWatch {
    /// File names, lowercased: Windows names files without regard to case.
    seen: BTreeSet<String>,
}

impl ModuleWatch {
    /// Starts from the libraries `loaded` now, which no later line names.
    pub fn new(loaded: impl IntoIterator<Item = String>) -> ModuleWatch {
        ModuleWatch {
            seen: loaded.into_iter().map(|name| name.to_lowercase()).collect(),
        }
    }

    /// The log line for the libraries among `loaded` that were not loaded before, as loaded
    /// while `stage` ran, in order of name; `None` when the stage loaded none. A library seen
    /// once is never named again, even if it is unloaded and loaded again.
    pub fn note(
        &mut self,
        stage: &str,
        loaded: impl IntoIterator<Item = String>,
    ) -> Option<String> {
        let mut new: Vec<String> = Vec::new();
        for name in loaded {
            if self.seen.insert(name.to_lowercase()) {
                new.push(name);
            }
        }
        if new.is_empty() {
            return None;
        }
        new.sort_by_key(|name| name.to_lowercase());
        Some(format!(
            "libraries loaded while {stage} ({}): {}",
            new.len(),
            new.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn nfr1_a_stage_names_only_the_libraries_it_loaded() {
        let mut watch = ModuleWatch::new(names(&["dsa-gfx.exe", "ntdll.dll", "KERNEL32.DLL"]));
        assert_eq!(
            watch
                .note(
                    "the Vulkan probe",
                    names(&[
                        "dsa-gfx.exe",
                        "ntdll.dll",
                        "kernel32.dll",
                        "vulkan-1.dll",
                        "igvk64.dll"
                    ])
                )
                .as_deref(),
            Some("libraries loaded while the Vulkan probe (2): igvk64.dll, vulkan-1.dll")
        );
    }

    #[test]
    fn nfr1_a_stage_that_loads_nothing_writes_no_line_and_a_library_is_named_once() {
        let mut watch = ModuleWatch::new(names(&["ntdll.dll"]));
        assert_eq!(watch.note("the probe", names(&["ntdll.dll"])), None);
        assert!(watch
            .note("gfxstream's start", names(&["ntdll.dll", "igd9dxva64.dll"]))
            .is_some());
        // Unloaded and loaded again, or named in another case: not new.
        assert_eq!(watch.note("serving", names(&["IGD9DXVA64.DLL"])), None);
    }
}

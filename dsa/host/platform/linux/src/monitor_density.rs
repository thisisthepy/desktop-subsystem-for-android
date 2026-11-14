//! The scale of the user's main monitor, for the density Android starts at.
//!
//! Every app starts at the guest display's density, and Android relaunches an activity whose
//! density changes unless the app says it handles that itself. So Android starts at the
//! density a window on the user's main monitor gives its task (130 dpi at a scale of 100%,
//! 196 at 150%), and the apps that open there are never asked for another one right after they
//! start.
//!
//! The service runs as root outside the user's graphical session and cannot ask the desktop
//! about its monitors. The window manager, which runs in that session, hears each output's
//! scale from the Wayland compositor and records the main one's DPI in the user's `tmp` folder
//! whenever it changes ([`DensityRecorder`]). Wayland names no main output, so the main one is
//! the first the compositor announced, which is the primary one on the desktops that have one.
//! The service reads the record when it starts the VM ([`boot_density`]). Without a record
//! Android starts at the density of a 100% monitor.

use std::path::{Path, PathBuf};

use dsa_host_core::guest_display::{
    self, density_record_text, parse_density_record, with_display_density,
};

/// The record's name in the user's `tmp` folder.
const RECORD_NAME: &str = "monitor-density.txt";

/// More than any real record.
const MAX_RECORD_BYTES: usize = 64;

/// A monitor's dots per inch at a Wayland output scale of 1.
const BASE_DPI: u32 = 96;

/// Where the record is, in the user's `tmp` folder `tmp`.
pub fn record_path(tmp: &Path) -> PathBuf {
    tmp.join(RECORD_NAME)
}

/// The main monitor's DPI recorded in `tmp`, when there is a record with one in it.
pub fn read_record(tmp: &Path) -> Option<u32> {
    let bytes = std::fs::read(record_path(tmp)).ok()?;
    let text = std::str::from_utf8(bytes.get(..MAX_RECORD_BYTES.min(bytes.len()))?).ok()?;
    parse_density_record(text)
}

/// Writes the record whole, so the service never reads half of one.
pub fn write_record(tmp: &Path, dpi: u32) -> std::io::Result<()> {
    std::fs::create_dir_all(tmp)?;
    let path = record_path(tmp);
    let partial = path.with_extension("txt.new");
    std::fs::write(&partial, density_record_text(dpi))?;
    std::fs::rename(&partial, &path)
}

/// The density Android starts at for the main monitor recorded in `tmp`: that monitor's, or a
/// 100% monitor's without a record.
pub fn boot_density(tmp: &Path) -> u32 {
    guest_display::boot_density(read_record(tmp))
}

/// `cmdline` with the display's density set for the main monitor recorded in `tmp`.
pub fn with_boot_density(cmdline: &str, tmp: &Path) -> String {
    with_display_density(cmdline, boot_density(tmp))
}

/// The DPI of the main output among `outputs`, each its global name and its scale: the one
/// announced first, which has the lowest name. `None` when there is no output.
pub fn main_output_dpi(outputs: impl IntoIterator<Item = (u32, i32)>) -> Option<u32> {
    outputs
        .into_iter()
        .min_by_key(|(name, _)| *name)
        .map(|(_, scale)| BASE_DPI * scale.max(1) as u32)
}

/// Keeps the record up to date from the window manager's outputs.
pub struct DensityRecorder {
    tmp: Option<PathBuf>,
    last: Option<u32>,
}

impl DensityRecorder {
    /// Records in `tmp`, the user's `tmp` folder; with none, records nothing.
    pub fn new(tmp: Option<PathBuf>) -> DensityRecorder {
        DensityRecorder { tmp, last: None }
    }

    /// Records the main output's DPI among `outputs` (each its global name and its scale), if
    /// it changed. No outputs leave the last record. A failure is returned once, not on every
    /// call.
    pub fn record(&mut self, outputs: impl IntoIterator<Item = (u32, i32)>) -> Result<(), String> {
        let Some(dpi) = main_output_dpi(outputs) else {
            return Ok(());
        };
        if self.last == Some(dpi) {
            return Ok(());
        }
        self.last = Some(dpi);
        match &self.tmp {
            Some(tmp) => write_record(tmp, dpi)
                .map_err(|e| format!("recording the main monitor's scale: {e}")),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-density-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fr4_android_starts_at_the_main_monitors_density() {
        let dir = empty_dir("record");
        assert_eq!(boot_density(&dir), 130, "no record is a 100% monitor");
        let mut recorder = DensityRecorder::new(Some(dir.clone()));
        // The first output announced is the main one, whatever the others' scales.
        recorder.record([(12, 1), (7, 2)]).unwrap();
        assert_eq!(read_record(&dir), Some(192));
        assert_eq!(boot_density(&dir), 262);
        assert_eq!(
            with_boot_density(
                "androidboot.dsa.display_dpi=130 androidboot.lcd_density=130",
                &dir
            ),
            "androidboot.dsa.display_dpi=262 androidboot.lcd_density=262"
        );
        // No outputs leave the record as it was.
        recorder.record([]).unwrap();
        assert_eq!(read_record(&dir), Some(192));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

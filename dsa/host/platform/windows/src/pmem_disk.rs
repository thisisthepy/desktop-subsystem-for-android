//! The disks of the host's direct-mapped mode on Windows: the uncompressed system disk the
//! service makes, and the add-on disk, attached as Hyper-V virtual persistent memory (see
//! `dsa_host_core::direct_mapped`).
//!
//! Virtual persistent memory maps its file into the VM as it is, so it takes a flat image: a
//! fixed VHD (HCS's `Vhd1`), the disk's bytes followed by a 512-byte footer. The add-on disk is
//! one already. The system disk is the copy `dsa_host_core::direct_mapped_disk` makes from the
//! image's `system.vhdx` in the background, after the image's download was checked, with its
//! read-only partitions uncompressed, and checks against the hash the image's signed
//! `image.json` gives. It is kept in the machine's DSA folder, in `direct-mapped`
//! ([`crate::paths::MachineRoot::direct_mapped`]), where only SYSTEM writes
//! ([`DIRECT_MAPPED_SDDL`]); administrators may read and delete, and the VMs read.
//!
//! The service makes it at the lowest priority Windows has (`THREAD_MODE_BACKGROUND_BEGIN`:
//! idle CPU, very low I/O and memory priority), so a running Android and the desktop go first.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use dsa_host_core::direct_mapped_disk::{
    worker::{image_version, CurrentImage},
    DirectMappedImages, Store,
};

/// The `direct-mapped` folder: SYSTEM writes; administrators read and may delete (an uninstall
/// removes it); Hyper-V's virtual machines (`S-1-5-83-0`) read, so a VM opens its system disk
/// read only and never with write access. Inherited by what the folder holds.
pub const DIRECT_MAPPED_SDDL: &str =
    "D:P(A;OICI;FA;;;SY)(A;OICI;0x1300e9;;;BA)(A;OICI;0x1200a9;;;S-1-5-83-0)";

const FOOTER_BYTES: u64 = 512;

/// The size of the disk in the fixed VHD at `path`, without its footer: the footer's cookie
/// (`conectix`) and disk type (fixed) are checked, and the size it gives must be the file's.
pub fn fixed_vhd_disk_bytes(path: &Path) -> io::Result<u64> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length < FOOTER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too short to be a VHD", path.display()),
        ));
    }
    file.seek(SeekFrom::Start(length - FOOTER_BYTES))?;
    let mut footer = [0u8; FOOTER_BYTES as usize];
    file.read_exact(&mut footer)?;
    let size = u64::from_be_bytes(footer[48..56].try_into().expect("eight bytes"));
    let kind = u32::from_be_bytes(footer[60..64].try_into().expect("four bytes"));
    if &footer[0..8] != b"conectix" || kind != 2 || size != length - FOOTER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a fixed VHD", path.display()),
        ));
    }
    Ok(size)
}

/// The image the next boot starts from, for the disk maker: the update store's current one,
/// else the one the configuration names.
pub fn current_image(
    store: Option<Arc<dsa_host_core::update::store::ImageStore>>,
    configured: Option<std::path::PathBuf>,
) -> CurrentImage {
    Box::new(move || {
        let dir = match &store {
            Some(store) => {
                let state = store.state().ok()?;
                store.image_dir(&state.current.version)
            }
            None => configured.as_ref()?.parent()?.to_path_buf(),
        };
        Some((image_version(&dir)?, dir))
    })
}

/// Takes the calling thread to Windows' background mode: idle CPU priority, very low I/O and
/// memory priority, for as long as it runs.
pub fn lower_this_thread() -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
        };
        // SAFETY: the pseudo handle of the calling thread, which needs no closing.
        if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) } == 0 {
            return Err(io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

/// Starts the service's disk maker over the folder `folder`, made and closed to all but
/// SYSTEM first ([`DIRECT_MAPPED_SDDL`]), for the image `current` names.
pub fn start(
    folder: &Path,
    current: CurrentImage,
    log: dsa_host_core::direct_mapped_disk::worker::Log,
) -> Arc<DirectMappedImages> {
    if let Err(why) = fs::create_dir_all(folder) {
        log(&format!(
            "direct-mapped: creating {}: {why}",
            folder.display()
        ));
    }
    #[cfg(windows)]
    if let Err(why) = crate::user_disk::protect_folder_as(folder, DIRECT_MAPPED_SDDL) {
        log(&format!("direct-mapped: {why}"));
    }
    DirectMappedImages::start(
        Store::new(folder),
        current,
        Box::new(lower_this_thread),
        log,
    )
}

/// The version and folder of the image whose system disk is `system_image`.
pub fn image_of(system_image: &Path) -> Option<(String, std::path::PathBuf)> {
    let dir = system_image.parent()?.to_path_buf();
    Some((image_version(&dir)?, dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-pmem-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixed_vhd(bytes: u64) -> Vec<u8> {
        let mut data = vec![0u8; bytes as usize + 512];
        let footer = &mut data[bytes as usize..];
        footer[0..8].copy_from_slice(b"conectix");
        footer[48..56].copy_from_slice(&bytes.to_be_bytes());
        footer[60..64].copy_from_slice(&2u32.to_be_bytes());
        data
    }

    #[test]
    fn nfr1_only_a_fixed_vhd_can_be_mapped() {
        let dir = scratch("vhd");
        let path = dir.join("addons.vhd");
        fs::write(&path, fixed_vhd(2 << 20)).unwrap();
        assert_eq!(fixed_vhd_disk_bytes(&path).unwrap(), 2 << 20);
        let mut dynamic = fixed_vhd(2 << 20);
        let at = dynamic.len() - 512 + 60;
        dynamic[at..at + 4].copy_from_slice(&3u32.to_be_bytes());
        fs::write(&path, dynamic).unwrap();
        assert!(fixed_vhd_disk_bytes(&path).is_err());
        fs::write(&path, vec![0u8; 100]).unwrap();
        assert!(fixed_vhd_disk_bytes(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr1_the_image_of_a_system_disk_is_its_folders() {
        let dir = scratch("image");
        let image = dir.join("17.0.0-1-2");
        fs::create_dir_all(&image).unwrap();
        assert_eq!(
            image_of(&image.join("system.vhdx")),
            Some(("17.0.0-1-2".to_string(), image.clone()))
        );
        fs::write(image.join("image.json"), r#"{"version": "17.0.0-1-3"}"#).unwrap();
        assert_eq!(
            image_of(&image.join("system.vhdx")).map(|(v, _)| v),
            Some("17.0.0-1-3".to_string())
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

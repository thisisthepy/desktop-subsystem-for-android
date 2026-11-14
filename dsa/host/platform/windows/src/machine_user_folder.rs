//! Each user's folder in the machine's DSA folder (`users\<SID>`), where the service keeps the
//! disks it writes for that user: the add-on disk and the pstore disk.
//!
//! The service runs as SYSTEM. A file it writes in the user's own `%LOCALAPPDATA%\DSA` is in a
//! folder the user can change, so these disks live where only SYSTEM and Administrators write.
//! The user may read the folder and what is in it, and so may Hyper-V's virtual machines, which
//! the service lets into each disk when a VM starts.
//!
//! Releases before this one kept both disks in the user's folder. The service moves them once
//! ([`migrate_disks`]): each old file is opened as itself, refused when it is a link or has
//! another name, checked to be at the path the service expected, copied into the new place,
//! and removed through the handle it was read through.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dsa_host_core::addon_disk::{AddonDisk, Format};

use crate::paths::{MachineRoot, UserRoot};
use crate::safe_fs;

/// One user's folder: SYSTEM and Administrators write it; the user and Hyper-V's virtual
/// machines read it; inherited by everything in it, and nothing inherited from above.
pub fn folder_sddl(sid: &str) -> String {
    format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;{sid})\
         (A;OICI;0x1200a9;;;S-1-5-83-0)"
    )
}

/// Makes the user's folder in the machine's folder, with [`folder_sddl`], and returns it.
pub fn prepare(machine: &MachineRoot, sid: &str) -> Result<PathBuf, String> {
    if !crate::vm::is_sid(sid) {
        return Err(format!("{sid:?} is not a Windows SID, so it has no folder"));
    }
    let dir = machine.user_dir(sid);
    let mut prepared = PREPARED.lock().unwrap_or_else(|e| e.into_inner());
    if prepared.contains(&dir) && dir.is_dir() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    crate::user_disk::protect_folder_as(&dir, &folder_sddl(sid))?;
    prepared.insert(dir.clone());
    Ok(dir)
}

/// The folders this process has given their access list, so it is set once per run.
static PREPARED: Mutex<std::collections::BTreeSet<PathBuf>> =
    Mutex::new(std::collections::BTreeSet::new());

/// The moves go one at a time, whoever starts them.
static MOVING: Mutex<()> = Mutex::new(());

/// Moves the add-on disk (and its pending one) and the pstore disk an earlier release kept in
/// the user's folder `user` into the machine's folder for them, once. A disk that is already
/// in its new place wins, and the old one is removed. A file that is a link, has another name
/// or is not where it should be is left alone and reported; so is a disk that is not one the
/// host would write. Returns what to log.
pub fn migrate_disks(machine: &MachineRoot, user: &UserRoot, sid: &str) -> Vec<String> {
    let _moving = MOVING.lock().unwrap_or_else(|e| e.into_inner());
    let mut notes = Vec::new();
    let old_addons = user.old_addon_disk();
    let old_pstore = user.old_pstore_disk();
    let old_pending = beside(&old_addons, ".next");
    let old_partials = [beside(&old_addons, ".part"), beside(&old_pending, ".part")];
    let mut all = vec![old_addons.clone(), old_pending.clone(), old_pstore.clone()];
    all.extend(old_partials.iter().cloned());
    if !all
        .iter()
        .any(|path| std::fs::symlink_metadata(path).is_ok())
    {
        return notes;
    }
    // The user's folder stays the folder it is checked to be for as long as files move out
    // of it.
    let _guard = match safe_fs::guard_below(user.dir(), user.dir()) {
        Ok(guard) => guard,
        Err(why) => {
            notes.push(format!(
                "{sid}'s disks stay in their folder, which the service does not use: {why}"
            ));
            return notes;
        }
    };
    if let Err(why) = prepare(machine, sid) {
        notes.push(format!("{sid}'s disks stay in their folder: {why}"));
        return notes;
    }
    let disk = AddonDisk::new(machine.user_addon_disk(sid), Format::Vhd);
    for (old, pending) in [(&old_addons, false), (&old_pending, true)] {
        if let Some(note) = move_addon_disk(old, &disk, pending) {
            notes.push(note);
        }
    }
    for partial in &old_partials {
        if let Err(why) = safe_fs::remove_plain_file(partial) {
            notes.push(format!("a half-written add-on disk stays: {why}"));
        }
    }
    if let Some(note) = move_pstore_disk(&old_pstore, &machine.user_pstore_disk(sid)) {
        notes.push(note);
    }
    notes
}

fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_os_string();
    text.push(suffix);
    PathBuf::from(text)
}

fn move_addon_disk(old: &Path, disk: &AddonDisk, pending: bool) -> Option<String> {
    let mut file = match safe_fs::open_existing(old) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(why) => return Some(format!("the add-on disk stays where it is: {why}")),
    };
    let target = if pending {
        disk.pending_path()
    } else {
        disk.path().to_path_buf()
    };
    if target.exists() {
        return Some(match safe_fs::delete_open(&file) {
            Ok(()) => format!(
                "removed the earlier add-on disk {}: {} is in its place already",
                old.display(),
                target.display()
            ),
            Err(why) => format!("the earlier add-on disk {} stays: {why}", old.display()),
        });
    }
    match disk.take_over(&mut file, pending) {
        Ok(addons) => {
            let removed = safe_fs::delete_open(&file);
            let mut note = format!(
                "moved the add-on disk {} to {} ({} add-ons)",
                old.display(),
                target.display(),
                addons.len()
            );
            if let Err(why) = removed {
                note.push_str(&format!("; the old copy stays: {why}"));
            }
            Some(note)
        }
        Err(why) => Some(format!(
            "the add-on disk {} is not moved and Android starts without it until the add-ons \
             are installed again: {why}",
            old.display()
        )),
    }
}

/// The pstore disk holds the guest kernel's last log: copied when it is the size a pstore
/// disk has (the next start checks the rest and makes it again if it is damaged), and the old
/// one removed either way.
fn move_pstore_disk(old: &Path, new: &Path) -> Option<String> {
    let mut file = match safe_fs::open_existing(old) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(why) => return Some(format!("the pstore disk stays where it is: {why}")),
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if !new.exists() && size == crate::pstore_disk::FILE_BYTES {
        let mut data = Vec::with_capacity(size as usize);
        let copied = (&mut file)
            .take(size)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())
            .and_then(|_| std::fs::write(new, &data).map_err(|e| e.to_string()));
        if let Err(why) = copied {
            let _ = std::fs::remove_file(new);
            return Some(format!(
                "the pstore disk {} is not moved, and a new one is made: {why}",
                old.display()
            ));
        }
    }
    Some(match safe_fs::delete_open(&file) {
        Ok(()) => format!(
            "moved the pstore disk {} to {}",
            old.display(),
            new.display()
        ),
        Err(why) => format!("the earlier pstore disk {} stays: {why}", old.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::addon_disk::Addon;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsa-machine-user-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn erofs_image(name: &str, data: &[u8]) -> Vec<u8> {
        let mut tree = dsa_host_core::erofs::Builder::new();
        tree.file(name, 0o644, data.to_vec()).unwrap();
        let mut out = Vec::new();
        tree.write(&mut out).unwrap();
        out
    }

    #[test]
    fn fr32_machine_user_folder_sddl_lets_only_system_and_administrators_write() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            folder_sddl(sid),
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;S-1-5-21-1-2-3-1001)\
             (A;OICI;0x1200a9;;;S-1-5-83-0)"
        );
        let dir = scratch("sddl");
        let machine = MachineRoot::at(dir.join("ProgramData").join("DSA"));
        assert!(prepare(&machine, "not-a-sid").is_err());
        let me = crate::pipe::current_user_sid().unwrap();
        let user_dir = prepare(&machine, &me).unwrap();
        assert_eq!(user_dir, machine.user_dir(&me));
        assert!(user_dir.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_migrate_disks_moves_valid_disks_and_refuses_links_or_invalid_ones() {
        let dir = scratch("migrate");
        let sid = crate::pipe::current_user_sid().unwrap();
        let machine = MachineRoot::at(dir.join("ProgramData").join("DSA"));
        let user = UserRoot::in_local_app_data(&dir.join("Local"));
        fs::create_dir_all(user.dir()).unwrap();

        let old_addon = AddonDisk::new(user.old_addon_disk(), Format::Vhd);
        let gapps = erofs_image("a.apk", b"gapps");
        let translator = erofs_image("b.so", b"translator");
        old_addon
            .install(Addon::Gapps, gapps.len() as u64, None, &mut &gapps[..])
            .unwrap();
        assert_eq!(old_addon.prepare_for_boot(), [Addon::Gapps]);
        old_addon
            .install(
                Addon::ArmTranslation,
                translator.len() as u64,
                None,
                &mut &translator[..],
            )
            .unwrap();
        fs::write(beside(&user.old_addon_disk(), ".part"), b"half").unwrap();
        crate::pstore_disk::ensure(&user.old_pstore_disk()).unwrap();

        let notes = migrate_disks(&machine, &user, &sid);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(!user.old_addon_disk().exists());
        assert!(!old_addon.pending_path().exists());
        assert!(!beside(&user.old_addon_disk(), ".part").exists());
        assert!(!user.old_pstore_disk().exists());
        assert!(machine.user_pstore_disk(&sid).is_file());

        let new_addon = AddonDisk::new(machine.user_addon_disk(&sid), Format::Vhd);
        assert_eq!(new_addon.attached(), [Addon::Gapps]);
        assert_eq!(
            new_addon.prepare_for_boot(),
            [Addon::Gapps, Addon::ArmTranslation]
        );

        // An invalid or hard-linked file in the user's folder is refused and never replaces
        // what is in the machine's folder.
        let other = dir.join("other");
        let machine_two = MachineRoot::at(other.join("ProgramData").join("DSA"));
        let user_two = UserRoot::in_local_app_data(&other.join("Local"));
        fs::create_dir_all(user_two.dir()).unwrap();
        fs::write(user_two.old_addon_disk(), vec![0u8; 4096]).unwrap();
        let notes = migrate_disks(&machine_two, &user_two, &sid);
        assert!(notes.iter().any(|n| n.contains("not moved")), "{notes:?}");
        assert!(!machine_two.user_addon_disk(&sid).exists());
        assert!(user_two.old_addon_disk().exists());

        let linked = user_two.dir().join("second.vhd");
        fs::hard_link(user_two.old_addon_disk(), &linked).unwrap();
        let notes = migrate_disks(&machine_two, &user_two, &sid);
        assert!(notes.iter().any(|n| n.contains("hard link")), "{notes:?}");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&other);
    }
}

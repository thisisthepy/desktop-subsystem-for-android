//! The user's backup folder (`~/DSA Backup`) on a Unix host is kept to its owner alone: the
//! files in it are encrypted, but even the folder's listing is nobody else's business.

/// The mode the backup folder is kept at.
pub const FOLDER_MODE: u32 = 0o700;

/// The mode a folder of mode `mode` is given, if it is not that already.
pub fn private_mode(mode: u32) -> Option<u32> {
    (mode & 0o7777 != FOLDER_MODE).then_some(FOLDER_MODE)
}

/// Keeps `folder` to its owner alone, when it is a folder (never through a link) and is not
/// already. Run with the owner's rights.
#[cfg(unix)]
pub fn keep_private(folder: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::symlink_metadata(folder) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    if let Some(mode) = private_mode(metadata.permissions().mode()) {
        if let Err(error) = std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode))
        {
            eprintln!(
                "backup: {} cannot be kept to its owner alone: {error}",
                folder.display()
            );
        }
    }
}

/// A backup folder's [`dsa_host_core::backup::FileAccess`] for a service that runs as the
/// user: the process's own rights, and the folder kept private after each use.
#[cfg(unix)]
pub fn own_rights(folder: std::path::PathBuf) -> dsa_host_core::backup::FileAccess {
    std::sync::Arc::new(move |run: &mut dyn FnMut()| {
        run();
        keep_private(&folder);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_the_backup_folder_is_kept_to_its_owner_alone() {
        assert_eq!(private_mode(0o40755), Some(0o700));
        assert_eq!(private_mode(0o40700), None);
        assert_eq!(
            private_mode(0o41700),
            Some(0o700),
            "no sticky or set-ID bits stay"
        );
    }
}

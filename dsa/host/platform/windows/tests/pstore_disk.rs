//! Where the pstore disk is kept on Windows. The disk itself is made by the host core.

use std::path::{Path, PathBuf};

use dsa_host_windows::paths::{pstore_disk_beside, MachineRoot, UserRoot};

#[test]
fn fr19_the_pstore_disk_is_in_the_machines_user_folder_and_uninstall_removes_the_old_one() {
    let machine = MachineRoot::at(r"C:\ProgramData\DSA");
    assert_eq!(
        machine.user_pstore_disk("S-1-5-21-1-2-3-1001"),
        PathBuf::from(r"C:\ProgramData\DSA\users\S-1-5-21-1-2-3-1001\pstore.vhd")
    );
    let root = UserRoot::from_dir(Path::new(r"C:\Users\me\AppData\Local\DSA"));
    assert_eq!(
        root.old_pstore_disk(),
        PathBuf::from(r"C:\Users\me\AppData\Local\DSA\pstore.vhd")
    );
    assert_eq!(
        pstore_disk_beside(&root.android_disk()),
        root.old_pstore_disk()
    );
    assert!(!UserRoot::kept_on_uninstall("pstore.vhd"));
}

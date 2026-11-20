//! Which apps the daemon names to see the computer's memory as the device's.

use dsa_guest_channeld::host_memory::{
    property_value, property_value_for, HostMemoryApps, APPS, NONE, PROPERTY, REAL_SIZE_APPS,
    REAL_SIZE_PROPERTY,
};
use dsa_guest_channeld::su_relay::uids_from_packages_list;

const PACKAGES: &str =
    "com.android.settings 1000 0 /data/user/0/com.android.settings platform none\n\
com.android.vending 10123 0 /data/user/0/com.android.vending default:privapp 3003\n\
com.google.android.gms 10087 0 /data/user/0/com.google.android.gms default:privapp 3003\n\
com.android.chrome 10150 0 /data/user/0/com.android.chrome default 3003\n";

#[test]
fn fr32_only_the_play_store_and_play_services_are_named() {
    assert_eq!(APPS, ["com.android.vending", "com.google.android.gms"]);
    let value = property_value(&uids_from_packages_list(PACKAGES));
    assert_eq!(value, "10087,10123");
    // Chrome and Settings are not named, so they keep seeing the VM's memory.
    assert!(!value.contains("10150"));
    assert!(!value.contains("1000,"));
}

#[test]
fn fr32_without_google_play_no_app_is_named() {
    let value = property_value(&uids_from_packages_list(
        "com.android.settings 1000 0 /data/user/0/com.android.settings platform none\n",
    ));
    assert_eq!(value, NONE);
}

#[test]
fn fr32_the_name_is_the_app_id_for_every_android_user() {
    // A second Android user's copy of Play services has uid 1010087.
    let value = property_value(&uids_from_packages_list(
        "com.google.android.gms 1010087 0 /data/user/10/com.google.android.gms default 3003\n",
    ));
    assert_eq!(value, "10087");
}

#[test]
fn fr32_the_property_is_set_again_only_when_google_play_comes_or_goes() {
    let mut apps = HostMemoryApps::new();
    // Before boot the list cannot be read: nothing is set.
    assert_eq!(apps.look(None), None);
    let first = apps.look(Some(PACKAGES)).unwrap();
    assert_eq!(first, "10087,10123");
    apps.set(first);
    assert_eq!(apps.look(Some(PACKAGES)), None);
    // Google Play removed.
    let removed = apps
        .look(Some("com.android.settings 1000 0 /x platform none\n"))
        .unwrap();
    assert_eq!(removed, NONE);
    apps.set(removed);
    assert_eq!(
        apps.look(Some("com.android.settings 1000 0 /x platform none\n")),
        None
    );
}

#[test]
fn fr32_the_property_is_one_init_reads_for_the_kernel() {
    assert_eq!(PROPERTY, "vendor.dsa.host_memory.uids");
    let rc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../image/dsa-memory.rc"),
    )
    .unwrap();
    assert!(rc.contains(&format!("on property:{PROPERTY}=*")));
    assert!(rc.contains(&format!(
        "write /sys/module/dsa_host_memory/parameters/uids ${{{PROPERTY}}}"
    )));
}

#[test]
fn fr32_the_agent_is_named_to_see_the_vms_own_memory() {
    // The agent reports the guest's memory to the host, so it must read the VM's real size,
    // not the memory the VM may grow to that every other app sees.
    assert_eq!(REAL_SIZE_APPS, ["io.github.thisisthepy.dsa.agent"]);
    let packages = format!(
        "{PACKAGES}io.github.thisisthepy.dsa.agent 10042 0 /data/user/0/io.github.thisisthepy.dsa.agent platform:privapp 3003\n"
    );
    let uids = uids_from_packages_list(&packages);
    assert_eq!(property_value_for(&REAL_SIZE_APPS, &uids), "10042");
    // Google Play is not on that list, and the agent is not on Google Play's.
    assert_eq!(property_value(&uids), "10087,10123");
    let mut agent = HostMemoryApps::for_apps(&REAL_SIZE_APPS);
    assert_eq!(agent.look(Some(PACKAGES)).as_deref(), Some(NONE));
    assert_eq!(agent.look(Some(&packages)).as_deref(), Some("10042"));
}

#[test]
fn fr32_the_agents_property_is_one_init_reads_for_the_kernel() {
    assert_eq!(REAL_SIZE_PROPERTY, "vendor.dsa.host_memory.real_uids");
    let rc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../image/dsa-memory.rc"),
    )
    .unwrap();
    assert!(rc.contains(&format!("on property:{REAL_SIZE_PROPERTY}=*")));
    assert!(rc.contains(&format!(
        "write /sys/module/dsa_host_memory/parameters/real_uids ${{{REAL_SIZE_PROPERTY}}}"
    )));
}

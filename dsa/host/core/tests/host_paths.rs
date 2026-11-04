//! The storage port: every place is below one of the two roots, and a user's places are
//! below that user's root.

use std::path::PathBuf;

use dsa_host_core::host_paths::{FakeHostPaths, HostPaths};

fn fake() -> FakeHostPaths {
    FakeHostPaths::new(PathBuf::from("machine"), PathBuf::from("users"))
}

#[test]
fn fr19_machine_places_are_below_the_machine_root() {
    let paths = fake();
    let root = paths.machine_data();
    for place in [
        paths.images(),
        paths.updates(),
        paths.settings(),
        paths.machine_logs(),
        paths.machine_tmp(),
    ] {
        assert_eq!(place.parent(), Some(root.as_path()), "{}", place.display());
    }
}

#[test]
fn fr19_user_places_are_below_that_users_root() {
    let paths = fake();
    let root = paths.user_data("alice").unwrap();
    assert_eq!(root, PathBuf::from("users").join("alice"));
    for place in [
        paths.apps("alice").unwrap(),
        paths.user_logs("alice").unwrap(),
        paths.user_tmp("alice").unwrap(),
    ] {
        assert_eq!(place.parent(), Some(root.as_path()), "{}", place.display());
    }
    assert_ne!(paths.user_data("bob").unwrap(), root);
}

#[test]
fn fr19_a_user_name_that_is_a_path_is_refused() {
    let paths = fake();
    for user in ["", ".", "..", "a/b", "a\\b"] {
        assert!(paths.user_data(user).is_err(), "{user:?}");
    }
}

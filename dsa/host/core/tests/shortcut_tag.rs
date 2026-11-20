//! The short name a pinned shortcut's files carry, the same on every host.

use dsa_host_core::apps::shortcut_tag;

#[test]
fn fr23_a_shortcut_tag_is_the_fnv_1a_hash_in_sixteen_digits() {
    // The published FNV-1a 64-bit test vectors.
    assert_eq!(shortcut_tag(""), "cbf29ce484222325");
    assert_eq!(shortcut_tag("a"), "af63dc4c8601ec8c");
    assert_ne!(shortcut_tag("ana"), shortcut_tag("bob"));
}

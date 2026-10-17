use dsa_protocol_schema::{normalize, schema_hash};

const BASE: &str = r#"
syntax = "proto3";
// A comment.
message Hello {
  uint32 protocol_version = 1; /* trailing */
  string build = 4;
}
"#;

#[test]
fn pr2_comments_and_layout_do_not_change_the_schema_hash() {
    let reformatted = "syntax=\"proto3\";message Hello{uint32 protocol_version=1;\n\n// other words\nstring build=4;}";
    assert_eq!(schema_hash(BASE), schema_hash(reformatted));
}

#[test]
fn pr2_any_change_to_the_definition_changes_the_schema_hash() {
    for changed in [
        BASE.replace("= 4", "= 5"),
        BASE.replace("build", "builds"),
        BASE.replace("uint32", "uint64"),
        BASE.replace("}\n", "  bytes extra = 9;\n}\n"),
    ] {
        assert_ne!(schema_hash(BASE), schema_hash(&changed), "{changed}");
    }
}

#[test]
fn pr2_a_comment_marker_inside_a_string_is_not_a_comment() {
    let a = r#"option x = "a//b"; message M {}"#;
    let b = r#"option x = "a//c"; message M {}"#;
    assert_ne!(schema_hash(a), schema_hash(b));
    assert_eq!(normalize(a), r#"option x = "a//b" ; message M { }"#);
}

//! The words DSA shows or says in its own interface that every host shares, one table for
//! each part, in each of the languages the interface is shown in ([`crate::ui_language`]).
//!
//! A host keeps only the words its operating system owns (a menu item the desktop names
//! itself, a shortcut key written the way that system writes it) and takes the rest from
//! here, so a translation is corrected in one place.

pub mod caption;
pub mod pin_shortcut;
pub mod root_approval;
pub mod window_menu;

/// `template` with each `{name}` among `values` replaced by its value, in one pass, so a value
/// that holds braces itself (a command such as `echo {who}`) is shown as it is.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').and_then(|close| {
            let name = &after[..close];
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value, close))
        });
        match value {
            Some((value, close)) => {
                out.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn fr17_placeholders_are_filled_once() {
        assert_eq!(
            fill(
                "{who} runs {command}",
                &[("who", "{command}"), ("command", "id")]
            ),
            "{command} runs id"
        );
        assert_eq!(fill("{other} {x", &[("x", "1")]), "{other} {x");
        assert_eq!(fill("{app} {name}", &[("app", "{name}")]), "{name} {name}");
        assert_eq!(fill("{other}", &[("app", "x")]), "{other}");
    }
}

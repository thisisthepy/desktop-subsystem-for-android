//! Where two versions of a text differ, as an editor tells the other side what a person typed.
//!
//! The text is a slice of units: characters on a host that counts characters, UTF-16 code
//! units on one that counts those. The unit decides which neighbours must not be split.

/// Where `old` and `new` differ: the start of the change, then where the change ends in `old`
/// and in `new`, after the start and the end both share. `None` when they are the same.
///
/// `high_half` and `low_half` say whether a unit is the first or the second half of a pair
/// that is one character (the surrogates of UTF-16); the change never splits such a pair, so
/// what is sent is always whole characters. A unit that has no halves answers false.
pub fn difference<T: PartialEq>(
    old: &[T],
    new: &[T],
    high_half: impl Fn(&T) -> bool,
    low_half: impl Fn(&T) -> bool,
) -> Option<(usize, usize, usize)> {
    if old == new {
        return None;
    }
    let mut prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix > 0 && high_half(&old[prefix - 1]) {
        prefix -= 1;
    }
    let most = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(most)
        .take_while(|(a, b)| a == b)
        .count();
    if suffix > 0 && low_half(&old[old.len() - suffix]) {
        suffix -= 1;
    }
    Some((prefix, old.len() - suffix, new.len() - suffix))
}

/// Whether a UTF-16 code unit is the first half of a surrogate pair.
pub fn is_high_surrogate(unit: &u16) -> bool {
    (0xd800..=0xdbff).contains(unit)
}

/// Whether a UTF-16 code unit is the second half of a surrogate pair.
pub fn is_low_surrogate(unit: &u16) -> bool {
    (0xdc00..=0xdfff).contains(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn fr17_the_change_is_what_lies_between_the_shared_start_and_end() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        let none = |_: &char| false;
        assert_eq!(difference(&chars("abc"), &chars("abc"), none, none), None);
        assert_eq!(
            difference(
                &chars("hello world"),
                &chars("hello brave world"),
                none,
                none
            ),
            Some((6, 6, 12))
        );
        assert_eq!(
            difference(&chars("abcd"), &chars("ad"), none, none),
            Some((1, 3, 1))
        );
        assert_eq!(
            difference(&chars("aa"), &chars("a"), none, none),
            Some((1, 2, 1))
        );
    }

    #[test]
    fn fr17_a_change_never_splits_a_surrogate_pair() {
        // U+1F600 and U+1F601 share their first half.
        let (old, new) = (utf16("a\u{1F600}"), utf16("a\u{1F601}"));
        assert_eq!(
            difference(&old, &new, is_high_surrogate, is_low_surrogate),
            Some((1, 3, 3))
        );
        // They share their second half only when the first differs.
        let (old, new) = (utf16("\u{1F600}z"), utf16("\u{1F640}z"));
        let change = difference(&old, &new, is_high_surrogate, is_low_surrogate);
        assert_eq!(change, Some((0, 2, 2)));
    }
}

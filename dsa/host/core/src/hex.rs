//! Bytes as lower-case hexadecimal text, the form hashes and keys are written in.

/// `bytes` as lower-case hexadecimal, two digits each.
pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes `text` writes as hexadecimal, two digits each in either case. `None` when the text
/// has an odd length or a character that is not a hexadecimal digit.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn d9_hex_text_is_read_back_as_the_bytes_it_was_written_from() {
        assert_eq!(decode("000fa5FF"), Some(vec![0x00, 0x0f, 0xa5, 0xff]));
        assert_eq!(decode(""), Some(vec![]));
        assert_eq!(decode(&encode(&[1, 2, 254])), Some(vec![1, 2, 254]));
    }

    #[test]
    fn d9_hex_text_of_odd_length_or_with_other_characters_is_refused() {
        assert_eq!(decode("abc"), None);
        assert_eq!(decode("zz"), None);
        // A multi-byte character must not split a slice.
        assert_eq!(decode("aé"), None);
    }

    #[test]
    fn d9_bytes_are_written_as_two_lower_case_digits_each() {
        assert_eq!(encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(encode(&[]), "");
    }
}

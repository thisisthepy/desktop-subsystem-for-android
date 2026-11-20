//! Percent-encoded text, as it comes in a `file://` link and in a notification's image path.

/// The bytes `text` stands for once each `%XX` is replaced by the byte it names. `None` when a
/// `%` is not followed by two hexadecimal digits.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    Some(decoded)
}

/// Like [`decode`], and `None` too when the bytes are not UTF-8.
pub fn decode_text(text: &str) -> Option<String> {
    String::from_utf8(decode(text)?).ok()
}

#[cfg(test)]
mod tests {
    use super::{decode, decode_text};

    #[test]
    fn d9_percent_escapes_become_the_bytes_they_name() {
        assert_eq!(decode("a%20b%2Fc"), Some(b"a b/c".to_vec()));
        assert_eq!(decode("plain"), Some(b"plain".to_vec()));
        assert_eq!(decode("%ff"), Some(vec![0xff]));
    }

    #[test]
    fn d9_a_percent_without_two_digits_is_refused() {
        assert_eq!(decode("100%"), None);
        assert_eq!(decode("%4"), None);
        assert_eq!(decode("%zz"), None);
    }

    #[test]
    fn d9_decoded_text_must_be_utf8() {
        assert_eq!(decode_text("caf%C3%A9"), Some("café".to_string()));
        assert_eq!(decode_text("%ff"), None);
    }
}

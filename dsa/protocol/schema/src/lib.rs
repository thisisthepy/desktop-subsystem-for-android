//! The schema hash of the control protocol definition.
//!
//! Both ends of the control channel send the hash of the definition they were generated
//! from, and the host refuses a peer whose hash differs. The hash covers the definition's
//! tokens only: comments, blank lines and spacing do not change it, while any change to a
//! name, a number, a type or an option does.
//!
//! The generator writes the hash into every language's generated code, so this module is
//! the only place it is computed.

use sha2::{Digest, Sha256};

/// The definition's tokens, joined by single spaces.
///
/// Comments (`//` to the end of the line, and `/* ... */`) and whitespace are dropped.
/// String literals are kept whole, so a `//` inside one is not a comment.
pub fn normalize(definition: &str) -> String {
    let chars: Vec<char> = definition.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            tokens.push(chars[start..i.min(chars.len())].iter().collect());
        } else if c.is_alphanumeric() || c == '_' || c == '.' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        } else {
            tokens.push(c.to_string());
            i += 1;
        }
    }
    tokens.join(" ")
}

/// SHA-256 of [`normalize`]`(definition)`.
pub fn schema_hash(definition: &str) -> [u8; 32] {
    Sha256::digest(normalize(definition).as_bytes()).into()
}

/// Lower-case hexadecimal, for messages and generated files.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

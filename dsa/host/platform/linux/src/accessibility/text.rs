//! A node's text as AT-SPI's Text interface reads it: by character, word, sentence or line,
//! with offsets counted in characters, and what changed between two texts.

/// What a piece of text is read by: AT-SPI's `AtspiTextGranularity`, and the older
/// `AtspiTextBoundaryType` mapped onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Char,
    Word,
    Sentence,
    Line,
}

impl Unit {
    /// `GetStringAtOffset`'s granularity: character, word, sentence, line, paragraph. A node's
    /// text has no paragraphs apart from its lines.
    pub fn from_granularity(granularity: u32) -> Unit {
        match granularity {
            0 => Unit::Char,
            1 => Unit::Word,
            2 => Unit::Sentence,
            _ => Unit::Line,
        }
    }

    /// `GetTextAtOffset`'s boundary type: character, word start and end, sentence start and
    /// end, line start and end.
    pub fn from_boundary(boundary: u32) -> Unit {
        match boundary {
            0 => Unit::Char,
            1 | 2 => Unit::Word,
            3 | 4 => Unit::Sentence,
            _ => Unit::Line,
        }
    }
}

/// The number of characters in `text`.
pub fn length(text: &str) -> i32 {
    text.chars().count().min(i32::MAX as usize) as i32
}

/// The characters from `start` to `end` (`end` of -1 is the end), cut to the text.
pub fn slice(text: &str, start: i32, end: i32) -> String {
    let len = length(text);
    let end = if end < 0 || end > len { len } else { end };
    let start = start.clamp(0, end);
    text.chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect()
}

/// The character at `offset` as a Unicode scalar, or 0 past the text.
pub fn character_at(text: &str, offset: i32) -> i32 {
    if offset < 0 {
        return 0;
    }
    text.chars()
        .nth(offset as usize)
        .map(|c| c as i32)
        .unwrap_or(0)
}

fn ends_sentence(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}')
}

/// The unit of `text` around `offset`: its text and its start and end offsets. An offset past
/// the text gives an empty piece at the end.
pub fn unit_at(text: &str, offset: i32, unit: Unit) -> (String, i32, i32) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i32;
    if offset < 0 || offset >= len {
        let at = offset.clamp(0, len);
        return (String::new(), at, at);
    }
    let at = offset as usize;
    let (start, end) = match unit {
        Unit::Char => (at, at + 1),
        Unit::Word => {
            // A word with the spaces after it, as AT-SPI's word start boundary reads it: a
            // space belongs to the word before it, and spaces that start the text to the word
            // after them.
            let mut start = at;
            while start > 0 && chars[start].is_whitespace() {
                start -= 1;
            }
            if !chars[start].is_whitespace() {
                while start > 0 && !chars[start - 1].is_whitespace() {
                    start -= 1;
                }
            }
            let mut end = start;
            while end < chars.len() && chars[end].is_whitespace() {
                end += 1;
            }
            while end < chars.len() && !chars[end].is_whitespace() {
                end += 1;
            }
            while end < chars.len() && chars[end].is_whitespace() && chars[end] != '\n' {
                end += 1;
            }
            (start, end)
        }
        Unit::Sentence => {
            let mut start = at;
            while start > 0 && !(ends_sentence(chars[start - 1]) || chars[start - 1] == '\n') {
                start -= 1;
            }
            while start < at && chars[start].is_whitespace() {
                start += 1;
            }
            let mut end = at;
            while end < chars.len() && !ends_sentence(chars[end]) && chars[end] != '\n' {
                end += 1;
            }
            if end < chars.len() {
                end += 1;
            }
            while end < chars.len() && chars[end].is_whitespace() && chars[end] != '\n' {
                end += 1;
            }
            (start, end)
        }
        Unit::Line => {
            let mut start = at;
            while start > 0 && chars[start - 1] != '\n' {
                start -= 1;
            }
            let mut end = at;
            while end < chars.len() && chars[end] != '\n' {
                end += 1;
            }
            if end < chars.len() {
                end += 1;
            }
            (start, end)
        }
    };
    (chars[start..end].iter().collect(), start as i32, end as i32)
}

/// The unit before the one at `offset`.
pub fn unit_before(text: &str, offset: i32, unit: Unit) -> (String, i32, i32) {
    let (_, start, _) = unit_at(text, offset.min(length(text) - 1).max(0), unit);
    if start == 0 || offset <= 0 {
        return (String::new(), 0, 0);
    }
    unit_at(text, start - 1, unit)
}

/// The unit after the one at `offset`.
pub fn unit_after(text: &str, offset: i32, unit: Unit) -> (String, i32, i32) {
    let (_, _, end) = unit_at(text, offset, unit);
    unit_at(text, end, unit)
}

/// What changed from `old` to `new`: where the change starts, the characters taken out and
/// the characters put in, after the start and end both texts share. `None` when they are the
/// same.
pub fn difference(old: &str, new: &str) -> Option<(i32, String, String)> {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let (prefix, old_end, new_end) =
        dsa_host_core::text_edit::difference(&a, &b, |_| false, |_| false)?;
    Some((
        prefix as i32,
        a[prefix..old_end].iter().collect(),
        b[prefix..new_end].iter().collect(),
    ))
}

/// `text` with `inserted` put in at `position`.
pub fn insert(text: &str, position: i32, inserted: &str) -> String {
    let len = length(text);
    let at = position.clamp(0, len);
    format!("{}{inserted}{}", slice(text, 0, at), slice(text, at, len))
}

/// `text` without the characters from `start` to `end`.
pub fn delete(text: &str, start: i32, end: i32) -> String {
    let len = length(text);
    let end = if end < 0 || end > len { len } else { end };
    let start = start.clamp(0, end);
    format!("{}{}", slice(text, 0, start), slice(text, end, len))
}

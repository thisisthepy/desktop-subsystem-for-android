//! What every platform's speech and spelling needs alike: text cut into the pieces a voice
//! speaks one at a time, words and where they are in UTF-16 code units, the sound of a WAV
//! file, and the voice for a language.

use super::{Pcm, Voice};

/// A piece of spoken text is cut here or sooner, at a space, when no sentence ends before.
const MAX_PIECE_CHARS: usize = 400;
/// Sentences shorter than this go with the next one, so a voice does not pause after each.
const MIN_PIECE_CHARS: usize = 40;

/// How many UTF-16 code units `text` takes, as Android and the host's spell checkers count.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn ends_sentence(c: char) -> bool {
    matches!(
        c,
        '.' | '!' | '?' | '\n' | '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | '\u{2026}' | ';'
    )
}

/// Cuts `text` into the pieces the host speaks one after the other: whole sentences, short
/// ones together, long ones at a space. The first piece's sound reaches the guest while the
/// host still speaks the rest, and a stop takes effect between pieces. Pieces of nothing but
/// spaces are left out.
pub fn speech_pieces(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        count += 1;
        let at_end = ends_sentence(c)
            && chars
                .peek()
                .map(|next| next.is_whitespace() || !next.is_alphanumeric())
                .unwrap_or(true);
        if at_end {
            sentences.push(std::mem::take(&mut current));
            count = 0;
        } else if count >= MAX_PIECE_CHARS {
            // No sentence ends in time: cut at the last space, or here when there is none.
            let cut = current
                .char_indices()
                .filter(|(_, c)| c.is_whitespace())
                .map(|(i, _)| i)
                .next_back()
                .filter(|&i| i > 0);
            match cut {
                Some(at) => {
                    let rest = current.split_off(at);
                    sentences.push(std::mem::replace(&mut current, rest));
                }
                None => sentences.push(std::mem::take(&mut current)),
            }
            count = current.chars().count();
        }
    }
    if !current.is_empty() {
        sentences.push(current);
    }
    let mut pieces: Vec<String> = Vec::new();
    for sentence in sentences {
        if sentence.trim().is_empty() {
            if let Some(last) = pieces.last_mut() {
                last.push_str(&sentence);
            }
            continue;
        }
        match pieces.last_mut() {
            Some(last)
                if last.trim().chars().count() < MIN_PIECE_CHARS
                    && last.chars().count() + sentence.chars().count() <= MAX_PIECE_CHARS =>
            {
                last.push_str(&sentence)
            }
            _ => pieces.push(sentence),
        }
    }
    pieces
        .into_iter()
        .map(|piece| piece.trim().to_string())
        .filter(|piece| !piece.is_empty())
        .collect()
}

/// A word of a text, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word<'a> {
    pub text: &'a str,
    /// In UTF-16 code units from the start of the text.
    pub start: u32,
    pub length: u32,
}

fn joins_word(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}' | '-')
}

/// The words of `text` a spell checker that checks one word at a time looks at: runs of
/// letters (and the marks that go with them), with an apostrophe or a hyphen between letters
/// kept inside. A run with a digit in it is no word to check, and neither is one letter.
pub fn words(text: &str) -> Vec<Word<'_>> {
    let mut found = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut units = 0u32;
    let mut i = 0usize;
    while i < chars.len() {
        let (byte, c) = chars[i];
        if !(c.is_alphabetic() || c.is_numeric()) {
            units += c.len_utf16() as u32;
            i += 1;
            continue;
        }
        let start_byte = byte;
        let start_units = units;
        let mut digit = false;
        let mut letters = 0usize;
        let mut end = i;
        while end < chars.len() {
            let c = chars[end].1;
            if c.is_alphabetic() || is_mark(c) {
                letters += 1;
            } else if c.is_numeric() {
                digit = true;
            } else if joins_word(c)
                && end + 1 < chars.len()
                && chars[end + 1].1.is_alphabetic()
                && end > i
            {
                // Kept inside the word.
            } else {
                break;
            }
            units += c.len_utf16() as u32;
            end += 1;
        }
        let end_byte = chars.get(end).map(|(b, _)| *b).unwrap_or(text.len());
        if !digit && letters > 1 {
            found.push(Word {
                text: &text[start_byte..end_byte],
                start: start_units,
                length: units - start_units,
            });
        }
        i = end;
    }
    found
}

/// A combining mark, which belongs to the letter before it.
fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// The sound of a WAV file: its 16-bit PCM samples and their format. Anything else (another
/// sample format, a file cut short) is an error.
pub fn wav_pcm(file: &[u8]) -> Result<Pcm, String> {
    if file.len() < 12 || &file[0..4] != b"RIFF" || &file[8..12] != b"WAVE" {
        return Err("the speech service's sound is not a WAV file".into());
    }
    let mut at = 12usize;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    while at + 8 <= file.len() {
        let id = &file[at..at + 4];
        let size =
            u32::from_le_bytes([file[at + 4], file[at + 5], file[at + 6], file[at + 7]]) as usize;
        let body = at + 8;
        let end = body.saturating_add(size).min(file.len());
        if id == b"fmt " {
            if end - body < 16 {
                return Err("the WAV file's format is cut short".into());
            }
            let f = &file[body..end];
            let mut tag = u16::from_le_bytes([f[0], f[1]]);
            let channels = u16::from_le_bytes([f[2], f[3]]);
            let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            let bits = u16::from_le_bytes([f[14], f[15]]);
            // WAVE_FORMAT_EXTENSIBLE names the real format in its sub-format GUID.
            if tag == 0xFFFE && f.len() >= 26 {
                tag = u16::from_le_bytes([f[24], f[25]]);
            }
            format = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            let Some((tag, channels, rate, bits)) = format else {
                return Err("the WAV file's sound comes before its format".into());
            };
            if tag != 1 || bits != 16 {
                return Err(format!(
                    "the speech service's sound is format {tag} at {bits} bits, not 16-bit PCM"
                ));
            }
            if channels == 0 || rate == 0 {
                return Err("the WAV file names no channels or rate".into());
            }
            let frame = usize::from(channels) * 2;
            let whole = (end - body) / frame * frame;
            return Ok(Pcm {
                sample_rate_hz: rate,
                channels: u32::from(channels),
                data: file[body..body + whole].to_vec(),
            });
        }
        // Chunks are padded to an even size.
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    Err("the WAV file has no sound".into())
}

/// `tag` in one spelling: hyphens, and lower case but for the region.
pub fn normalize_tag(tag: &str) -> String {
    tag.trim()
        .replace('_', "-")
        .split('-')
        .filter(|part| !part.is_empty())
        .enumerate()
        .map(|(i, part)| {
            if i > 0 && part.len() == 2 {
                part.to_ascii_uppercase()
            } else {
                part.to_ascii_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// The language of a tag: "en" of "en-US".
pub fn primary_language(tag: &str) -> String {
    normalize_tag(tag)
        .split('-')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The voice to speak with: the one named, else the default when it speaks the language,
/// else the best one for the language (one for its very region first, natural ones before
/// the rest), else the default. `None` when there is none for the language.
pub fn pick_voice<'a>(
    voices: &'a [Voice],
    voice_id: &str,
    language_tag: &str,
    default_id: &str,
) -> Option<&'a Voice> {
    if !voice_id.is_empty() {
        if let Some(voice) = voices.iter().find(|v| v.id == voice_id) {
            return Some(voice);
        }
    }
    let default = voices.iter().find(|v| v.id == default_id);
    if language_tag.trim().is_empty() {
        return default.or_else(|| voices.first());
    }
    let wanted = normalize_tag(language_tag);
    let language = primary_language(language_tag);
    if let Some(default) = default {
        if normalize_tag(&default.language_tag) == wanted {
            return Some(default);
        }
    }
    let rank = |v: &Voice| {
        let exact = normalize_tag(&v.language_tag) == wanted;
        (!exact, !v.natural, v.needs_network)
    };
    voices
        .iter()
        .filter(|v| primary_language(&v.language_tag) == language)
        .min_by_key(|v| rank(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_text_is_spoken_a_sentence_at_a_time() {
        let long = "This is the first sentence of the text, which is long enough. \
                    And here is the second one, also long enough to stand alone! Short.";
        let pieces = speech_pieces(long);
        assert_eq!(pieces.len(), 3, "{pieces:?}");
        assert!(pieces[0].ends_with("enough."));
        assert_eq!(pieces[2], "Short.");
        assert_eq!(speech_pieces("  \n "), Vec::<String>::new());
        assert_eq!(speech_pieces("3.14 is pi"), vec!["3.14 is pi".to_string()]);
        let endless = "word ".repeat(200);
        let pieces = speech_pieces(&endless);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|p| p.chars().count() <= MAX_PIECE_CHARS));
    }

    #[test]
    fn fr36_words_are_found_with_their_utf16_offsets() {
        let found = words("Héllo, wörld's 2nd x 😀 well-known");
        let texts: Vec<_> = found.iter().map(|w| w.text).collect();
        assert_eq!(texts, ["Héllo", "wörld's", "well-known"]);
        assert_eq!((found[0].start, found[0].length), (0, 5));
        assert_eq!((found[1].start, found[1].length), (7, 7));
        // The emoji takes two UTF-16 code units.
        assert_eq!(found[2].start, 24);
    }

    fn wav(tag: u16, bits: u16, channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
        file.extend_from_slice(b"fmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&tag.to_le_bytes());
        file.extend_from_slice(&channels.to_le_bytes());
        file.extend_from_slice(&rate.to_le_bytes());
        file.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        file.extend_from_slice(&(channels * 2).to_le_bytes());
        file.extend_from_slice(&bits.to_le_bytes());
        file.extend_from_slice(b"LIST");
        file.extend_from_slice(&3u32.to_le_bytes());
        file.extend_from_slice(b"abc\0");
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(data.len() as u32).to_le_bytes());
        file.extend_from_slice(data);
        file
    }

    #[test]
    fn fr36_a_wav_files_pcm_is_read_past_other_chunks() {
        let pcm = wav_pcm(&wav(1, 16, 1, 22050, &[1, 2, 3, 4, 5])).unwrap();
        assert_eq!(pcm.sample_rate_hz, 22050);
        assert_eq!(pcm.channels, 1);
        assert_eq!(pcm.data, [1, 2, 3, 4], "a half sample is left out");
        assert!(wav_pcm(&wav(3, 32, 1, 22050, &[0; 8])).is_err());
        assert!(wav_pcm(b"not a wav").is_err());
    }

    fn voice(id: &str, tag: &str, natural: bool) -> Voice {
        Voice {
            id: id.into(),
            name: id.into(),
            language_tag: tag.into(),
            natural,
            needs_network: false,
        }
    }

    #[test]
    fn fr36_the_voice_for_a_language_is_picked_best_first() {
        let voices = [
            voice("david", "en-US", false),
            voice("aria", "en-US", true),
            voice("hazel", "en-GB", false),
            voice("heami", "ko-KR", false),
        ];
        let id = |v: Option<&Voice>| v.map(|v| v.id.clone());
        assert_eq!(
            id(pick_voice(&voices, "hazel", "ko-KR", "david")),
            Some("hazel".into())
        );
        assert_eq!(
            id(pick_voice(&voices, "", "en_us", "david")),
            Some("david".into())
        );
        assert_eq!(
            id(pick_voice(&voices, "", "en-US", "heami")),
            Some("aria".into())
        );
        assert_eq!(
            id(pick_voice(&voices, "", "en-AU", "heami")),
            Some("aria".into())
        );
        assert_eq!(
            id(pick_voice(&voices, "", "", "heami")),
            Some("heami".into())
        );
        assert_eq!(pick_voice(&voices, "", "fr-FR", "david"), None);
        assert_eq!(normalize_tag("en_us"), "en-US");
    }
}

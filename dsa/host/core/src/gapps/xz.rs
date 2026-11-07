//! A decoder for xz files whose blocks use the LZMA2 filter alone, as LiteGapps packs its
//! `files.tar.xz`: what `xz` makes by default, in one block or in several.
//!
//! It follows the xz file format (1.2.1) and the LZMA decoder of xz-embedded, and writes what
//! it decodes to a sink as it goes, keeping only the dictionary in memory. The integrity checks
//! inside the file are not verified: the whole download has been checked against the digest
//! its host publishes before this runs.

use std::io::Write;

const MAGIC: &[u8; 6] = b"\xFD7zXZ\0";
/// The filter ID of LZMA2.
const LZMA2: u64 = 0x21;
/// The largest dictionary taken. xz's highest preset uses 64 MiB.
const MAX_DICTIONARY: u64 = 256 << 20;

fn cut() -> String {
    "the xz data ends early".to_string()
}

/// Decodes the xz file `input` into `out`, refusing to write more than `limit` bytes, and
/// returns how many it wrote.
pub fn decode(input: &[u8], out: &mut dyn Write, limit: u64) -> Result<u64, String> {
    if input.len() < 12 || &input[..6] != MAGIC {
        return Err("the data is not an xz file".into());
    }
    if input[6] != 0 || input[7] & 0xF0 != 0 {
        return Err("the xz file uses stream flags this decoder does not know".into());
    }
    let check_size = match input[7] & 0x0F {
        0 => 0,
        1..=3 => 4,
        4..=6 => 8,
        7..=9 => 16,
        10..=12 => 32,
        _ => 64,
    };
    let mut at = 12;
    let mut written = 0u64;
    loop {
        let size_byte = *input.get(at).ok_or_else(cut)?;
        if size_byte == 0 {
            // The index: every block has been read.
            return Ok(written);
        }
        let block_start = at;
        let header_size = (usize::from(size_byte) + 1) * 4;
        let header = input.get(at..at + header_size).ok_or_else(cut)?;
        let flags = header[1];
        if flags & 0x3C != 0 {
            return Err("an xz block header has flags this decoder does not know".into());
        }
        let mut field = 2;
        let compressed = if flags & 0x40 != 0 {
            Some(varint(header, &mut field)?)
        } else {
            None
        };
        let uncompressed = if flags & 0x80 != 0 {
            Some(varint(header, &mut field)?)
        } else {
            None
        };
        if flags & 0x03 != 0 {
            return Err(
                "an xz block uses a filter besides LZMA2, which this decoder does not take".into(),
            );
        }
        let filter = varint(header, &mut field)?;
        let properties_size = varint(header, &mut field)?;
        if filter != LZMA2 || properties_size != 1 {
            return Err(format!(
                "an xz block uses filter {filter:#x}; only LZMA2 is taken"
            ));
        }
        let properties = *header.get(field).ok_or_else(cut)?;
        let dictionary = dictionary_size(properties)?;
        at += header_size;
        let size = match uncompressed {
            Some(size) => dictionary.min(size.max(1)),
            None => dictionary,
        };
        let mut dict = Dict::new(size as usize, out, limit - written);
        let used = lzma2(&input[at..], &mut dict)?;
        let produced = dict.written;
        if compressed.is_some_and(|size| size != used as u64)
            || uncompressed.is_some_and(|size| size != produced)
        {
            return Err("an xz block's sizes differ from what its header says".into());
        }
        written += produced;
        at += used;
        // Block padding to a multiple of four bytes, then the block's check.
        at += (4 - (at - block_start) % 4) % 4;
        at += check_size;
    }
}

/// Reads one of the header's variable length integers.
fn varint(data: &[u8], at: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *data.get(*at).ok_or_else(cut)?;
        *at += 1;
        value |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("an xz header has a number that is too long".into())
}

/// The dictionary size LZMA2's property byte names.
fn dictionary_size(properties: u8) -> Result<u64, String> {
    let bits = u64::from(properties & 0x3F);
    if bits > 40 {
        return Err("an xz block names a dictionary size that does not exist".into());
    }
    let size = if bits == 40 {
        u64::from(u32::MAX)
    } else {
        (2 | (bits & 1)) << (bits / 2 + 11)
    };
    if size > MAX_DICTIONARY {
        return Err(format!(
            "an xz block needs a {} MiB dictionary, more than the {} MiB taken",
            size >> 20,
            MAX_DICTIONARY >> 20
        ));
    }
    Ok(size)
}

/// The window of what has been decoded, which matches copy from, and which is written to the
/// sink as it fills.
struct Dict<'a> {
    buf: Vec<u8>,
    pos: usize,
    /// How many bytes of `buf` hold data.
    full: usize,
    /// What of `buf` before `pos` has gone to the sink.
    flushed: usize,
    /// How many bytes have been decoded since the dictionary was last reset.
    total: u64,
    out: &'a mut dyn Write,
    written: u64,
    limit: u64,
}

impl<'a> Dict<'a> {
    fn new(size: usize, out: &'a mut dyn Write, limit: u64) -> Dict<'a> {
        Dict {
            buf: vec![0u8; size.max(1)],
            pos: 0,
            full: 0,
            flushed: 0,
            total: 0,
            out,
            written: 0,
            limit,
        }
    }

    fn reset(&mut self) {
        self.full = 0;
        self.total = 0;
    }

    fn put(&mut self, byte: u8) -> Result<(), String> {
        if self.pos == self.buf.len() {
            self.flush()?;
            self.pos = 0;
            self.flushed = 0;
        }
        self.buf[self.pos] = byte;
        self.pos += 1;
        if self.full < self.buf.len() {
            self.full += 1;
        }
        self.total += 1;
        Ok(())
    }

    /// The byte `distance + 1` bytes back.
    fn get(&self, distance: u32) -> Result<u8, String> {
        let distance = distance as usize;
        if distance >= self.full {
            return Err("the xz data refers to data before its start".into());
        }
        let at = if distance < self.pos {
            self.pos - distance - 1
        } else {
            self.buf.len() + self.pos - distance - 1
        };
        Ok(self.buf[at])
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.flushed < self.pos {
            let length = (self.pos - self.flushed) as u64;
            if self.written + length > self.limit {
                return Err(format!(
                    "the xz data holds more than the {} MiB taken",
                    self.limit >> 20
                ));
            }
            self.out
                .write_all(&self.buf[self.flushed..self.pos])
                .map_err(|e| e.to_string())?;
            self.written += length;
            self.flushed = self.pos;
        }
        Ok(())
    }
}

/// Decodes one block's LZMA2 data, which ends with its end marker, into `dict`, and returns
/// how many bytes of `input` it took.
fn lzma2(input: &[u8], dict: &mut Dict<'_>) -> Result<usize, String> {
    let mut lzma = Lzma::new();
    let mut at = 0;
    let mut need_dictionary_reset = true;
    let mut need_properties = true;
    loop {
        let control = *input.get(at).ok_or_else(cut)?;
        at += 1;
        if control == 0 {
            dict.flush()?;
            return Ok(at);
        }
        let two = |at: usize| -> Result<usize, String> {
            input
                .get(at..at + 2)
                .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
                .ok_or_else(cut)
        };
        if control == 1 || control == 2 {
            // An uncompressed chunk.
            if control == 1 {
                dict.reset();
                need_dictionary_reset = false;
            } else if need_dictionary_reset {
                return Err("the xz data does not start with a dictionary reset".into());
            }
            let size = two(at)? + 1;
            at += 2;
            let data = input.get(at..at + size).ok_or_else(cut)?;
            for &byte in data {
                dict.put(byte)?;
            }
            at += size;
            continue;
        }
        if control < 0x80 {
            return Err("the xz data is not LZMA2".into());
        }
        let unpacked = (usize::from(control & 0x1F) << 16) + two(at)? + 1;
        let packed = two(at + 2)? + 1;
        at += 4;
        let reset = (control >> 5) & 3;
        if reset == 3 {
            dict.reset();
            need_dictionary_reset = false;
        } else if need_dictionary_reset {
            return Err("the xz data does not start with a dictionary reset".into());
        }
        if reset >= 2 {
            let properties = *input.get(at).ok_or_else(cut)?;
            at += 1;
            lzma.set_properties(properties)?;
            need_properties = false;
        } else if need_properties {
            return Err("the xz data does not say how it is compressed".into());
        }
        if reset >= 1 {
            lzma.reset();
        }
        let chunk = input.get(at..at + packed).ok_or_else(cut)?;
        lzma.chunk(dict, chunk, unpacked)?;
        at += packed;
    }
}

/// The range decoder over one chunk.
struct Range<'a> {
    input: &'a [u8],
    at: usize,
    range: u32,
    code: u32,
}

const TOP: u32 = 1 << 24;
const BIT_MODEL_BITS: u32 = 11;
const BIT_MODEL_TOTAL: u16 = 1 << BIT_MODEL_BITS;
const MOVE_BITS: u32 = 5;
const PROBABILITY_INIT: u16 = BIT_MODEL_TOTAL / 2;

impl<'a> Range<'a> {
    fn new(input: &'a [u8]) -> Result<Range<'a>, String> {
        if input.len() < 5 || input[0] != 0 {
            return Err("an LZMA2 chunk is damaged".into());
        }
        Ok(Range {
            input,
            at: 5,
            range: u32::MAX,
            code: u32::from_be_bytes([input[1], input[2], input[3], input[4]]),
        })
    }

    fn normalize(&mut self) {
        if self.range < TOP {
            // Past the end of the chunk reads zeros; the chunk is checked once it is done.
            let byte = self.input.get(self.at).copied().unwrap_or(0);
            self.at += 1;
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(byte);
        }
    }

    fn bit(&mut self, probability: &mut u16) -> u32 {
        self.normalize();
        let bound = (self.range >> BIT_MODEL_BITS) * u32::from(*probability);
        if self.code < bound {
            self.range = bound;
            *probability += (BIT_MODEL_TOTAL - *probability) >> MOVE_BITS;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *probability -= *probability >> MOVE_BITS;
            1
        }
    }

    /// A symbol of `limit`'s bits, high bit first, from the tree in `probabilities`.
    fn tree(&mut self, probabilities: &mut [u16], limit: u32) -> u32 {
        let mut symbol = 1u32;
        while symbol < limit {
            symbol = (symbol << 1) | self.bit(&mut probabilities[symbol as usize]);
        }
        symbol - limit
    }

    /// `bits` bits, low bit first, from the tree whose first node is
    /// `probabilities[offset]`.
    fn tree_reverse(&mut self, probabilities: &mut [u16], offset: usize, bits: u32) -> u32 {
        let mut symbol = 1usize;
        let mut value = 0u32;
        for i in 0..bits {
            let bit = self.bit(&mut probabilities[offset + symbol - 1]);
            symbol = (symbol << 1) | bit as usize;
            value |= bit << i;
        }
        value
    }

    /// `bits` bits with even odds.
    fn direct(&mut self, bits: u32) -> u32 {
        let mut value = 0u32;
        for _ in 0..bits {
            self.normalize();
            self.range >>= 1;
            let bit = if self.code >= self.range {
                self.code -= self.range;
                1
            } else {
                0
            };
            value = (value << 1) | bit;
        }
        value
    }
}

const STATES: usize = 12;
const POS_STATES: usize = 16;
const LITERAL_CODER: usize = 0x300;
const DIST_STATES: usize = 4;
const DIST_SLOTS: u32 = 64;
const DIST_MODEL_START: u32 = 4;
const DIST_MODEL_END: u32 = 14;
const FULL_DISTANCES: usize = 128;
const ALIGN_BITS: u32 = 4;
const MATCH_LEN_MIN: u32 = 2;

/// The probabilities of a length.
struct Length {
    choice: u16,
    choice2: u16,
    low: [u16; POS_STATES * 8],
    mid: [u16; POS_STATES * 8],
    high: [u16; 256],
}

impl Length {
    fn new() -> Length {
        Length {
            choice: PROBABILITY_INIT,
            choice2: PROBABILITY_INIT,
            low: [PROBABILITY_INIT; POS_STATES * 8],
            mid: [PROBABILITY_INIT; POS_STATES * 8],
            high: [PROBABILITY_INIT; 256],
        }
    }

    fn decode(&mut self, rc: &mut Range<'_>, pos_state: usize) -> u32 {
        if rc.bit(&mut self.choice) == 0 {
            MATCH_LEN_MIN + rc.tree(&mut self.low[pos_state * 8..pos_state * 8 + 8], 8)
        } else if rc.bit(&mut self.choice2) == 0 {
            MATCH_LEN_MIN + 8 + rc.tree(&mut self.mid[pos_state * 8..pos_state * 8 + 8], 8)
        } else {
            MATCH_LEN_MIN + 16 + rc.tree(&mut self.high, 256)
        }
    }
}

/// The LZMA decoder's state, which lasts across the chunks of a block.
struct Lzma {
    lc: u32,
    lp: u32,
    pb: u32,
    state: usize,
    reps: [u32; 4],
    /// What is left of a match that ran past the end of the last chunk.
    pending: usize,
    is_match: [u16; STATES * POS_STATES],
    is_rep: [u16; STATES],
    is_rep0: [u16; STATES],
    is_rep1: [u16; STATES],
    is_rep2: [u16; STATES],
    is_rep0_long: [u16; STATES * POS_STATES],
    dist_slot: [u16; DIST_STATES * DIST_SLOTS as usize],
    dist_special: [u16; FULL_DISTANCES - DIST_MODEL_END as usize],
    dist_align: [u16; 1 << ALIGN_BITS],
    match_len: Length,
    rep_len: Length,
    literal: Vec<u16>,
}

impl Lzma {
    fn new() -> Lzma {
        Lzma {
            lc: 0,
            lp: 0,
            pb: 0,
            state: 0,
            reps: [0; 4],
            pending: 0,
            is_match: [PROBABILITY_INIT; STATES * POS_STATES],
            is_rep: [PROBABILITY_INIT; STATES],
            is_rep0: [PROBABILITY_INIT; STATES],
            is_rep1: [PROBABILITY_INIT; STATES],
            is_rep2: [PROBABILITY_INIT; STATES],
            is_rep0_long: [PROBABILITY_INIT; STATES * POS_STATES],
            dist_slot: [PROBABILITY_INIT; DIST_STATES * DIST_SLOTS as usize],
            dist_special: [PROBABILITY_INIT; FULL_DISTANCES - DIST_MODEL_END as usize],
            dist_align: [PROBABILITY_INIT; 1 << ALIGN_BITS],
            match_len: Length::new(),
            rep_len: Length::new(),
            literal: vec![PROBABILITY_INIT; LITERAL_CODER << 4],
        }
    }

    fn set_properties(&mut self, properties: u8) -> Result<(), String> {
        let mut value = u32::from(properties);
        if value >= 9 * 5 * 5 {
            return Err("an LZMA2 chunk has properties that do not exist".into());
        }
        self.pb = value / 45;
        value %= 45;
        self.lp = value / 9;
        self.lc = value % 9;
        if self.lc + self.lp > 4 {
            return Err("an LZMA2 chunk has more literal context bits than LZMA2 allows".into());
        }
        Ok(())
    }

    /// Back to the state a block starts in, with the properties kept.
    fn reset(&mut self) {
        let (lc, lp, pb) = (self.lc, self.lp, self.pb);
        *self = Lzma::new();
        self.lc = lc;
        self.lp = lp;
        self.pb = pb;
    }

    /// Decodes one chunk, `unpacked` bytes once decoded.
    fn chunk(&mut self, dict: &mut Dict<'_>, input: &[u8], unpacked: usize) -> Result<(), String> {
        let mut rc = Range::new(input)?;
        let mut left = unpacked;
        let pending = self.pending.min(left);
        for _ in 0..pending {
            let byte = dict.get(self.reps[0])?;
            dict.put(byte)?;
        }
        self.pending -= pending;
        left -= pending;
        let pos_mask = (1u64 << self.pb) - 1;
        let literal_mask = (1u64 << self.lp) - 1;
        while left > 0 {
            let pos_state = (dict.total & pos_mask) as usize;
            let state = self.state;
            if rc.bit(&mut self.is_match[state * POS_STATES + pos_state]) == 0 {
                let previous = if dict.full == 0 { 0 } else { dict.get(0)? };
                let context = (((dict.total & literal_mask) as u32) << self.lc)
                    + (u32::from(previous) >> (8 - self.lc));
                let start = LITERAL_CODER * context as usize;
                let probabilities = &mut self.literal[start..start + LITERAL_CODER];
                let byte = if state < 7 {
                    rc.tree(probabilities, 0x100)
                } else {
                    let mut match_byte = u32::from(dict.get(self.reps[0])?) << 1;
                    let mut offset = 0x100u32;
                    let mut symbol = 1u32;
                    while symbol < 0x100 {
                        let match_bit = match_byte & offset;
                        match_byte <<= 1;
                        let index = (offset + match_bit + symbol) as usize;
                        if rc.bit(&mut probabilities[index]) == 1 {
                            symbol = (symbol << 1) | 1;
                            offset = match_bit;
                        } else {
                            symbol <<= 1;
                            offset ^= match_bit;
                        }
                    }
                    symbol - 0x100
                };
                dict.put(byte as u8)?;
                left -= 1;
                self.state = match state {
                    0..=3 => 0,
                    4..=9 => state - 3,
                    _ => state - 6,
                };
                continue;
            }
            let length;
            if rc.bit(&mut self.is_rep[state]) == 0 {
                // A match with a new distance.
                self.state = if state < 7 { 7 } else { 10 };
                self.reps = [0, self.reps[0], self.reps[1], self.reps[2]];
                length = self.match_len.decode(&mut rc, pos_state);
                let dist_state = ((length - MATCH_LEN_MIN) as usize).min(DIST_STATES - 1);
                let slots = dist_state * DIST_SLOTS as usize;
                let slot = rc.tree(
                    &mut self.dist_slot[slots..slots + DIST_SLOTS as usize],
                    DIST_SLOTS,
                );
                let distance = if slot < DIST_MODEL_START {
                    slot
                } else {
                    let bits = (slot >> 1) - 1;
                    let base = (2 | (slot & 1)) << bits;
                    if slot < DIST_MODEL_END {
                        base + rc.tree_reverse(&mut self.dist_special, (base - slot) as usize, bits)
                    } else {
                        let high = rc.direct(bits - ALIGN_BITS) << ALIGN_BITS;
                        base + high + rc.tree_reverse(&mut self.dist_align, 1, ALIGN_BITS)
                    }
                };
                if distance == u32::MAX {
                    return Err("an LZMA2 chunk holds an end marker".into());
                }
                self.reps[0] = distance;
            } else {
                if rc.bit(&mut self.is_rep0[state]) == 0 {
                    if rc.bit(&mut self.is_rep0_long[state * POS_STATES + pos_state]) == 0 {
                        // One byte from the last distance.
                        self.state = if state < 7 { 9 } else { 11 };
                        let byte = dict.get(self.reps[0])?;
                        dict.put(byte)?;
                        left -= 1;
                        continue;
                    }
                } else {
                    let distance;
                    if rc.bit(&mut self.is_rep1[state]) == 0 {
                        distance = self.reps[1];
                    } else {
                        if rc.bit(&mut self.is_rep2[state]) == 0 {
                            distance = self.reps[2];
                        } else {
                            distance = self.reps[3];
                            self.reps[3] = self.reps[2];
                        }
                        self.reps[2] = self.reps[1];
                    }
                    self.reps[1] = self.reps[0];
                    self.reps[0] = distance;
                }
                self.state = if state < 7 { 8 } else { 11 };
                length = self.rep_len.decode(&mut rc, pos_state);
            }
            let distance = self.reps[0];
            let length = length as usize;
            let now = length.min(left);
            for _ in 0..now {
                let byte = dict.get(distance)?;
                dict.put(byte)?;
            }
            left -= now;
            self.pending = length - now;
        }
        if rc.at > input.len() {
            return Err(cut());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test file: `sample.bin` (text, random bytes, runs and repeats from far back), and
    /// what `xz` made of it with several presets, block sizes, checks and LZMA properties.
    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/gapps/testdata")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn decoded(input: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        decode(input, &mut out, 1 << 30).map(|_| out)
    }

    #[test]
    fn fr32_xz_files_decode_to_what_was_compressed() {
        let plain = fixture("sample.bin");
        for name in [
            "sample.bin.xz",
            "sample-blocks.bin.xz",
            "sample-extreme.bin.xz",
            "sample-props.bin.xz",
            "sample-fast.bin.xz",
        ] {
            assert_eq!(decoded(&fixture(name)).unwrap(), plain, "{name}");
        }
    }

    /// Data that compresses to more than one LZMA2 chunk (a chunk holds at most 64 KiB of
    /// compressed data): `chunks.bin.xz` holds what `xz` made of it.
    fn chunky() -> Vec<u8> {
        let mut state: u64 = 32;
        let mut draw = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        let words: Vec<Vec<u8>> = (0..400)
            .map(|_| {
                let length = 2 + draw() % 5;
                (0..length).map(|_| (draw() & 0xFF) as u8).collect()
            })
            .collect();
        let mut out = Vec::new();
        while out.len() < 160_000 {
            out.extend_from_slice(&words[(draw() % 400) as usize]);
        }
        out
    }

    #[test]
    fn fr32_a_block_of_several_lzma2_chunks_decodes() {
        let packed = fixture("chunks.bin.xz");
        assert!(packed.len() > 65_536, "more than one chunk's worth");
        assert_eq!(decoded(&packed).unwrap(), chunky());
    }

    #[test]
    fn fr32_an_uncompressed_lzma2_chunk_is_taken() {
        // An xz file built by hand: one block holding one uncompressed chunk, no check.
        let mut file = b"\xFD7zXZ\0\x00\x00".to_vec();
        file.extend_from_slice(&[0, 0, 0, 0]);
        // Header: size byte 2 (12 bytes), no sizes, one filter: LZMA2, 1 property byte.
        file.extend_from_slice(&[2, 0, 0x21, 1, 16, 0, 0, 0, 0, 0, 0, 0]);
        file.extend_from_slice(&[1, 0, 4]);
        file.extend_from_slice(b"hello");
        file.push(0);
        while file.len() % 4 != 0 {
            file.push(0);
        }
        file.push(0);
        assert_eq!(decoded(&file).unwrap(), b"hello");
    }

    #[test]
    fn fr32_damaged_xz_data_is_refused_without_a_panic() {
        assert!(decoded(b"not xz at all").is_err());
        let good = fixture("sample.bin.xz");
        assert!(decoded(&good[..good.len() / 2]).is_err(), "cut in half");
        let mut flipped = good.clone();
        for at in (40..flipped.len() - 40).step_by(97) {
            flipped[at] ^= 0x5A;
        }
        // Damage either fails or decodes to something else; it never panics.
        let plain = fixture("sample.bin");
        assert_ne!(decoded(&flipped).ok(), Some(plain));
        let mut out = Vec::new();
        assert!(decode(&good, &mut out, 100).is_err(), "more than the limit");
    }
}

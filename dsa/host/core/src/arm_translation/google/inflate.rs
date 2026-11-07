//! A streaming DEFLATE decoder (RFC 1951), for an archive entry too large to hold in memory:
//! an emulator image's system disk is several GiB once uncompressed.
//!
//! The output goes to a sink in pieces of about a megabyte, keeping only the last 32 KiB
//! (the farthest a DEFLATE stream refers back) between them. The sink can stop the decoding
//! once it has what it needs. Codes up to 10 bits long are decoded through a table; longer
//! ones a bit at a time, as zlib's reference decoder `puff` does.

use std::io::Read;

/// The farthest back a DEFLATE stream refers.
const WINDOW: usize = 32 * 1024;
/// How much output is gathered before it goes to the sink.
const PIECE: usize = 1 << 20;
/// The longest code in any Huffman table DEFLATE uses.
const MAX_BITS: usize = 15;
/// Codes up to this long are decoded through a table.
const FAST_BITS: u32 = 10;

fn early() -> String {
    "the compressed data ends early".to_string()
}

/// The input, read a byte at a time into a bit buffer, lowest bit first.
struct Bits<R> {
    input: R,
    buffer: Vec<u8>,
    position: usize,
    filled: usize,
    value: u64,
    count: u32,
}

impl<R: Read> Bits<R> {
    /// Fills the bit buffer to at least 57 bits, or to what the input has left.
    fn refill(&mut self) -> Result<(), String> {
        while self.count <= 56 {
            if self.position == self.filled {
                self.filled = loop {
                    match self.input.read(&mut self.buffer) {
                        Ok(read) => break read,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(format!("the compressed data cannot be read: {e}")),
                    }
                };
                self.position = 0;
                if self.filled == 0 {
                    return Ok(());
                }
            }
            self.value |= u64::from(self.buffer[self.position]) << self.count;
            self.position += 1;
            self.count += 8;
        }
        Ok(())
    }

    /// The next `need` bits (at most 32) as a number.
    fn bits(&mut self, need: u32) -> Result<u32, String> {
        if self.count < need {
            self.refill()?;
            if self.count < need {
                return Err(early());
            }
        }
        let value = (self.value & ((1u64 << need) - 1)) as u32;
        self.skip(need);
        Ok(value)
    }

    fn skip(&mut self, count: u32) {
        self.value >>= count;
        self.count -= count;
    }
}

/// A canonical Huffman code: a table for the short codes, and for the rest how many codes
/// there are of each length and the symbols in code order.
struct Code {
    /// For each value of the next `FAST_BITS` bits, the symbol shifted left by 4 with the
    /// code's length in the low 4 bits, or 0 when the code is longer.
    fast: Vec<u16>,
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

/// The lowest `length` bits of `code`, in reverse order.
fn reverse(code: u32, length: u32) -> u32 {
    code.reverse_bits() >> (32 - length)
}

impl Code {
    /// The code for these code lengths, one per symbol (0 for an unused symbol). A code that
    /// uses more codes than its lengths allow is refused; an incomplete one is taken, as
    /// DEFLATE allows for a code with a single symbol.
    fn new(lengths: &[u8]) -> Result<Code, String> {
        let mut count = [0u16; MAX_BITS + 1];
        for &length in lengths {
            count[usize::from(length)] += 1;
        }
        let mut left: i32 = 1;
        for &codes in &count[1..] {
            left = (left << 1) - i32::from(codes);
            if left < 0 {
                return Err("the compressed data has an impossible Huffman code".into());
            }
        }
        // The first code of each length, and where its symbols start in `symbol`.
        let mut next = [0u32; MAX_BITS + 2];
        let mut offsets = [0u16; MAX_BITS + 2];
        let mut code = 0u32;
        for length in 1..=MAX_BITS {
            let shorter = if length == 1 { 0 } else { count[length - 1] };
            code = (code + u32::from(shorter)) << 1;
            next[length] = code;
            offsets[length + 1] = offsets[length] + count[length];
        }
        let mut symbol = vec![0u16; lengths.len()];
        let mut fast = vec![0u16; 1 << FAST_BITS];
        for (value, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let length = usize::from(length);
            symbol[usize::from(offsets[length])] = value as u16;
            offsets[length] += 1;
            let code = next[length];
            next[length] += 1;
            if length as u32 <= FAST_BITS {
                let entry = ((value as u16) << 4) | length as u16;
                let mut slot = reverse(code, length as u32) as usize;
                while slot < fast.len() {
                    fast[slot] = entry;
                    slot += 1 << length;
                }
            }
        }
        Ok(Code {
            fast,
            count,
            symbol,
        })
    }

    fn decode<R: Read>(&self, bits: &mut Bits<R>) -> Result<u16, String> {
        if bits.count < MAX_BITS as u32 {
            bits.refill()?;
        }
        let index = (bits.value & ((1 << FAST_BITS) - 1)) as usize;
        let entry = self.fast[index];
        if entry != 0 {
            let length = u32::from(entry & 15);
            if length > bits.count {
                return Err(early());
            }
            bits.skip(length);
            return Ok(entry >> 4);
        }
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for length in 1..=MAX_BITS {
            if length as u32 > bits.count {
                return Err(early());
            }
            code |= ((bits.value >> (length - 1)) & 1) as i32;
            let count = i32::from(self.count[length]);
            if code - count < first {
                bits.skip(length as u32);
                let at = (index + (code - first)) as usize;
                return self.symbol.get(at).copied().ok_or_else(bad_code);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(bad_code())
    }
}

fn bad_code() -> String {
    "the compressed data has a code that is not in its table".to_string()
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The order in which a dynamic block lists the lengths of its code length code.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// What the sink takes: a piece of output. It answers whether to go on.
pub type Sink<'a> = dyn FnMut(&[u8]) -> Result<bool, String> + 'a;

/// The output: what was decoded and not yet given to the sink, after the window kept for
/// references back.
struct Output<'a, 'b> {
    data: Vec<u8>,
    /// Where the bytes not yet given to the sink start in `data`.
    given: usize,
    total: u64,
    sink: &'a mut Sink<'b>,
}

impl Output<'_, '_> {
    /// Gives the sink what it has not had. Returns whether to go on.
    fn flush(&mut self) -> Result<bool, String> {
        let fresh = &self.data[self.given..];
        if fresh.is_empty() {
            return Ok(true);
        }
        self.total += fresh.len() as u64;
        let go_on = (self.sink)(fresh)?;
        let keep = self.data.len().saturating_sub(WINDOW);
        self.data.drain(..keep);
        self.given = self.data.len();
        Ok(go_on)
    }

    /// Flushes once a piece has gathered. Returns whether to go on.
    fn maybe_flush(&mut self) -> Result<bool, String> {
        if self.data.len() - self.given >= PIECE {
            self.flush()
        } else {
            Ok(true)
        }
    }
}

/// Decodes the raw DEFLATE stream `input`, giving the output to `sink` piece by piece until
/// the stream ends or the sink says to stop. Returns how many bytes the sink was given.
pub fn inflate<R: Read>(input: R, sink: &mut Sink<'_>) -> Result<u64, String> {
    let mut bits = Bits {
        input,
        buffer: vec![0; 64 * 1024],
        position: 0,
        filled: 0,
        value: 0,
        count: 0,
    };
    let mut out = Output {
        data: Vec::with_capacity(PIECE + WINDOW + 65_536),
        given: 0,
        total: 0,
        sink,
    };
    let (fixed_literals, fixed_distances) = fixed_codes()?;
    loop {
        let last = bits.bits(1)? == 1;
        let go_on = match bits.bits(2)? {
            0 => stored(&mut bits, &mut out)?,
            1 => codes(&mut bits, &mut out, &fixed_literals, &fixed_distances)?,
            2 => {
                let (literals, distances) = dynamic_codes(&mut bits)?;
                codes(&mut bits, &mut out, &literals, &distances)?
            }
            _ => return Err("the compressed data has a block of an unknown kind".into()),
        };
        if !go_on {
            return Ok(out.total);
        }
        if last {
            out.flush()?;
            return Ok(out.total);
        }
    }
}

fn stored<R: Read>(bits: &mut Bits<R>, out: &mut Output<'_, '_>) -> Result<bool, String> {
    // A stored block starts at the next byte.
    bits.skip(bits.count % 8);
    let length = bits.bits(16)?;
    let check = bits.bits(16)?;
    if length != !check & 0xFFFF {
        return Err("the compressed data has a damaged stored block".into());
    }
    for _ in 0..length {
        out.data.push(bits.bits(8)? as u8);
    }
    out.maybe_flush()
}

fn fixed_codes() -> Result<(Code, Code), String> {
    let mut lengths = [0u8; 288];
    for (symbol, length) in lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((Code::new(&lengths)?, Code::new(&[5u8; 30])?))
}

fn dynamic_codes<R: Read>(bits: &mut Bits<R>) -> Result<(Code, Code), String> {
    let literal_count = bits.bits(5)? as usize + 257;
    let distance_count = bits.bits(5)? as usize + 1;
    let code_length_count = bits.bits(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err("the compressed data has too many codes".into());
    }
    let mut code_lengths = [0u8; 19];
    for &slot in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_lengths[slot] = bits.bits(3)? as u8;
    }
    let code_length_code = Code::new(&code_lengths)?;
    let mut lengths = vec![0u8; literal_count + distance_count];
    let mut index = 0;
    while index < lengths.len() {
        let symbol = code_length_code.decode(bits)?;
        if symbol < 16 {
            lengths[index] = symbol as u8;
            index += 1;
            continue;
        }
        let (value, repeat) = match symbol {
            16 => {
                if index == 0 {
                    return Err("the compressed data repeats a length before the first".into());
                }
                (lengths[index - 1], 3 + bits.bits(2)? as usize)
            }
            17 => (0, 3 + bits.bits(3)? as usize),
            _ => (0, 11 + bits.bits(7)? as usize),
        };
        if index + repeat > lengths.len() {
            return Err("the compressed data has too many code lengths".into());
        }
        lengths[index..index + repeat].fill(value);
        index += repeat;
    }
    if lengths[256] == 0 {
        return Err("the compressed data has no end-of-block code".into());
    }
    let (literals, distances) = lengths.split_at(literal_count);
    Ok((Code::new(literals)?, Code::new(distances)?))
}

fn codes<R: Read>(
    bits: &mut Bits<R>,
    out: &mut Output<'_, '_>,
    literals: &Code,
    distances: &Code,
) -> Result<bool, String> {
    loop {
        let symbol = usize::from(literals.decode(bits)?);
        if symbol < 256 {
            out.data.push(symbol as u8);
        } else if symbol == 256 {
            return Ok(true);
        } else {
            let index = symbol - 257;
            if index >= LENGTH_BASE.len() {
                return Err("the compressed data has a bad length code".into());
            }
            let extra = bits.bits(u32::from(LENGTH_EXTRA[index]))? as usize;
            let length = usize::from(LENGTH_BASE[index]) + extra;
            let index = usize::from(distances.decode(bits)?);
            if index >= DISTANCE_BASE.len() {
                return Err("the compressed data has a bad distance code".into());
            }
            let extra = bits.bits(u32::from(DISTANCE_EXTRA[index]))? as usize;
            let distance = usize::from(DISTANCE_BASE[index]) + extra;
            if distance > out.data.len() {
                return Err("the compressed data refers back before its start".into());
            }
            let start = out.data.len() - distance;
            if distance >= length {
                out.data.extend_from_within(start..start + length);
            } else {
                for offset in 0..length {
                    let byte = out.data[start + offset];
                    out.data.push(byte);
                }
            }
        }
        if !out.maybe_flush()? {
            return Ok(false);
        }
    }
}

/// DEFLATE streams made with the fixed codes, for the tests here and of the image reader.
#[cfg(any(test, feature = "conformance"))]
pub(crate) mod fixture {
    /// Writes bits lowest first, and Huffman codes highest bit first, as DEFLATE does.
    #[derive(Default)]
    pub struct Writer {
        pub bytes: Vec<u8>,
        used: u32,
    }

    impl Writer {
        fn bit(&mut self, bit: u32) {
            if self.used % 8 == 0 {
                self.bytes.push(0);
            }
            let last = self.bytes.len() - 1;
            self.bytes[last] |= ((bit & 1) as u8) << (self.used % 8);
            self.used += 1;
        }

        pub fn number(&mut self, value: u32, count: u32) {
            for i in 0..count {
                self.bit(value >> i);
            }
        }

        pub fn code(&mut self, code: u32, length: u32) {
            for i in (0..length).rev() {
                self.bit(code >> i);
            }
        }

        /// A literal byte, in the fixed code.
        pub fn literal(&mut self, byte: u8) {
            let value = u32::from(byte);
            if value < 144 {
                self.code(0x30 + value, 8);
            } else {
                self.code(0x190 + value - 144, 9);
            }
        }
    }

    /// `data` as one block of the fixed codes: each run of 258 bytes equal to the byte
    /// before is a reference one back, everything else a literal.
    pub fn deflate(data: &[u8]) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.number(1, 1);
        writer.number(1, 2);
        let mut at = 0;
        while at < data.len() {
            let run = at > 0
                && at + 258 <= data.len()
                && data[at..at + 258].iter().all(|&b| b == data[at - 1]);
            if run {
                // Length 258 is symbol 285, the 8-bit code 0xC5; distance 1 is code 0.
                writer.code(0xC5, 8);
                writer.code(0, 5);
                at += 258;
            } else {
                writer.literal(data[at]);
                at += 1;
            }
        }
        // The end of the block is symbol 256, the 7-bit code 0.
        writer.code(0, 7);
        writer.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{deflate, Writer};
    use super::*;

    fn from_hex(text: &str) -> Vec<u8> {
        let text = text.trim();
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Decodes `data` whole, with the sink taking every piece.
    fn decode(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut sink = |piece: &[u8]| -> Result<bool, String> {
            out.extend_from_slice(piece);
            Ok(true)
        };
        let total = inflate(data, &mut sink)?;
        assert_eq!(total as usize, out.len());
        Ok(out)
    }

    /// A fixed-code block of "abc" followed by `repeats` copies of the 258 bytes 3 back.
    fn repeated_abc(repeats: usize) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.number(1, 1);
        writer.number(1, 2);
        for byte in *b"abc" {
            writer.literal(byte);
        }
        for _ in 0..repeats {
            // Length 258 is symbol 285, the 8-bit code 0xC5; distance 3 is code 2.
            writer.code(0xC5, 8);
            writer.code(2, 5);
        }
        writer.code(0, 7);
        writer.bytes
    }

    #[test]
    fn fr25_streamed_inflate_reads_stored_fixed_and_dynamic_blocks() {
        let stored = from_hex("010500faff68656c6c6f");
        assert_eq!(decode(&stored).unwrap(), b"hello");
        let fixed = from_hex("cb48cdc9c957c8c04e0200");
        assert_eq!(decode(&fixed).unwrap(), b"hello hello hello hello hello");
        let dynamic = from_hex(include_str!("../../apk_info/testdata/dynamic.hex"));
        let expected = crate::apk_info::inflate::inflate(&dynamic, 100_000).unwrap();
        assert_eq!(decode(&dynamic).unwrap(), expected);
    }

    #[test]
    fn fr25_streamed_inflate_reads_what_the_fixture_deflates() {
        let mut data: Vec<u8> = (0..=255u8).collect();
        data.extend(vec![0u8; 5000]);
        data.extend(b"end");
        assert_eq!(decode(&deflate(&data)).unwrap(), data);
    }

    #[test]
    fn fr25_streamed_inflate_refers_back_across_pieces() {
        let data = repeated_abc(10_000);
        let out = decode(&data).unwrap();
        assert_eq!(out.len(), 3 + 258 * 10_000);
        assert!(out.len() > 2 * PIECE, "crosses pieces");
        let wrong = (0..out.len()).find(|&i| out[i] != b"abc"[i % 3]);
        assert_eq!(wrong, None);
    }

    #[test]
    fn fr25_streamed_inflate_stops_when_the_sink_has_enough() {
        let data = repeated_abc(10_000);
        let mut pieces = 0;
        let mut sink = |_: &[u8]| -> Result<bool, String> {
            pieces += 1;
            Ok(false)
        };
        let total = inflate(&data[..], &mut sink).unwrap();
        assert_eq!(pieces, 1);
        assert!(total >= PIECE as u64 && total < 2 * PIECE as u64);
    }

    #[test]
    fn fr25_streamed_inflate_refuses_damaged_data() {
        let fixed = from_hex("cb48cdc9c957c8c04e0200");
        assert!(decode(&fixed[..5]).is_err(), "cut short");
        assert!(decode(&[]).is_err());
        assert!(decode(&[0x07]).is_err(), "unknown block kind");
        let refers_back = repeated_abc(1)[..1].to_vec();
        assert!(decode(&refers_back).is_err());
    }
}

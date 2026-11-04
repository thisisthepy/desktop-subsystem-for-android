//! A raw DEFLATE decoder (RFC 1951), for the compressed entries of an APK.
//!
//! An APK's manifest and its icons are usually stored compressed. They are small, so this
//! decoder is written for clarity rather than speed: it decodes Huffman codes one bit at a
//! time, as zlib's reference decoder `puff` does.

/// The longest code in any Huffman table DEFLATE uses.
const MAX_BITS: usize = 15;

struct Bits<'a> {
    input: &'a [u8],
    position: usize,
    bit_buffer: u32,
    bit_count: u32,
}

impl Bits<'_> {
    fn bits(&mut self, need: u32) -> Result<u32, String> {
        let mut value = self.bit_buffer;
        while self.bit_count < need {
            let byte = *self
                .input
                .get(self.position)
                .ok_or("the compressed data ends early")?;
            self.position += 1;
            value |= u32::from(byte) << self.bit_count;
            self.bit_count += 8;
        }
        self.bit_buffer = if need >= 32 { 0 } else { value >> need };
        self.bit_count -= need;
        Ok(if need >= 32 {
            value
        } else {
            value & ((1u32 << need) - 1)
        })
    }
}

/// A canonical Huffman code: how many codes there are of each length, and the symbols in
/// code order.
struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// Builds the code for these code lengths, one per symbol (0 for an unused symbol).
    /// A code that uses more codes than its lengths allow is refused; an incomplete one is
    /// taken, as DEFLATE allows for a code with a single symbol.
    fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let mut count = [0u16; MAX_BITS + 1];
        for &length in lengths {
            count[usize::from(length)] += 1;
        }
        if usize::from(count[0]) == lengths.len() {
            return Ok(Huffman {
                count,
                symbol: Vec::new(),
            });
        }
        let mut left: i32 = 1;
        for &codes in &count[1..] {
            left <<= 1;
            left -= i32::from(codes);
            if left < 0 {
                return Err("the compressed data has an impossible Huffman code".into());
            }
        }
        let mut offsets = [0u16; MAX_BITS + 2];
        for length in 1..=MAX_BITS {
            offsets[length + 1] = offsets[length] + count[length];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (index, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = &mut offsets[usize::from(length)];
                symbol[usize::from(*slot)] = index as u16;
                *slot += 1;
            }
        }
        Ok(Huffman { count, symbol })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, String> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for length in 1..=MAX_BITS {
            code |= bits.bits(1)? as i32;
            let count = i32::from(self.count[length]);
            if code - count < first {
                return self
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or_else(|| "the compressed data has a bad code".to_string());
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("the compressed data has a code that is not in its table".into())
    }
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

/// Decodes the raw DEFLATE stream `input`, refusing to produce more than `limit` bytes.
pub fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut bits = Bits {
        input,
        position: 0,
        bit_buffer: 0,
        bit_count: 0,
    };
    let mut out = Vec::new();
    loop {
        let last = bits.bits(1)? == 1;
        match bits.bits(2)? {
            0 => stored(&mut bits, &mut out, limit)?,
            1 => {
                let (literals, distances) = fixed_tables()?;
                codes(&mut bits, &mut out, &literals, &distances, limit)?;
            }
            2 => {
                let (literals, distances) = dynamic_tables(&mut bits)?;
                codes(&mut bits, &mut out, &literals, &distances, limit)?;
            }
            _ => return Err("the compressed data has a block of an unknown kind".into()),
        }
        if last {
            return Ok(out);
        }
    }
}

fn stored(bits: &mut Bits<'_>, out: &mut Vec<u8>, limit: usize) -> Result<(), String> {
    // A stored block starts at the next byte.
    bits.bit_buffer = 0;
    bits.bit_count = 0;
    let header = bits
        .input
        .get(bits.position..bits.position + 4)
        .ok_or("the compressed data ends early")?;
    let length = u16::from_le_bytes([header[0], header[1]]);
    let check = u16::from_le_bytes([header[2], header[3]]);
    if length != !check {
        return Err("the compressed data has a damaged stored block".into());
    }
    bits.position += 4;
    let data = bits
        .input
        .get(bits.position..bits.position + usize::from(length))
        .ok_or("the compressed data ends early")?;
    if out.len() + data.len() > limit {
        return Err(too_large(limit));
    }
    out.extend_from_slice(data);
    bits.position += usize::from(length);
    Ok(())
}

fn too_large(limit: usize) -> String {
    format!("the entry is larger than {limit} bytes once uncompressed")
}

fn fixed_tables() -> Result<(Huffman, Huffman), String> {
    let mut lengths = [0u8; 288];
    for (symbol, length) in lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((Huffman::new(&lengths)?, Huffman::new(&[5u8; 30])?))
}

fn dynamic_tables(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), String> {
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
    let code_length_code = Huffman::new(&code_lengths)?;
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
    Ok((
        Huffman::new(&lengths[..literal_count])?,
        Huffman::new(&lengths[literal_count..])?,
    ))
}

fn codes(
    bits: &mut Bits<'_>,
    out: &mut Vec<u8>,
    literals: &Huffman,
    distances: &Huffman,
    limit: usize,
) -> Result<(), String> {
    loop {
        let symbol = usize::from(literals.decode(bits)?);
        match symbol {
            0..=255 => {
                if out.len() >= limit {
                    return Err(too_large(limit));
                }
                out.push(symbol as u8);
            }
            256 => return Ok(()),
            _ => {
                let index = symbol - 257;
                if index >= LENGTH_BASE.len() {
                    return Err("the compressed data has a bad length code".into());
                }
                let length = usize::from(LENGTH_BASE[index])
                    + bits.bits(u32::from(LENGTH_EXTRA[index]))? as usize;
                let index = usize::from(distances.decode(bits)?);
                if index >= DISTANCE_BASE.len() {
                    return Err("the compressed data has a bad distance code".into());
                }
                let distance = usize::from(DISTANCE_BASE[index])
                    + bits.bits(u32::from(DISTANCE_EXTRA[index]))? as usize;
                if distance > out.len() {
                    return Err("the compressed data refers back before its start".into());
                }
                if out.len() + length > limit {
                    return Err(too_large(limit));
                }
                let start = out.len() - distance;
                for offset in 0..length {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn fr11_a_stored_block_is_copied() {
        // "hello" in one stored block.
        let data = from_hex("010500faff68656c6c6f");
        assert_eq!(inflate(&data, 100).unwrap(), b"hello");
    }

    #[test]
    fn fr11_a_block_with_the_fixed_codes_is_decoded() {
        // zlib's raw deflate (level 9) of "hello" five times, with spaces between.
        let data = from_hex("cb48cdc9c957c8c04e0200");
        let text = inflate(&data, 100).unwrap();
        assert_eq!(text, b"hello hello hello hello hello");
    }

    #[test]
    fn fr11_a_block_with_dynamic_codes_is_decoded() {
        let text: String = (0..200)
            .map(|i| format!("<uses-permission android:name=\"p{}\"/>", i % 37))
            .collect();
        let data = from_hex(DYNAMIC);
        assert_eq!(inflate(&data, 100_000).unwrap(), text.as_bytes());
    }

    #[test]
    fn fr11_an_entry_larger_than_the_limit_is_refused() {
        let data = from_hex("cb48cdc9c957c8c04e0200");
        assert!(inflate(&data, 10).is_err());
        assert!(inflate(&[], 10).is_err());
        assert!(inflate(&[0x07], 10).is_err());
    }

    /// zlib's raw deflate (level 9) of the permission lines in the test above.
    const DYNAMIC: &str = include_str!("testdata/dynamic.hex");
}

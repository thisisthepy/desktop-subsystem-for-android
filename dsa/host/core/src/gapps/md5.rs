//! MD5 (RFC 1321), the digest LiteGapps' file host publishes for each file. It is the only
//! digest published there, so a download is checked against it; the image the guest gets is
//! then checked with SHA-256 on its way.

/// How much each step rotates by.
const SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// An MD5 digest being computed.
#[derive(Clone)]
pub struct Md5 {
    state: [u32; 4],
    /// The constants, the integer part of 2^32 times |sin(i + 1)|, as RFC 1321 defines them.
    table: [u32; 64],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Md5::new()
    }
}

impl Md5 {
    pub fn new() -> Md5 {
        let mut table = [0u32; 64];
        for (i, value) in table.iter_mut().enumerate() {
            *value = ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
        }
        Md5 {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            table,
            buffer: [0u8; 64],
            buffered: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered < 64 {
                return;
            }
            let block = self.buffer;
            self.block(&block);
            self.buffered = 0;
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.block(block.try_into().expect("64 bytes"));
            data = rest;
        }
        self.buffer[..data.len()].copy_from_slice(data);
        self.buffered = data.len();
    }

    /// The digest of everything given to [`update`](Self::update).
    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.length.wrapping_mul(8);
        let mut tail = vec![0x80u8];
        let used = (self.buffered + 1) % 64;
        let zeros = if used <= 56 { 56 - used } else { 120 - used };
        tail.extend(std::iter::repeat_n(0u8, zeros));
        tail.extend_from_slice(&bits.to_le_bytes());
        let length = self.length;
        self.update(&tail);
        self.length = length;
        let mut out = [0u8; 16];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn block(&mut self, block: &[u8; 64]) {
        let mut words = [0u32; 16];
        for (i, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for (i, (&constant, &shift)) in self.table.iter().zip(&SHIFTS).enumerate() {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(constant)
                .wrapping_add(words[g])
                .rotate_left(shift);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

/// `digest` as lower-case hexadecimal.
pub fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md5(data: &[u8]) -> String {
        let mut md5 = Md5::new();
        md5.update(data);
        hex(&md5.finish())
    }

    #[test]
    fn fr32_md5_matches_the_rfc_1321_test_suite() {
        for (input, digest) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(md5(input.as_bytes()), digest, "{input:?}");
        }
    }

    #[test]
    fn fr32_md5_is_the_same_however_the_input_is_split() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 253) as u8).collect();
        let whole = md5(&data);
        for step in [1, 7, 63, 64, 65, 1000] {
            let mut parts = Md5::new();
            for piece in data.chunks(step) {
                parts.update(piece);
            }
            assert_eq!(hex(&parts.finish()), whole, "in pieces of {step}");
        }
    }
}

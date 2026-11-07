//! SHA-1 (FIPS 180-4), the checksum Google's SDK repository lists for each download.
//!
//! It is used only to compare a download with that list, never to sign or to trust anything
//! on its own; the list itself comes over HTTPS from Google.

/// A SHA-1 digest being computed.
#[derive(Debug, Clone)]
pub struct Sha1 {
    state: [u32; 5],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Sha1::new()
    }
}

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1 {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Adds `data` to what is hashed.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            let start = self.buffered;
            let end = start + take;
            self.buffer[start..end].copy_from_slice(&data[..take]);
            self.buffered = end;
            data = &data[take..];
            if self.buffered < 64 {
                return;
            }
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.buffered = 0;
        }
        let mut blocks = data.chunks_exact(64);
        for block in &mut blocks {
            compress(&mut self.state, block);
        }
        let rest = blocks.remainder();
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// The digest of everything added.
    pub fn finish(mut self) -> [u8; 20] {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buffered != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 20];
        for (bytes, word) in out.chunks_exact_mut(4).zip(self.state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

fn compress(state: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (i, word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(value);
    }
}

/// Lower-case hexadecimal of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(parts: &[&[u8]]) -> String {
        let mut hasher = Sha1::new();
        for part in parts {
            hasher.update(part);
        }
        hex(&hasher.finish())
    }

    #[test]
    fn fr25_sha1_matches_the_standard_test_vectors() {
        let empty = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        assert_eq!(digest(&[]), empty);
        let abc = "a9993e364706816aba3e25717850c26c9cd0d89d";
        assert_eq!(digest(&[b"abc"]), abc);
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        let expected = "84983e441c3bd26ebaae4aa1f95129e5e54670f1";
        assert_eq!(digest(&[long]), expected);
        let (first, second) = long.split_at(13);
        assert_eq!(digest(&[first, second]), expected, "in two parts");
    }

    #[test]
    fn fr25_sha1_of_a_million_bytes_in_uneven_parts() {
        let data = vec![b'a'; 1_000_000];
        let mut hasher = Sha1::new();
        for part in data.chunks(997) {
            hasher.update(part);
        }
        let expected = "34aa973cd4c4daa4f61eeb2bdbad27316534016f";
        assert_eq!(hex(&hasher.finish()), expected);
    }
}

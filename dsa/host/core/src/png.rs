//! PNG images in and out: the app icons and widget pictures the guest sends, decoded for the
//! windows, and the screenshots a window's menu takes, encoded for the clipboard and the
//! Pictures folder.
//!
//! The host links no image library for PNG, so both directions are written here, on the
//! DEFLATE decoder the APK reader already has. Decoding takes what Android's own encoder
//! writes (8 or 16 bits a sample, gray, RGB, palette, with or without alpha, not
//! interlaced). Encoding writes 8-bit RGB or RGBA with one fixed-code DEFLATE block and a
//! short LZ77 search, which keeps a screenshot a fraction of its raw size without a large
//! encoder.

use crate::apk_info::inflate::inflate;
use crate::diagnostics::crc32;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The largest image decoded, in pixels; an icon is far smaller, and a larger one comes from
/// a broken guest.
pub const MAX_PIXELS: usize = 4096 * 4096;

/// An image: `width` x `height` pixels, four bytes each (red, green, blue, alpha, not
/// premultiplied), rows from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The pixels of the PNG `png`.
pub fn decode(png: &[u8]) -> Result<Rgba, String> {
    if png.len() < 8 || png[..8] != SIGNATURE {
        return Err("not a PNG".into());
    }
    let (mut at, mut header, mut palette, mut alpha, mut data) =
        (8, None, Vec::new(), Vec::new(), Vec::new());
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]) as usize;
        let kind = &png[at + 4..at + 8];
        let body = png
            .get(at + 8..at + 8 + len)
            .ok_or("a PNG chunk runs past the end")?;
        match kind {
            b"IHDR" => header = Some(body.to_vec()),
            b"PLTE" => palette = body.to_vec(),
            b"tRNS" => alpha = body.to_vec(),
            b"IDAT" => data.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    let header = header.ok_or("the PNG has no header")?;
    if header.len() < 13 {
        return Err("the PNG's header is short".into());
    }
    let width = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let height = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let (depth, colour, interlace) = (header[8], header[9], header[12]);
    if width == 0 || height == 0 || width as usize * height as usize > MAX_PIXELS {
        return Err(format!("the PNG is {width}x{height}"));
    }
    let channels = match colour {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        other => return Err(format!("the PNG has colour type {other}")),
    };
    let bytes_per_sample = match (depth, colour) {
        (8, _) => 1,
        (16, 0 | 2 | 4 | 6) => 2,
        _ => {
            return Err(format!(
                "the PNG has {depth} bits a sample in colour type {colour}"
            ))
        }
    };
    if interlace != 0 {
        return Err("the PNG is interlaced".into());
    }
    if data.len() < 2 {
        return Err("the PNG has no image data".into());
    }
    let (width_px, height_px) = (width as usize, height as usize);
    let bpp = channels * bytes_per_sample;
    let stride = width_px * bpp;
    // A zlib stream: two bytes of header before the DEFLATE data.
    let raw = inflate(&data[2..], height_px * (stride + 1))?;
    if raw.len() < height_px * (stride + 1) {
        return Err("the PNG's image data is short".into());
    }
    let mut lines = vec![0u8; height_px * stride];
    for y in 0..height_px {
        let filter = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp {
                lines[y * stride + x - bpp]
            } else {
                0
            };
            let b = if y > 0 {
                lines[(y - 1) * stride + x]
            } else {
                0
            };
            let c = if x >= bpp && y > 0 {
                lines[(y - 1) * stride + x - bpp]
            } else {
                0
            };
            let predicted = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => paeth(a, b, c),
                other => return Err(format!("the PNG uses filter {other}")),
            };
            lines[y * stride + x] = line[x].wrapping_add(predicted);
        }
    }
    let mut pixels = Vec::with_capacity(width_px * height_px * 4);
    for pixel in lines.chunks_exact(bpp) {
        // A 16-bit sample keeps its high byte.
        let sample = |index: usize| pixel[index * bytes_per_sample];
        let rgba = match colour {
            0 => [sample(0), sample(0), sample(0), 0xff],
            2 => [sample(0), sample(1), sample(2), 0xff],
            3 => {
                let index = usize::from(sample(0));
                let rgb = palette
                    .get(index * 3..index * 3 + 3)
                    .ok_or("the PNG's palette is short")?;
                [
                    rgb[0],
                    rgb[1],
                    rgb[2],
                    alpha.get(index).copied().unwrap_or(0xff),
                ]
            }
            4 => [sample(0), sample(0), sample(0), sample(1)],
            _ => [sample(0), sample(1), sample(2), sample(3)],
        };
        pixels.extend_from_slice(&rgba);
    }
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// `image` as a PNG: RGBA when `keep_alpha`, else RGB, which a screenshot is.
pub fn encode(image: &Rgba, keep_alpha: bool) -> Vec<u8> {
    let (width, height) = (image.width as usize, image.height as usize);
    let bpp = if keep_alpha { 4 } else { 3 };
    let stride = width * bpp;
    let mut samples = Vec::with_capacity(height * stride);
    for pixel in image.pixels.chunks_exact(4).take(width * height) {
        samples.extend_from_slice(&pixel[..bpp]);
    }
    samples.resize(height * stride, 0);
    // Each row with the filter that leaves the smallest sum of magnitudes, the heuristic
    // libpng uses.
    let mut filtered = Vec::with_capacity(height * (stride + 1));
    let mut candidate = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for y in 0..height {
        let row = &samples[y * stride..(y + 1) * stride];
        let above = (y > 0).then(|| &samples[(y - 1) * stride..y * stride]);
        let mut best_filter = 0u8;
        let mut best_cost = u64::MAX;
        for filter in 0..=4u8 {
            for x in 0..stride {
                let a = if x >= bpp { row[x - bpp] } else { 0 };
                let b = above.map_or(0, |above| above[x]);
                let c = match above {
                    Some(above) if x >= bpp => above[x - bpp],
                    _ => 0,
                };
                let predicted = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                candidate[x] = row[x].wrapping_sub(predicted);
            }
            let cost: u64 = candidate
                .iter()
                .map(|&value| u64::from((value as i8).unsigned_abs()))
                .sum();
            if cost < best_cost {
                best_cost = cost;
                best_filter = filter;
                best.copy_from_slice(&candidate);
            }
        }
        filtered.push(best_filter);
        filtered.extend_from_slice(&best);
    }
    let mut zlib = vec![0x78, 0x01];
    zlib.extend_from_slice(&deflate(&filtered));
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&image.width.to_be_bytes());
    header.extend_from_slice(&image.height.to_be_bytes());
    header.extend_from_slice(&[8, if keep_alpha { 6 } else { 2 }, 0, 0, 0]);
    let mut png = SIGNATURE.to_vec();
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    png.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(body);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// DEFLATE's bits, least significant first.
struct BitWriter {
    out: Vec<u8>,
    bits: u64,
    count: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, count: u32) {
        self.bits |= u64::from(value) << self.count;
        self.count += count;
        while self.count >= 8 {
            self.out.push(self.bits as u8);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    /// A Huffman code, which DEFLATE stores from its most significant bit.
    fn code(&mut self, code: u32, length: u32) {
        let mut reversed = 0;
        for bit in 0..length {
            reversed = (reversed << 1) | ((code >> bit) & 1);
        }
        self.put(reversed, length);
    }

    /// A literal or length symbol in the fixed code.
    fn symbol(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0x30 + symbol, 8),
            144..=255 => self.code(0x190 + symbol - 144, 9),
            256..=279 => self.code(symbol - 256, 7),
            _ => self.code(0xc0 + symbol - 280, 8),
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push(self.bits as u8);
        }
        self.out
    }
}

const LENGTH_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// How far back a match may start.
const WINDOW: usize = 32768;
/// How many earlier places with the same three bytes are tried for a match.
const CHAIN: usize = 16;
const NO_PLACE: usize = usize::MAX;

/// `data` as one final DEFLATE block of the fixed code.
fn deflate(data: &[u8]) -> Vec<u8> {
    let mut bits = BitWriter {
        out: Vec::with_capacity(data.len() / 2 + 16),
        bits: 0,
        count: 0,
    };
    // The final block, of the fixed code.
    bits.put(1, 1);
    bits.put(1, 2);
    let mut places = Places {
        head: vec![NO_PLACE; 1 << 15],
        previous: vec![NO_PLACE; WINDOW],
    };
    let n = data.len();
    let mut at = 0;
    while at < n {
        let (mut best_length, mut best_distance) = (0, 0);
        if at + 3 <= n {
            let mut place = places.head[hash(data, at)];
            let longest = (n - at).min(258);
            for _ in 0..CHAIN {
                // The search ends at a place outside the window, or a stale entry of the table.
                if place == NO_PLACE || place >= at || at - place > WINDOW {
                    break;
                }
                let mut length = 0;
                while length < longest && data[place + length] == data[at + length] {
                    length += 1;
                }
                if length > best_length {
                    best_length = length;
                    best_distance = at - place;
                    if length == longest {
                        break;
                    }
                }
                place = places.previous[place % WINDOW];
            }
        }
        if best_length >= 3 {
            let length = best_length as u32;
            let code = LENGTH_BASE
                .iter()
                .rposition(|&base| base <= length)
                .unwrap_or(0);
            bits.symbol(257 + code as u32);
            bits.put(length - LENGTH_BASE[code], LENGTH_EXTRA[code]);
            let distance = best_distance as u32;
            let code = DISTANCE_BASE
                .iter()
                .rposition(|&base| base <= distance)
                .unwrap_or(0);
            bits.code(code as u32, 5);
            bits.put(distance - DISTANCE_BASE[code], DISTANCE_EXTRA[code]);
            for place in at..at + best_length {
                places.insert(data, place);
            }
            at += best_length;
        } else {
            bits.symbol(u32::from(data[at]));
            places.insert(data, at);
            at += 1;
        }
    }
    bits.symbol(256);
    bits.finish()
}

/// The last place each three bytes were seen, and before it, for finding matches.
struct Places {
    head: Vec<usize>,
    previous: Vec<usize>,
}

impl Places {
    fn insert(&mut self, data: &[u8], at: usize) {
        if at + 3 <= data.len() {
            let key = hash(data, at);
            self.previous[at % WINDOW] = self.head[key];
            self.head[key] = at;
        }
    }
}

fn hash(data: &[u8], at: usize) -> usize {
    ((usize::from(data[at]) << 10) ^ (usize::from(data[at + 1]) << 5) ^ usize::from(data[at + 2]))
        & 0x7fff
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Rgba {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x * 7) as u8,
                    (y * 5) as u8,
                    ((x + y) % 3) as u8,
                    0xff,
                ]);
            }
        }
        Rgba {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn fr22_a_screenshot_png_reads_back_as_the_pixels_it_was_made_of() {
        let image = gradient(61, 37);
        let png = encode(&image, false);
        assert_eq!(decode(&png), Ok(image.clone()));
        let mut clear = image.clone();
        for (index, pixel) in clear.pixels.chunks_exact_mut(4).enumerate() {
            pixel[3] = (index % 256) as u8;
        }
        assert_eq!(decode(&encode(&clear, true)), Ok(clear));
    }

    #[test]
    fn fr22_a_screenshot_of_one_colour_compresses() {
        let image = Rgba {
            width: 640,
            height: 480,
            pixels: [0x20, 0x40, 0x60, 0xff].repeat(640 * 480),
        };
        let png = encode(&image, false);
        assert!(png.len() < 640 * 480 / 10, "{} bytes", png.len());
        assert_eq!(decode(&png), Ok(image));
    }

    #[test]
    fn fr22_an_icon_png_of_every_colour_type_decodes() {
        // A 2x1 palette image with a see-through second entry, written by hand: the palette
        // is red and green, tRNS makes green half transparent.
        let raw = [0u8, 0, 1];
        let mut zlib = vec![0x78, 0x01];
        // A stored DEFLATE block of the three bytes.
        zlib.extend_from_slice(&[1, 3, 0, 0xfc, 0xff]);
        zlib.extend_from_slice(&raw);
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
        let mut png = SIGNATURE.to_vec();
        chunk(&mut png, b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 3, 0, 0, 0]);
        chunk(&mut png, b"PLTE", &[0xff, 0, 0, 0, 0xff, 0]);
        chunk(&mut png, b"tRNS", &[0xff, 0x80]);
        chunk(&mut png, b"IDAT", &zlib);
        chunk(&mut png, b"IEND", &[]);
        assert_eq!(
            decode(&png).unwrap().pixels,
            vec![0xff, 0, 0, 0xff, 0, 0xff, 0, 0x80]
        );
        assert!(decode(b"not a png").is_err());
        assert!(decode(&SIGNATURE).is_err());
    }
}

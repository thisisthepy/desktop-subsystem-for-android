//! The clipboard the host and the guest share: text and images.
//!
//! Text copied on one side becomes the other side's clipboard text, and an image copied on one
//! side becomes the other side's clipboard image, as long as the user has not turned sharing
//! off. Images cross as PNG. Every host turns its own image formats into PNG with the rules
//! here ([`dib_to_png`] and [`png_to_dib`] for the device independent bitmaps of Windows), so
//! a host never keeps its own copy of them. Other formats stay where they were copied.
//!
//! On the control channel an image goes in pieces ([`image_pieces`], put back together by
//! [`ImageAssembler`]), so other messages pass between them and one large image does not hold
//! up input or window messages.

use std::io::Cursor;
use std::sync::Arc;

use dsa_protocol::control;

/// The longest text that crosses, in bytes of UTF-8.
///
/// Android hands a new clip to the clipboard service in a binder transaction, and a process's
/// binder buffer is 1 MiB, shared by every transaction in flight. The text travels as UTF-16
/// there, so 256 KiB of UTF-8 takes at most 512 KiB, which leaves room for the rest. A longer
/// clip is dropped, never cut: a paste is either the whole text or nothing.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

/// The longest PNG that crosses, in bytes.
///
/// An image never travels through binder (the guest's clip holds a `content://` URI), so the
/// limit comes from the channels: the local API carries one image in one frame, which must stay
/// under the 16 MiB a frame may have. A screenshot of a 4K screen is a few MiB as PNG. A larger
/// image is dropped, never cut, and the clipboard on the other side stays as it was.
pub const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;

/// The most pixels an image may have, so a small PNG that unpacks into gigabytes is refused
/// before it is unpacked. An 8K screen (7680 by 4320) has 33 million.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;

/// The longest piece of an image on the control channel, in bytes.
pub const IMAGE_PIECE_BYTES: usize = 256 * 1024;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// What crosses between the clipboards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clip {
    /// Text, at most [`MAX_CLIPBOARD_BYTES`] of UTF-8.
    Text(String),
    /// An image as PNG, at most [`MAX_IMAGE_BYTES`]. Shared, so a clip kept in several places
    /// is held once.
    Image(Arc<[u8]>),
}

impl Clip {
    /// Whether the clip may cross, and why not when it may not, for a log line that says which
    /// side it was copied on (`from`).
    pub fn check(&self, from: &str) -> Result<(), String> {
        match self {
            Clip::Text(text) if fits(text) => Ok(()),
            Clip::Text(text) => Err(too_long(text, from)),
            Clip::Image(png) => check_image(png)
                .map_err(|why| format!("clipboard: dropped an image copied on the {from}: {why}")),
        }
    }
}

/// Whether `text` is short enough to cross.
pub fn fits(text: &str) -> bool {
    text.len() <= MAX_CLIPBOARD_BYTES
}

/// Says that a clip was too long to cross, for a log line.
pub fn too_long(text: &str, from: &str) -> String {
    format!(
        "clipboard: dropped text copied on the {from}: {} bytes, longer than the {} that cross",
        text.len(),
        MAX_CLIPBOARD_BYTES
    )
}

/// Whether `png` is an image that may cross: a PNG no longer than [`MAX_IMAGE_BYTES`], whose
/// header can be read and whose size is at most [`MAX_IMAGE_PIXELS`]. Only the header is read.
pub fn check_image(png: &[u8]) -> Result<(), String> {
    if png.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "{} bytes of PNG, more than the {MAX_IMAGE_BYTES} that cross",
            png.len()
        ));
    }
    if !png.starts_with(&PNG_SIGNATURE) {
        return Err("it is not a PNG".into());
    }
    let reader = png::Decoder::new(Cursor::new(png))
        .read_info()
        .map_err(|error| format!("its PNG header cannot be read ({error})"))?;
    let info = reader.info();
    check_pixels(info.width, info.height)
}

fn check_pixels(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("the image is empty".into());
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(format!(
            "{width} by {height} pixels, more than the {MAX_IMAGE_PIXELS} that cross"
        ));
    }
    Ok(())
}

/// The pieces an image goes in on the control channel, in order, each naming `clip_id` and the
/// whole length.
pub fn image_pieces(
    clip_id: u32,
    png: &[u8],
) -> impl Iterator<Item = control::ClipboardImage> + '_ {
    let size = png.len() as u64;
    png.chunks(IMAGE_PIECE_BYTES)
        .scan(0u64, move |offset, data| {
            let piece = control::ClipboardImage {
                clip_id,
                size,
                offset: *offset,
                data: data.to_vec(),
            };
            *offset += data.len() as u64;
            Some(piece)
        })
}

/// Puts the pieces of the images the other side sends back together.
#[derive(Debug, Default)]
pub struct ImageAssembler {
    /// The clip being put together, its whole length, and what came of it so far.
    current: Option<(u32, u64, Vec<u8>)>,
    /// A clip that was refused, whose later pieces are dropped without a word.
    refused: Option<u32>,
}

impl ImageAssembler {
    /// Takes one piece. `Ok(Some(png))` once a clip is whole, `Ok(None)` while it is not, and an
    /// error, once, when the clip is refused: longer than [`MAX_IMAGE_BYTES`], or with pieces
    /// that do not follow one another. A piece of another clip drops one that was not whole.
    pub fn take(&mut self, piece: control::ClipboardImage) -> Result<Option<Vec<u8>>, String> {
        if self.refused == Some(piece.clip_id) {
            return Ok(None);
        }
        if self.current.as_ref().map(|(id, _, _)| *id) != Some(piece.clip_id) {
            if piece.size > MAX_IMAGE_BYTES as u64 {
                return Err(self.refuse(
                    piece.clip_id,
                    format!(
                        "{} bytes of PNG, more than the {MAX_IMAGE_BYTES} that cross",
                        piece.size
                    ),
                ));
            }
            self.current = Some((piece.clip_id, piece.size, Vec::new()));
        }
        let Some((clip_id, size, have)) = self
            .current
            .as_ref()
            .map(|(clip_id, size, data)| (*clip_id, *size, data.len() as u64))
        else {
            return Ok(None);
        };
        if piece.size != size || piece.offset != have {
            return Err(self.refuse(
                clip_id,
                format!(
                    "a piece at {} of {} bytes came after {have} of {size}",
                    piece.offset, piece.size
                ),
            ));
        }
        if have + piece.data.len() as u64 > size {
            return Err(self.refuse(clip_id, format!("its pieces are longer than {size} bytes")));
        }
        let Some((_, _, data)) = self.current.as_mut() else {
            return Ok(None);
        };
        data.extend_from_slice(&piece.data);
        if data.len() as u64 == size {
            let (_, _, png) = self.current.take().unwrap_or_default();
            return Ok(Some(png));
        }
        Ok(None)
    }

    /// Forgets a clip that was not whole, as when the connection is new.
    pub fn reset(&mut self) {
        self.current = None;
        self.refused = None;
    }

    fn refuse(&mut self, clip_id: u32, why: String) -> String {
        self.current = None;
        self.refused = Some(clip_id);
        why
    }
}

/// An image as rows of 8-bit RGBA, top row first.
struct Rgba {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn encode_png(image: &Rgba) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("cannot write the PNG ({error})"))?;
        writer
            .write_image_data(&image.pixels)
            .map_err(|error| format!("cannot write the PNG ({error})"))?;
    }
    Ok(out)
}

fn decode_png(png: &[u8]) -> Result<Rgba, String> {
    check_image(png)?;
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("the PNG cannot be read ({error})"))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("the PNG cannot be read ({error})"))?;
    let (width, height) = (frame.width, frame.height);
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("the PNG kept its palette".into()),
    };
    let line = frame.line_size;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in buffer[..line * height as usize].chunks(line) {
        for px in row[..width as usize * channels].chunks(channels) {
            let rgba = match channels {
                1 => [px[0], px[0], px[0], 255],
                2 => [px[0], px[0], px[0], px[1]],
                3 => [px[0], px[1], px[2], 255],
                _ => [px[0], px[1], px[2], px[3]],
            };
            pixels.extend_from_slice(&rgba);
        }
    }
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// The value of the bits of `pixel` under `mask`, scaled to eight bits.
fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let value = (pixel & mask) >> shift;
    let max = (1u64 << bits) - 1;
    ((u64::from(value) * 255 + max / 2) / max) as u8
}

/// Turns a device independent bitmap (a `BITMAPINFOHEADER`, `BITMAPV4HEADER` or
/// `BITMAPV5HEADER`, its masks or palette, then the pixels, as the Windows clipboard holds
/// `CF_DIB` and `CF_DIBV5`) into a PNG.
///
/// Rules: 1, 4 and 8 bits a pixel go through the palette; 16 and 32 bits through their masks
/// (5-5-5 and 8-8-8 when the bitmap has none); 24 bits are blue, green, red. A bitmap that says
/// it has alpha (a mask for it) keeps it, except when every pixel's alpha is zero, which is how
/// many programs write an opaque 32-bit bitmap. Compressed bitmaps (RLE, JPEG, PNG inside a
/// DIB) are refused.
pub fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "the bitmap is cut short".to_string();
    let header = u32_at(dib, 0).ok_or_else(bad)? as usize;
    if header < 40 || header > dib.len() {
        return Err(format!("the bitmap has a header of {header} bytes"));
    }
    let width = u32_at(dib, 4).ok_or_else(bad)? as i32;
    let raw_height = u32_at(dib, 8).ok_or_else(bad)? as i32;
    let bits = u16_at(dib, 14).ok_or_else(bad)?;
    let compression = u32_at(dib, 16).ok_or_else(bad)?;
    let used_colors = u32_at(dib, 32).ok_or_else(bad)?;
    if width <= 0 || raw_height == 0 || raw_height == i32::MIN {
        return Err(format!("the bitmap is {width} by {raw_height} pixels"));
    }
    let width = width as u32;
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs();
    check_pixels(width, height)?;

    let mut at = header;
    let (mut red, mut green, mut blue, mut alpha) = match bits {
        16 => (0x7c00, 0x03e0, 0x001f, 0),
        32 => (0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0),
        _ => (0, 0, 0, 0),
    };
    match compression {
        BI_RGB => {}
        BI_BITFIELDS | BI_ALPHABITFIELDS if bits == 16 || bits == 32 => {
            if header >= 52 {
                red = u32_at(dib, 40).ok_or_else(bad)?;
                green = u32_at(dib, 44).ok_or_else(bad)?;
                blue = u32_at(dib, 48).ok_or_else(bad)?;
                if header >= 56 {
                    alpha = u32_at(dib, 52).ok_or_else(bad)?;
                }
            } else {
                red = u32_at(dib, at).ok_or_else(bad)?;
                green = u32_at(dib, at + 4).ok_or_else(bad)?;
                blue = u32_at(dib, at + 8).ok_or_else(bad)?;
                at += 12;
                if compression == BI_ALPHABITFIELDS {
                    alpha = u32_at(dib, at).ok_or_else(bad)?;
                    at += 4;
                }
            }
        }
        other => return Err(format!("the bitmap is compressed (kind {other})")),
    }
    // A V4 or V5 header holds an alpha mask even for BI_RGB; 32-bit pixels then carry alpha.
    if compression == BI_RGB && bits == 32 && header >= 56 {
        alpha = u32_at(dib, 52).ok_or_else(bad)?;
    }

    let palette: Vec<[u8; 4]> = if bits <= 8 {
        let count = if used_colors == 0 {
            1usize << bits
        } else {
            used_colors.min(256) as usize
        };
        let table = dib.get(at..at + count * 4).ok_or_else(bad)?;
        at += count * 4;
        table
            .chunks(4)
            .map(|entry| [entry[2], entry[1], entry[0], 255])
            .collect()
    } else {
        Vec::new()
    };
    if !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(format!("the bitmap has {bits} bits a pixel"));
    }

    let stride = (width as usize * bits as usize).div_ceil(32) * 4;
    let pixels = dib.get(at..).ok_or_else(bad)?;
    if pixels.len() < stride * height as usize {
        return Err(bad());
    }
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let source = if top_down { y } else { height as usize - 1 - y };
        let row = &pixels[source * stride..source * stride + stride];
        for x in 0..width as usize {
            let px = match bits {
                1 | 4 | 8 => {
                    let bit = x * bits as usize;
                    let byte = row[bit / 8];
                    let index =
                        (byte >> (8 - bits as usize - bit % 8)) & ((1u16 << bits) - 1) as u8;
                    *palette.get(index as usize).unwrap_or(&[0, 0, 0, 255])
                }
                16 => {
                    let value = u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]));
                    let a = if alpha == 0 {
                        255
                    } else {
                        channel(value, alpha)
                    };
                    [
                        channel(value, red),
                        channel(value, green),
                        channel(value, blue),
                        a,
                    ]
                }
                24 => [row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255],
                _ => {
                    let value =
                        u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap_or_default());
                    let a = if alpha == 0 {
                        255
                    } else {
                        channel(value, alpha)
                    };
                    [
                        channel(value, red),
                        channel(value, green),
                        channel(value, blue),
                        a,
                    ]
                }
            };
            rgba.extend_from_slice(&px);
        }
    }
    if alpha != 0 && rgba.chunks(4).all(|px| px[3] == 0) {
        for px in rgba.chunks_mut(4) {
            px[3] = 255;
        }
    }
    encode_png(&Rgba {
        width,
        height,
        pixels: rgba,
    })
}

/// Turns a PNG into a device independent bitmap with a `BITMAPV5HEADER`, as the Windows
/// clipboard holds `CF_DIBV5`: 32 bits a pixel, bottom row first, with masks for red, green,
/// blue and alpha, in sRGB. Windows makes `CF_DIB` and `CF_BITMAP` from it for programs that
/// ask for those.
pub fn png_to_dib(png: &[u8]) -> Result<Vec<u8>, String> {
    let image = decode_png(png)?;
    let (width, height) = (image.width as usize, image.height as usize);
    let size = (width * height * 4) as u32;
    let mut dib = Vec::with_capacity(124 + width * height * 4);
    let mut put = |value: u32| dib.extend_from_slice(&value.to_le_bytes());
    put(124); // bV5Size
    put(image.width); // bV5Width
    put(image.height); // bV5Height, positive: bottom row first
    put(1 | (32 << 16)); // bV5Planes = 1, bV5BitCount = 32
    put(BI_BITFIELDS); // bV5Compression
    put(size); // bV5SizeImage
    put(2835); // bV5XPelsPerMeter, 72 dpi
    put(2835); // bV5YPelsPerMeter
    put(0); // bV5ClrUsed
    put(0); // bV5ClrImportant
    put(0x00ff_0000); // bV5RedMask
    put(0x0000_ff00); // bV5GreenMask
    put(0x0000_00ff); // bV5BlueMask
    put(0xff00_0000); // bV5AlphaMask
    put(0x7352_4742); // bV5CSType, LCS_sRGB
    for _ in 0..9 {
        put(0); // bV5Endpoints
    }
    put(0); // bV5GammaRed
    put(0); // bV5GammaGreen
    put(0); // bV5GammaBlue
    put(4); // bV5Intent, LCS_GM_IMAGES
    put(0); // bV5ProfileData
    put(0); // bV5ProfileSize
    put(0); // bV5Reserved
    for y in (0..height).rev() {
        for px in image.pixels[y * width * 4..(y + 1) * width * 4].chunks(4) {
            dib.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Ok(dib)
}

/// What a host's clipboard holds now, by the rule every host follows: text when there is
/// text, and only otherwise an image. A program that copies both (a spreadsheet's cells, a
/// web page's selection) means the text, and the image is a picture of it.
pub fn choose(text: Option<String>, image: impl FnOnce() -> Option<Vec<u8>>) -> Option<Clip> {
    match text {
        Some(text) => Some(Clip::Text(text)),
        None => image().map(|png| Clip::Image(png.into())),
    }
}

/// The user's clipboard on a host, as [`sync`] uses it. Each host turns its own formats into
/// a [`Clip`] and back.
pub trait HostClipboard {
    /// What the clipboard holds now, if it holds text or an image (by [`choose`]).
    fn read(&mut self) -> Option<Clip>;
    /// Puts `clip` on the clipboard.
    fn write(&mut self, clip: &Clip) -> Result<(), String>;
}

/// How long [`sync`] waits for the service to answer.
const SYNC_ANSWER: std::time::Duration = std::time::Duration::from_secs(2);

/// How long [`sync`] waits before trying the service again.
const SYNC_RETRY: std::time::Duration = std::time::Duration::from_millis(500);

/// How long [`sync`] waits for a clip from Android before it looks for one copied on the host.
const SYNC_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Keeps the user's clipboard on a host and their Android's in step until `stop` is set, over
/// the service's local API, named `build` there. Every host runs it in the user's session.
///
/// `changes` carries each clip the user copies on the host. Each new connection to the service
/// first passes what the clipboard holds then, so the service has it for an Android that
/// starts later. A connection that breaks is made again; what is copied meanwhile is not lost,
/// since the next connection passes the clipboard's clip. A clip Android copied that the host
/// put on its clipboard is not sent back when the clipboard says it changed. A clip that may
/// not cross ([`Clip::check`]) is logged through `note` and left out. The service decides
/// whether anything crosses, from the user's clipboard setting.
pub fn sync(
    mut connector: Box<dyn dsa_transport::Connector>,
    build: &str,
    clipboard: &mut dyn HostClipboard,
    changes: std::sync::mpsc::Receiver<Clip>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    note: &dyn Fn(&str),
) {
    use std::sync::atomic::Ordering;
    let mut last_error = String::new();
    // What Android copied that the host put on the clipboard last.
    let mut from_android: Option<Clip> = None;
    let offer = |clip: &Clip| match clip.check("host") {
        Ok(()) => true,
        Err(why) => {
            note(&why);
            false
        }
    };
    let send = |stream: &mut crate::host_api::ClipboardStream, clip: &Clip| match clip {
        Clip::Text(text) => stream.send(text).is_ok(),
        Clip::Image(png) => match stream.send_image(png) {
            Ok(()) => true,
            Err(crate::host_api::ClientError::Malformed(why)) => {
                note(&format!(
                    "clipboard: dropped an image copied on the host: {why}"
                ));
                true
            }
            Err(_) => false,
        },
    };
    while !stop.load(Ordering::Relaxed) {
        let stream = crate::host_api::HostApiClient::connect(&mut *connector, build, SYNC_ANSWER)
            .and_then(|client| client.watch_clipboard());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(&format!("the clipboard cannot reach Android: {error}"));
                    last_error = error;
                }
                // What was copied meanwhile is on the clipboard for the next connection.
                while changes.try_recv().is_ok() {}
                std::thread::sleep(SYNC_RETRY);
                continue;
            }
        };
        last_error.clear();
        if let Some(clip) = clipboard
            .read()
            .filter(|clip| from_android.as_ref() != Some(clip))
        {
            if offer(&clip) && !send(&mut stream, &clip) {
                continue;
            }
        }
        'connected: while !stop.load(Ordering::Relaxed) {
            while let Ok(clip) = changes.try_recv() {
                if from_android.as_ref() == Some(&clip) || !offer(&clip) {
                    continue;
                }
                if !send(&mut stream, &clip) {
                    break 'connected;
                }
            }
            match stream.next_clip(SYNC_POLL) {
                Ok(Some(clip)) => {
                    if let Err(why) = clipboard.write(&clip) {
                        note(&format!(
                            "could not put what Android copied on the clipboard: {why}"
                        ));
                    }
                    from_android = Some(clip);
                }
                Ok(None) => {}
                Err(error) => {
                    note(&format!(
                        "the clipboard's connection to the service ended: {error}"
                    ));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: i32, height: i32, bits: u16, compression: u32) -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&width.to_le_bytes());
        dib.extend_from_slice(&height.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&bits.to_le_bytes());
        dib.extend_from_slice(&compression.to_le_bytes());
        dib.extend_from_slice(&[0; 20]);
        dib
    }

    fn pixels(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let image = decode_png(png).expect("a PNG");
        (image.width, image.height, image.pixels)
    }

    #[test]
    fn fr13_a_24_bit_bitmap_becomes_a_png_with_its_bottom_row_last() {
        let mut dib = header(2, 2, 24, BI_RGB);
        // Bottom row first: blue, green; then the top row: red, white. Rows pad to 4 bytes.
        dib.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        dib.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
        let (width, height, rgba) = pixels(&dib_to_png(&dib).unwrap());
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            rgba,
            vec![
                255, 0, 0, 255, 255, 255, 255, 255, // top: red, white
                0, 0, 255, 255, 0, 255, 0, 255, // bottom: blue, green
            ]
        );
    }

    #[test]
    fn fr13_a_top_down_bitmap_keeps_its_order() {
        let mut dib = header(1, -2, 24, BI_RGB);
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        let (_, _, rgba) = pixels(&dib_to_png(&dib).unwrap());
        assert_eq!(rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn fr13_a_32_bit_bitmap_with_no_alpha_at_all_is_opaque() {
        let mut dib = header(1, 1, 32, BI_RGB);
        dib.extend_from_slice(&[10, 20, 30, 0]);
        let (_, _, rgba) = pixels(&dib_to_png(&dib).unwrap());
        assert_eq!(rgba, vec![30, 20, 10, 255]);
    }

    #[test]
    fn fr13_a_palette_bitmap_goes_through_its_palette() {
        let mut dib = header(8, 1, 1, BI_RGB);
        dib.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
        dib.extend_from_slice(&[0b1000_0001, 0, 0, 0]);
        let (_, _, rgba) = pixels(&dib_to_png(&dib).unwrap());
        assert_eq!(&rgba[0..4], &[255, 255, 255, 255]);
        assert_eq!(&rgba[4..8], &[0, 0, 0, 255]);
        assert_eq!(&rgba[28..32], &[255, 255, 255, 255]);
    }

    #[test]
    fn fr13_a_png_becomes_a_v5_bitmap_and_back_with_its_alpha() {
        let png = encode_png(&Rgba {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 128, 200, 100, 50, 255],
        })
        .unwrap();
        let dib = png_to_dib(&png).unwrap();
        assert_eq!(u32_at(&dib, 0), Some(124));
        assert_eq!(&dib[124..], &[3, 2, 1, 128, 50, 100, 200, 255]);
        let (_, _, rgba) = pixels(&dib_to_png(&dib).unwrap());
        assert_eq!(rgba, vec![1, 2, 3, 128, 200, 100, 50, 255]);
    }

    #[test]
    fn fr13_a_compressed_or_cut_bitmap_is_refused() {
        let mut rle = header(1, 1, 8, 1);
        rle.extend_from_slice(&[0; 8]);
        assert!(dib_to_png(&rle).is_err());
        let cut = header(100, 100, 24, BI_RGB);
        assert!(dib_to_png(&cut).is_err());
        assert!(dib_to_png(&[1, 2, 3]).is_err());
    }

    #[test]
    fn fr13_an_image_larger_than_the_cap_does_not_cross() {
        let png = encode_png(&Rgba {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 255],
        })
        .unwrap();
        assert!(check_image(&png).is_ok());
        let mut long = png.clone();
        long.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(check_image(&long).is_err());
        assert!(check_image(b"GIF89a").is_err());
        let mut assembler = ImageAssembler::default();
        let piece = control::ClipboardImage {
            clip_id: 1,
            size: MAX_IMAGE_BYTES as u64 + 1,
            offset: 0,
            data: vec![0; 10],
        };
        assert!(assembler.take(piece.clone()).is_err());
        // The rest of a refused clip is dropped without another word.
        assert_eq!(assembler.take(piece), Ok(None));
    }

    #[test]
    fn fr13_pieces_put_back_together_are_the_image() {
        let png: Vec<u8> = (0..IMAGE_PIECE_BYTES * 2 + 7).map(|i| i as u8).collect();
        let pieces: Vec<_> = image_pieces(3, &png).collect();
        assert_eq!(pieces.len(), 3);
        let mut assembler = ImageAssembler::default();
        let mut whole = None;
        for piece in pieces {
            assert!(piece.data.len() <= IMAGE_PIECE_BYTES);
            whole = assembler.take(piece).unwrap();
        }
        assert_eq!(whole, Some(png));
    }

    #[test]
    fn fr13_a_new_clip_drops_one_that_was_not_whole() {
        let png = vec![7u8; IMAGE_PIECE_BYTES + 1];
        let mut first = image_pieces(1, &png);
        let mut assembler = ImageAssembler::default();
        assert_eq!(assembler.take(first.next().unwrap()), Ok(None));
        let other = vec![9u8; 5];
        let piece = image_pieces(2, &other).next().unwrap();
        assert_eq!(assembler.take(piece), Ok(Some(other)));
    }
}

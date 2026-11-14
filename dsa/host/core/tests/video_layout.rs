//! Where a decoded picture lies in a hardware decoder's padded surface: the chroma plane's
//! start, the visible size, and the sizes VP9 and AV1 headers carry.

use dsa_host_core::video::Codec;
use dsa_host_core::video_layout::{
    av1_frame_size, chroma_start_row, mapped_chroma_row, picture_from_surface,
    picture_from_surface_area, rows_from_length, staging_rows, stream_frame_size, visible_area,
    visible_size, vp9_frame_size, Area, StreamSizes, SurfaceFormat,
};

const Y: u8 = 81;
const U: u8 = 90;
const V: u8 = 240;

/// An NV12 surface as a hardware decoder leaves it: `width` by `height` visible, rows `pitch`
/// bytes apart, `rows` rows of luma (the rest below the picture zero), then the chroma plane.
fn surface(width: usize, height: usize, pitch: usize, rows: usize) -> Vec<u8> {
    let mut data = vec![0u8; pitch * (rows + rows.div_ceil(2))];
    for row in 0..height {
        data[row * pitch..row * pitch + width].fill(Y);
    }
    let chroma = rows * pitch;
    for row in 0..height.div_ceil(2) {
        for x in 0..width.div_ceil(2) {
            data[chroma + row * pitch + 2 * x] = U;
            data[chroma + row * pitch + 2 * x + 1] = V;
        }
    }
    data
}

fn chroma_of(nv12: &[u8], stride: usize, height: usize) -> &[u8] {
    &nv12[stride * height..]
}

#[test]
fn fr26_the_chroma_plane_starts_after_the_surfaces_own_rows() {
    // A 360x640 picture, coded 368x640, in a surface the driver made 1088 rows tall.
    assert_eq!(chroma_start_row(Some(1088), 640, 640), 1088);
    // Without the surface's height, the coded height, never less than the picture's.
    assert_eq!(chroma_start_row(None, 368, 360), 368);
    assert_eq!(chroma_start_row(None, 0, 240), 240);
    // A surface height smaller than the picture is not one.
    assert_eq!(chroma_start_row(Some(120), 256, 240), 256);
}

#[test]
fn fr26_a_staging_surface_is_a_whole_number_of_aligned_rows() {
    assert_eq!(staging_rows(360), 384);
    assert_eq!(staging_rows(368), 384);
    assert_eq!(staging_rows(640), 640);
    assert_eq!(staging_rows(1088), 1088);
    assert_eq!(staging_rows(1), 64);
    assert_eq!(staging_rows(0), 64);
}

#[test]
fn fr26_a_mapping_that_says_it_is_taller_moves_the_chroma() {
    let pitch = 640;
    // A 4:2:0 mapping of exactly the staging rows.
    assert_eq!(mapped_chroma_row(384, pitch, pitch * 576), 384);
    // A driver that padded the luma plane further says so in the mapping's size.
    assert_eq!(mapped_chroma_row(384, pitch, pitch * (448 + 224)), 448);
    // Sizes that say nothing about the chroma: only the luma plane, none, a page-rounded one,
    // and one that would put the chroma before the staging rows.
    assert_eq!(mapped_chroma_row(384, pitch, pitch * 384), 384);
    assert_eq!(mapped_chroma_row(384, pitch, 0), 384);
    assert_eq!(mapped_chroma_row(384, pitch, pitch * 576 + 4096), 384);
    assert_eq!(mapped_chroma_row(384, pitch, pitch * 540), 384);
    assert_eq!(mapped_chroma_row(384, 0, pitch * 576), 384);
}

#[test]
fn fr26_the_top_rows_keep_their_colour_when_the_driver_pads_the_surface() {
    // A 640x360 picture in a 360-row surface whose driver maps it with its chroma at row 384,
    // the padding rows below the picture holding what the decoder left there.
    let (width, height, pitch, padded) = (640usize, 360usize, 640usize, 384usize);
    let mut data = surface(width, height, pitch, padded);
    for row in height..padded {
        for x in 0..width {
            data[row * pitch + x] = if (x / 16 + row) % 2 == 0 { 16 } else { 235 };
        }
    }
    // Read as the surface's own height says, the first chroma rows are that padding: the
    // picture's top 48 rows showed as blocks of green and purple.
    let wrong = picture_from_surface(1, &data, SurfaceFormat::Nv12, pitch, height, 640, 360)
        .expect("a picture");
    let wrong_chroma = chroma_of(&wrong.nv12, 640, 360);
    assert!(wrong_chroma[..640].iter().any(|&c| c != U && c != V));
    // Read through a staging surface of whole aligned rows, every chroma row is the picture's.
    let rows = mapped_chroma_row(staging_rows(height), pitch, data.len());
    assert_eq!(rows, padded);
    let picture = picture_from_surface(1, &data, SurfaceFormat::Nv12, pitch, rows, 640, 360)
        .expect("a picture");
    let chroma = chroma_of(&picture.nv12, 640, 360);
    assert_eq!(chroma.len(), 640 * 180);
    for (i, pair) in chroma.chunks(2).enumerate() {
        assert_eq!(
            pair,
            [U, V],
            "chroma pair {i} (row {}) is not the picture's",
            i / 320
        );
    }
    assert!(picture.nv12[..640 * 360].iter().all(|&y| y == Y));
}

#[test]
fn fr26_a_copy_reaches_the_far_edges_of_the_area_in_whole_pairs() {
    assert_eq!(Area::at_corner((640, 360)).reach(), (640, 360));
    let cropped = Area {
        left: 2,
        top: 4,
        width: 425,
        height: 239,
    };
    assert_eq!(cropped.reach(), (428, 244));
}

#[test]
fn fr26_a_picture_in_a_taller_surface_keeps_its_chroma() {
    // 426x240, coded 432x240, in a surface rounded up to 256 rows.
    let (width, height, pitch, rows) = (426, 240, 512, 256);
    let data = surface(width, height, pitch, rows);
    let start = chroma_start_row(Some(rows), 240, height);
    let picture = picture_from_surface(1, &data, SurfaceFormat::Nv12, pitch, start, 426, 240)
        .expect("a picture");
    assert_eq!(
        (picture.width, picture.height, picture.stride),
        (426, 240, 426)
    );
    assert!(picture.nv12[..426 * 240].iter().all(|&y| y == Y));
    let chroma = chroma_of(&picture.nv12, 426, 240);
    assert_eq!(chroma.len(), 426 * 120);
    assert!(chroma.chunks(2).all(|pair| pair == [U, V]), "chroma lost");

    // Taking the chroma after the coded rows reads the zero padding: a green picture.
    let wrong = picture_from_surface(1, &data, SurfaceFormat::Nv12, pitch, 240, 426, 240).unwrap();
    assert!(chroma_of(&wrong.nv12, 426, 240)[..426 * 8]
        .iter()
        .all(|&c| c == 0));
}

#[test]
fn fr26_an_odd_width_keeps_whole_chroma_pairs() {
    let data = surface(5, 3, 8, 4);
    let picture = picture_from_surface(1, &data, SurfaceFormat::Nv12, 8, 4, 5, 3).unwrap();
    assert_eq!(picture.stride, 6);
    assert_eq!(picture.nv12.len(), 6 * (3 + 2));
    assert_eq!(&chroma_of(&picture.nv12, 6, 3)[..6], &[U, V, U, V, U, V]);
}

#[test]
fn fr26_a_surface_too_short_for_its_geometry_gives_no_picture() {
    let data = surface(4, 2, 8, 4);
    assert!(picture_from_surface(1, &data[..10], SurfaceFormat::Nv12, 8, 4, 4, 2).is_none());
    assert!(picture_from_surface(1, &data, SurfaceFormat::Nv12, 2, 4, 4, 2).is_none());
    assert!(picture_from_surface(1, &data, SurfaceFormat::Nv12, 8, 1, 4, 2).is_none());
    assert!(picture_from_surface(1, &data, SurfaceFormat::Nv12, 8, 64, 4, 2).is_none());
}

#[test]
fn fr26_a_10_bit_surface_becomes_8_bit_nv12() {
    // 2x2 P010: each sample two bytes, little-endian, its value in the top bits.
    let pitch = 8;
    let mut data = vec![0u8; pitch * 3];
    for row in 0..2 {
        for x in 0..2 {
            data[row * pitch + 2 * x + 1] = Y;
        }
    }
    data[2 * pitch + 1] = U;
    data[2 * pitch + 3] = V;
    let picture = picture_from_surface(1, &data, SurfaceFormat::P010, pitch, 2, 2, 2).unwrap();
    assert_eq!(picture.nv12, vec![Y, Y, Y, Y, U, V]);
}

#[test]
fn fr26_a_surfaces_height_follows_from_its_length() {
    assert_eq!(rows_from_length(512 * (256 + 128), 512), Some(256));
    assert_eq!(rows_from_length(8 * (5 + 3), 8), Some(5));
    assert_eq!(rows_from_length(100, 8), None);
    assert_eq!(rows_from_length(8 * 4, 8), None);
}

#[test]
fn fr26_the_visible_size_is_the_aperture_then_the_streams_own() {
    // H.264 360x640: coded 368x640, cropped by the aperture.
    assert_eq!(visible_size((368, 640), Some((360, 640)), None), (360, 640));
    // VP9 426x240: no aperture, the frame header's size.
    assert_eq!(visible_size((432, 256), None, Some((426, 240))), (426, 240));
    // A size larger than the surface is not believed.
    assert_eq!(
        visible_size((432, 256), Some((1920, 1080)), None),
        (432, 256)
    );
    assert_eq!(
        visible_size((432, 256), Some((0, 240)), Some((426, 240))),
        (426, 240)
    );
    assert_eq!(visible_size((640, 368), None, None), (640, 368));
}

#[test]
fn fr26_a_picture_cropped_at_its_top_left_is_copied_from_its_offset() {
    // A 4x2 picture at column 2, row 2 of an 8x6 surface whose other samples are padding (0).
    let (pitch, rows) = (8, 6);
    let mut data = vec![0u8; pitch * (rows + rows / 2)];
    for row in 2..4 {
        data[row * pitch + 2..row * pitch + 6].fill(Y);
    }
    // Chroma row 1 covers luma rows 2 and 3; columns 2..6 are pairs 1 and 2.
    let chroma = (rows + 1) * pitch;
    data[chroma + 2..chroma + 6].copy_from_slice(&[U, V, U, V]);
    let area = visible_area(
        (8, 6),
        Some(Area {
            left: 2,
            top: 2,
            width: 4,
            height: 2,
        }),
        None,
    );
    assert_eq!((area.left, area.top, area.width, area.height), (2, 2, 4, 2));
    let picture =
        picture_from_surface_area(1, &data, SurfaceFormat::Nv12, pitch, rows, area).unwrap();
    // Before, the size was taken at the corner: padding (green) along the top and left.
    assert_eq!(picture.nv12, vec![Y, Y, Y, Y, Y, Y, Y, Y, U, V, U, V]);
    // An area that starts off a chroma pair is moved onto one, keeping its other edges.
    let odd = visible_area(
        (8, 6),
        Some(Area {
            left: 3,
            top: 1,
            width: 3,
            height: 3,
        }),
        None,
    );
    assert_eq!((odd.left, odd.top, odd.width, odd.height), (2, 0, 4, 4));
    assert!(picture_from_surface_area(
        1,
        &data,
        SurfaceFormat::Nv12,
        pitch,
        rows,
        Area {
            left: 3,
            top: 2,
            width: 4,
            height: 2
        }
    )
    .is_none());
    // An area past the coded size is not believed.
    let past = visible_area(
        (8, 6),
        Some(Area {
            left: 6,
            top: 0,
            width: 4,
            height: 2,
        }),
        Some((8, 4)),
    );
    assert_eq!((past.left, past.top, past.width, past.height), (0, 0, 8, 4));
}

#[test]
fn fr26_a_pictures_size_follows_the_header_of_the_packet_it_came_from() {
    // A stream changes size at packet 10 while the decoder still holds pictures of 4..9.
    let mut sizes = StreamSizes::default();
    assert_eq!(sizes.for_picture(3), None);
    sizes.record(0, (1920, 1080));
    sizes.record(10, (1280, 720));
    // Before, every picture took the newest header: the old ones cut to 1280x720.
    assert_eq!(sizes.for_picture(9), Some((1920, 1080)));
    assert_eq!(sizes.for_picture(10), Some((1280, 720)));
    assert_eq!(sizes.for_picture(42), Some((1280, 720)));
    // Only the latest headers are kept.
    for id in 100..200 {
        sizes.record(id, (640, 360));
    }
    assert_eq!(sizes.for_picture(250), Some((640, 360)));
    assert_eq!(sizes.for_picture(5), None);
}

/// Writes bits most significant first.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            if self.bits % 8 == 0 {
                self.bytes.push(0);
            }
            if (value >> i) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

/// A VP9 profile 0 key frame's uncompressed header for `width` by `height`.
fn vp9_key_frame(width: u32, height: u32) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.put(2, 2); // frame_marker
    w.put(0, 2); // profile 0
    w.put(0, 1); // show_existing_frame
    w.put(0, 1); // key frame
    w.put(1, 1); // show_frame
    w.put(0, 1); // error_resilient_mode
    w.put(0x49_83_42, 24);
    w.put(2, 3); // color_space BT.709
    w.put(0, 1); // color_range
    w.put(width - 1, 16);
    w.put(height - 1, 16);
    w.put(0, 1); // render size as the frame's
    w.bytes.extend_from_slice(&[0; 8]);
    w.bytes
}

#[test]
fn fr26_a_vp9_key_frame_gives_its_size() {
    assert_eq!(vp9_frame_size(&vp9_key_frame(426, 240)), Some((426, 240)));
    assert_eq!(
        stream_frame_size(Codec::Vp9, &vp9_key_frame(360, 640)),
        Some((360, 640))
    );
    assert_eq!(
        stream_frame_size(Codec::H264, &vp9_key_frame(360, 640)),
        None
    );
}

#[test]
fn fr26_a_vp9_superframe_gives_its_first_frames_size() {
    let first = vp9_key_frame(426, 240);
    let second = vec![0x86, 0x00, 0x40];
    let marker = 0xc0 | (1 << 3) | 1; // two frames, two bytes each
    let mut packet = first.clone();
    packet.extend_from_slice(&second);
    packet.push(marker);
    packet.extend_from_slice(&(first.len() as u16).to_le_bytes());
    packet.extend_from_slice(&(second.len() as u16).to_le_bytes());
    packet.push(marker);
    assert_eq!(vp9_frame_size(&packet), Some((426, 240)));
}

#[test]
fn fr26_a_vp9_inter_frame_gives_no_size() {
    let mut w = BitWriter::default();
    w.put(2, 2);
    w.put(0, 2);
    w.put(0, 1);
    w.put(1, 1); // inter frame
    w.put(1, 1); // shown, so not intra-only
    w.put(0, 1);
    w.bytes.extend_from_slice(&[0; 8]);
    assert_eq!(vp9_frame_size(&w.bytes), None);
    assert_eq!(vp9_frame_size(&[]), None);
}

/// An AV1 sequence header OBU for `width` by `height`, after a temporal delimiter.
fn av1_temporal_unit(width: u32, height: u32) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.put(0, 3); // seq_profile
    w.put(0, 1); // still_picture
    w.put(0, 1); // reduced_still_picture_header
    w.put(0, 1); // timing_info_present_flag
    w.put(0, 1); // initial_display_delay_present_flag
    w.put(0, 5); // operating_points_cnt_minus_1
    w.put(0, 12); // operating_point_idc
    w.put(8, 5); // seq_level_idx 4.0
    w.put(0, 1); // seq_tier
    w.put(9, 4); // frame_width_bits_minus_1
    w.put(10, 4); // frame_height_bits_minus_1
    w.put(width - 1, 10);
    w.put(height - 1, 11);
    w.put(0, 8);
    let mut unit = vec![0x12, 0x00];
    unit.push((1 << 3) | 0x2);
    unit.push(w.bytes.len() as u8);
    unit.extend_from_slice(&w.bytes);
    unit
}

#[test]
fn fr26_an_av1_sequence_header_gives_its_size() {
    assert_eq!(
        av1_frame_size(&av1_temporal_unit(426, 240)),
        Some((426, 240))
    );
    assert_eq!(
        stream_frame_size(Codec::Av1, &av1_temporal_unit(360, 640)),
        Some((360, 640))
    );
    // The codec configuration record Android passes: four bytes, then the OBUs.
    let mut record = vec![0x81, 0x00, 0x0c, 0x00];
    record.extend_from_slice(&av1_temporal_unit(360, 640)[2..]);
    assert_eq!(av1_frame_size(&record), Some((360, 640)));
    // A temporal unit with no sequence header.
    assert_eq!(av1_frame_size(&[0x12, 0x00, 0x32, 0x01, 0x00]), None);
}

//! The copy of a decoded picture out of the decoder's padded surface.

use dsa_host_windows::video_decoder::picture_from_planes;

/// A 4x2 visible picture in a surface of pitch 8 and 4 rows: luma rows are 10, 11 (then
/// padding rows), chroma row 20 starts after 4 rows.
fn surface() -> Vec<u8> {
    let pitch = 8;
    let mut data = vec![0xee; pitch * (4 + 2)];
    data[..4].copy_from_slice(&[10, 10, 10, 10]);
    data[8..12].copy_from_slice(&[11, 11, 11, 11]);
    data[32..36].copy_from_slice(&[20, 21, 20, 21]);
    data
}

#[test]
fn pr4_a_picture_is_its_visible_rows_of_both_planes() {
    let picture = picture_from_planes(5, &surface(), 8, 4, 4, 2).expect("a picture");
    assert_eq!(picture.id, 5);
    assert_eq!((picture.width, picture.height, picture.stride), (4, 2, 4));
    assert_eq!(
        picture.nv12,
        vec![10, 10, 10, 10, 11, 11, 11, 11, 20, 21, 20, 21]
    );
}

#[test]
fn pr4_an_odd_width_keeps_whole_chroma_pairs() {
    let picture = picture_from_planes(1, &surface(), 8, 4, 3, 2).expect("a picture");
    assert_eq!(picture.stride, 4);
    assert_eq!(picture.nv12.len(), 4 * 3);
}

#[test]
fn pr4_a_surface_too_short_for_its_size_gives_no_picture() {
    assert!(picture_from_planes(1, &[0; 10], 8, 4, 4, 2).is_none());
    assert!(picture_from_planes(1, &surface(), 2, 4, 4, 2).is_none());
    assert!(picture_from_planes(1, &surface(), 8, 1, 4, 2).is_none());
}

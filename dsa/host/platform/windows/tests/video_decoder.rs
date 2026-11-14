//! The copy of a decoded picture out of the decoder's padded surface.

use dsa_host_core::video::Codec;
use dsa_host_windows::video_decoder::{parse_probe, picture_from_planes, probe_line};

#[test]
fn nfr1_the_video_probe_report_round_trips_the_formats_the_gpu_decodes() {
    let text = [
        probe_line(Codec::H264, &Ok(())),
        probe_line(Codec::Hevc, &Err("no decoder\nhere".into())),
        probe_line(Codec::Av1, &Ok(())),
        "garbage".to_string(),
    ]
    .join("\n");
    assert_eq!(parse_probe(&text), vec![Codec::H264, Codec::Av1]);
    assert_eq!(probe_line(Codec::Hevc, &Err("a\nb".into())), "no HEVC: a b");
}

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

#[cfg(windows)]
#[test]
fn nfr2_the_decoders_let_their_device_go_with_the_last_of_them() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use dsa_host_core::video::VideoDecoders;
    use dsa_host_windows::video_decoder::MediaFoundationDecoders;

    let let_go = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&let_go);
    let decoders = MediaFoundationDecoders::lazy(
        Box::new(|_: &str| {}),
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let first = match decoders.open(Codec::H264, 1280, 720) {
        Ok(decoder) => decoder,
        Err(why) => {
            eprintln!("skipped: this machine decodes no H.264 on a GPU ({why})");
            return;
        }
    };
    let second = decoders
        .open(Codec::H264, 1280, 720)
        .expect("a second decoder shares the first one's device");
    drop(first);
    // One decoder still open: what they share stays.
    decoders.all_closed();
    assert_eq!(let_go.load(Ordering::SeqCst), 0);
    drop(second);
    decoders.all_closed();
    assert_eq!(let_go.load(Ordering::SeqCst), 1);
    // Nothing opened since: nothing more to let go.
    decoders.all_closed();
    assert_eq!(let_go.load(Ordering::SeqCst), 1);
    // A decoder opened after that makes a new device, and lets it go in turn.
    let again = decoders
        .open(Codec::H264, 1280, 720)
        .expect("a decoder opens again after the device was let go");
    drop(again);
    decoders.all_closed();
    assert_eq!(let_go.load(Ordering::SeqCst), 2);
}

//! How a screenshot is named, whichever host takes it.

use std::path::Path;

use dsa_host_core::screenshot::{app_content, file_name, free_path, LocalTime, PixelOrder};

const TIME: LocalTime = LocalTime {
    year: 2026,
    month: 11,
    day: 8,
    hour: 9,
    minute: 5,
    second: 3,
};

#[test]
fn fr22_a_screenshot_is_named_as_the_snipping_tool_names_its_own() {
    assert_eq!(
        file_name("Screenshot", &TIME, 1),
        "Screenshot 2026-11-08 090503.png"
    );
    assert_eq!(
        file_name("\u{c2a4}\u{d06c}\u{b9b0}\u{c0f7}", &TIME, 2),
        "\u{c2a4}\u{d06c}\u{b9b0}\u{c0f7} 2026-11-08 090503 (2).png"
    );
}

#[test]
fn fr22_a_name_taken_goes_to_the_next_copy() {
    let folder = Path::new("shots");
    let taken = [
        folder.join("Screenshot 2026-11-08 090503.png"),
        folder.join("Screenshot 2026-11-08 090503 (2).png"),
    ];
    assert_eq!(
        free_path(folder, "Screenshot", &TIME, |path| taken
            .iter()
            .any(|t| t == path)),
        folder.join("Screenshot 2026-11-08 090503 (3).png")
    );
    assert_eq!(free_path(folder, "Screenshot", &TIME, |_| false), taken[0]);
}

#[test]
fn fr22_a_screenshot_is_the_app_content_without_the_band() {
    // A 2x3 frame in a surface 3 pixels wide, BGRA, the top row the band.
    let stride = 12;
    let mut pixels = vec![0u8; stride * 3];
    for y in 0..3 {
        for x in 0..2 {
            let at = y * stride + x * 4;
            pixels[at..at + 4].copy_from_slice(&[x as u8, y as u8, 9, 0]);
        }
    }
    let shot = app_content(&pixels, stride, (2, 3), 1, PixelOrder::Bgra).unwrap();
    assert_eq!((shot.width, shot.height), (2, 2));
    // RGBA, opaque, from the row under the band.
    assert_eq!(&shot.rgba[..4], &[9, 1, 0, 0xFF]);
    assert_eq!(&shot.rgba[4..8], &[9, 1, 1, 0xFF]);
    assert_eq!(&shot.rgba[8..12], &[9, 2, 0, 0xFF]);
    // No band: the whole frame.
    let whole = app_content(&pixels, stride, (2, 3), 0, PixelOrder::Bgra);
    assert_eq!(whole.unwrap().height, 3);
    // Nothing under the band, or too few pixels.
    assert_eq!(
        app_content(&pixels, stride, (2, 3), 3, PixelOrder::Bgra),
        None
    );
    assert_eq!(
        app_content(&pixels[..20], stride, (2, 3), 0, PixelOrder::Bgra),
        None
    );
}

#[test]
fn fr22_a_frame_of_rgba_rows_keeps_its_colors_and_gains_opacity() {
    let pixels = [1, 2, 3, 0, 4, 5, 6, 0];
    let shot = app_content(&pixels, 8, (2, 1), 0, PixelOrder::Rgba).unwrap();
    assert_eq!(shot.rgba, vec![1, 2, 3, 0xFF, 4, 5, 6, 0xFF]);
}

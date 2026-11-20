//! Colors as the hosts draw them.

/// `color` (alpha in the top byte, then blue, green and red from the lowest) at `coverage` (0
/// to 1, clamped), with each of blue, green and red multiplied by the resulting alpha, as a
/// premultiplied surface holds it. Each of the four values is on the scale 0 to 255, not yet
/// rounded: blue, green, red, then alpha.
pub fn premultiplied(color: u32, coverage: f64) -> [f64; 4] {
    let alpha = f64::from(color >> 24) / 255.0 * coverage.clamp(0.0, 1.0);
    [
        f64::from(color & 0xff) * alpha,
        f64::from((color >> 8) & 0xff) * alpha,
        f64::from((color >> 16) & 0xff) * alpha,
        alpha * 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::premultiplied;

    #[test]
    fn d9_an_opaque_color_keeps_its_channels_and_a_half_covered_one_halves_them() {
        assert_eq!(
            premultiplied(0xff10_2030, 1.0),
            [0x30 as f64, 0x20 as f64, 0x10 as f64, 255.0]
        );
        assert_eq!(
            premultiplied(0xffff_ffff, 0.5),
            [127.5, 127.5, 127.5, 127.5]
        );
    }

    #[test]
    fn d9_coverage_outside_zero_to_one_is_clamped_and_a_clear_color_stays_clear() {
        assert!((premultiplied(0x80ff_ffff, 2.0)[3] - 128.0).abs() < 1e-9);
        assert_eq!(premultiplied(0xffff_ffff, -1.0), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(premultiplied(0x00ff_ffff, 1.0), [0.0, 0.0, 0.0, 0.0]);
    }
}

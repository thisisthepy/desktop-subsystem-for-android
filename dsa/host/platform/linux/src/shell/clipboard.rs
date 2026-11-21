//! The clipboard formats that carry text and images, as `wl_data_offer` names them.
//!
//! The host offers text the guest copied under every name a Linux program looks for text by,
//! and takes text from an offer under the first of those it has, preferring UTF-8. Images
//! cross as PNG (`image/png`), both ways; an offer with text and an image gives its text.

use dsa_host_core::clipboard;

/// The names text goes by, best first.
pub const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// The type to read text from an offer that has `offered`; `None` when it has no text.
pub fn text_type(offered: &[String]) -> Option<&str> {
    TEXT_TYPES.iter().find_map(|wanted| {
        offered
            .iter()
            .find(|offered| offered.eq_ignore_ascii_case(wanted))
            .map(String::as_str)
    })
}

/// The text a desktop program copied, as the guest takes it: `None` when it is not UTF-8 or
/// too long for the guest's clipboard.
pub fn host_text(bytes: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(bytes).ok()?;
    clipboard::fits(&text).then_some(text)
}

/// The name images go by: they cross as PNG.
pub const IMAGE_TYPE: &str = "image/png";

/// What to read from an offer that has `offered`, by the rule every host follows
/// ([`clipboard::choose`]): its text when it has text, otherwise its PNG. The type, and
/// whether it is the image.
pub fn selection_type(offered: &[String]) -> Option<(&str, bool)> {
    if let Some(text) = text_type(offered) {
        return Some((text, false));
    }
    offered
        .iter()
        .find(|offered| offered.eq_ignore_ascii_case(IMAGE_TYPE))
        .map(|image| (image.as_str(), true))
}

/// The image a desktop program copied, as the guest takes it: `None` when it is no PNG the
/// guest may take (longer than the cap, or unreadable), which is logged.
pub fn host_image(bytes: Vec<u8>) -> Option<Vec<u8>> {
    match clipboard::check_image(&bytes) {
        Ok(()) => Some(bytes),
        Err(why) => {
            eprintln!("clipboard: dropped an image copied on the desktop: {why}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr13_text_is_read_in_utf8_when_the_program_offers_it() {
        let offered = vec![
            "text/html".to_string(),
            "STRING".into(),
            "UTF8_STRING".into(),
        ];
        assert_eq!(text_type(&offered), Some("UTF8_STRING"));
        let offered = vec!["TEXT/PLAIN;CHARSET=UTF-8".to_string()];
        assert_eq!(text_type(&offered), Some("TEXT/PLAIN;CHARSET=UTF-8"));
        assert_eq!(text_type(&["image/png".to_string()]), None);
    }

    #[test]
    fn fr13_an_image_is_read_as_png_only_when_there_is_no_text() {
        let both = vec!["image/png".to_string(), "UTF8_STRING".into()];
        assert_eq!(selection_type(&both), Some(("UTF8_STRING", false)));
        let image = vec!["text/html".to_string(), "IMAGE/PNG".into()];
        assert_eq!(selection_type(&image), Some(("IMAGE/PNG", true)));
        assert_eq!(selection_type(&["image/jpeg".to_string()]), None);
    }

    #[test]
    fn fr13_an_image_larger_than_the_cap_does_not_cross() {
        let mut dib = vec![0u8; 40];
        dib[0] = 40;
        dib[4] = 1;
        dib[8] = 1;
        dib[12] = 1;
        dib[14] = 24;
        dib.extend_from_slice(&[0, 0, 255, 0]);
        let png = clipboard::dib_to_png(&dib).unwrap();
        assert_eq!(host_image(png.clone()), Some(png.clone()));
        let mut huge = png;
        huge.resize(clipboard::MAX_IMAGE_BYTES + 1, 0);
        assert_eq!(host_image(huge), None);
        assert_eq!(host_image(b"not a png".to_vec()), None);
    }

    #[test]
    fn fr13_text_too_long_for_the_guest_or_not_utf8_is_left_out() {
        assert_eq!(host_text(b"hello".to_vec()), Some("hello".into()));
        assert_eq!(host_text(vec![0xff, 0xfe]), None);
        assert_eq!(
            host_text(vec![b'a'; clipboard::MAX_CLIPBOARD_BYTES + 1]),
            None
        );
    }
}

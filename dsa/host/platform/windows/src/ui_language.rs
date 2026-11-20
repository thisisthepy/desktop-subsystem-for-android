//! The language the window manager, the app windows and DSA's other Windows programs show
//! their own words in: the user's Windows display language when DSA has a translation for it,
//! else English.
//!
//! [`user_language`] reads the user's preferred display languages
//! (`GetUserPreferredUILanguages`, as names such as `ko-KR` or `zh-Hant-HK`), most preferred
//! first, and takes the first one DSA has; when Windows gives no names it maps the display
//! language's ID (`GetUserDefaultUILanguage`) through [`from_langid`]. Every menu, notice,
//! accessibility name and prompt of the Windows host asks it, so they all speak one language.

pub use dsa_host_core::ui_language::UiLanguage;

/// The interface language for a Windows language ID (LANGID), English when DSA has no
/// translation for it.
///
/// The primary language is the low ten bits. Chinese follows its sublanguage: Taiwan (1),
/// Hong Kong (3), Macao (5) and the Traditional neutral ID 0x7C04 are Traditional Chinese,
/// every other Chinese ID Simplified. Portuguese is Brazilian Portuguese in every region.
pub fn from_langid(langid: u16) -> UiLanguage {
    const ZH_HANT: u16 = 0x7C04;
    let primary = langid & 0x3FF;
    let sublanguage = langid >> 10;
    match primary {
        0x09 => UiLanguage::English,
        0x12 => UiLanguage::Korean,
        0x11 => UiLanguage::Japanese,
        0x04 => {
            if langid == ZH_HANT || matches!(sublanguage, 1 | 3 | 5) {
                UiLanguage::ChineseTraditional
            } else {
                UiLanguage::ChineseSimplified
            }
        }
        0x0A => UiLanguage::Spanish,
        0x0C => UiLanguage::French,
        0x07 => UiLanguage::German,
        0x16 => UiLanguage::PortugueseBrazil,
        0x19 => UiLanguage::Russian,
        0x10 => UiLanguage::Italian,
        0x2A => UiLanguage::Vietnamese,
        0x21 => UiLanguage::Indonesian,
        0x1F => UiLanguage::Turkish,
        _ => UiLanguage::English,
    }
}

/// The interface language for the user's preferred display language names, most preferred
/// first: the first one DSA has a translation for, else `fallback` when Windows gave no names.
/// A list of names none of which DSA has is English.
pub fn from_preferred(names: &[String], fallback: impl FnOnce() -> UiLanguage) -> UiLanguage {
    if names.iter().all(|name| name.trim().is_empty()) {
        return fallback();
    }
    UiLanguage::first_supported(names.iter().map(String::as_str))
}

/// The language the user reads Windows in, as DSA shows its own words: the first of their
/// preferred display languages DSA has, else English.
#[cfg(windows)]
pub fn user_language() -> UiLanguage {
    from_preferred(&preferred_names(), || {
        // SAFETY: reads the user's display language.
        from_langid(unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() })
    })
}

/// English, where there is no Windows display language to read.
#[cfg(not(windows))]
pub fn user_language() -> UiLanguage {
    UiLanguage::English
}

/// The user's preferred display languages as names (`ko-KR`), most preferred first; empty
/// when Windows cannot say.
#[cfg(windows)]
fn preferred_names() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let mut count = 0u32;
    let mut length = 0u32;
    // SAFETY: asks for the length only.
    let sized = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut length,
        )
    };
    if sized == 0 || length == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: a buffer of the length Windows asked for.
    let read = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            buffer.as_mut_ptr(),
            &mut length,
        )
    };
    if read == 0 {
        return Vec::new();
    }
    buffer
        .split(|unit| *unit == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

pub use dsa_host_core::ui_text::fill;

/// What is wrong with `text`, the `language` translation of `english` named `what`: empty,
/// an em dash, or other `{}` placeholders than the English one. For the tests of every text.
pub fn translation_problem(
    language: UiLanguage,
    what: &str,
    english: &str,
    text: &str,
) -> Option<String> {
    use dsa_host_core::ui_language::placeholders;
    let tag = language.tag();
    if text.trim().is_empty() {
        return Some(format!("{what} is empty in {tag}"));
    }
    if text.contains(char::from_u32(0x2014).unwrap()) {
        return Some(format!("{what} has an em dash in {tag}: {text}"));
    }
    if placeholders(text) != placeholders(english) {
        return Some(format!(
            "{what} in {tag} has other placeholders than in English: {text}"
        ));
    }
    None
}

/// Panics with what is wrong with `text`, the `language` translation of `english`.
pub fn assert_translation(language: UiLanguage, what: &str, english: &str, text: &str) {
    if let Some(problem) = translation_problem(language, what, english, text) {
        panic!("{problem}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr17_windows_language_ids_pick_their_translation() {
        let cases = [
            (0x0409, UiLanguage::English),
            (0x0809, UiLanguage::English),
            (0x0412, UiLanguage::Korean),
            (0x0411, UiLanguage::Japanese),
            (0x0804, UiLanguage::ChineseSimplified),
            (0x1004, UiLanguage::ChineseSimplified),
            (0x0004, UiLanguage::ChineseSimplified),
            (0x0404, UiLanguage::ChineseTraditional),
            (0x0C04, UiLanguage::ChineseTraditional),
            (0x1404, UiLanguage::ChineseTraditional),
            (0x7C04, UiLanguage::ChineseTraditional),
            (0x0416, UiLanguage::PortugueseBrazil),
            (0x0816, UiLanguage::PortugueseBrazil),
            (0x0C0A, UiLanguage::Spanish),
            (0x080A, UiLanguage::Spanish),
            (0x040C, UiLanguage::French),
            (0x0407, UiLanguage::German),
            (0x0419, UiLanguage::Russian),
            (0x0410, UiLanguage::Italian),
            (0x042A, UiLanguage::Vietnamese),
            (0x0421, UiLanguage::Indonesian),
            (0x041F, UiLanguage::Turkish),
        ];
        for (langid, language) in cases {
            assert_eq!(from_langid(langid), language, "{langid:#06x}");
        }
    }

    #[test]
    fn fr17_a_windows_language_dsa_has_no_translation_for_is_english() {
        // Dutch, Polish, Swedish and Arabic.
        for langid in [0x0413, 0x0415, 0x041D, 0x0401] {
            assert_eq!(from_langid(langid), UiLanguage::English, "{langid:#06x}");
        }
    }

    #[test]
    fn fr17_the_first_preferred_display_language_dsa_has_wins() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        fn unused() -> UiLanguage {
            panic!("the language ID is read only when Windows gives no names")
        }
        assert_eq!(
            from_preferred(&names(&["nl-NL", "de-DE"]), unused),
            UiLanguage::German
        );
        assert_eq!(
            from_preferred(&names(&["zh-Hant-HK"]), unused),
            UiLanguage::ChineseTraditional
        );
        assert_eq!(
            from_preferred(&names(&["zh-SG"]), unused),
            UiLanguage::ChineseSimplified
        );
        assert_eq!(
            from_preferred(&names(&["pt-PT"]), unused),
            UiLanguage::PortugueseBrazil
        );
        assert_eq!(
            from_preferred(&names(&["nl-NL", "pl-PL"]), unused),
            UiLanguage::English
        );
        assert_eq!(
            from_preferred(&[], || from_langid(0x0412)),
            UiLanguage::Korean
        );
    }
}
